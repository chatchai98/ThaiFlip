use crate::{
    input::{CaptureMode, Shortcut},
    preferences::{Preferences, Runtime},
    startup::{Registration, Value},
};
use std::{
    cell::RefCell,
    sync::{
        atomic::{AtomicIsize, Ordering},
        Arc, Mutex,
    },
};
use windows::{
    core::{w, PCWSTR},
    Win32::{
        Foundation::*,
        Graphics::Gdi::*,
        System::LibraryLoader::GetModuleHandleW,
        UI::{Controls::BST_CHECKED, HiDpi::*, WindowsAndMessaging::*},
    },
};

pub const SETTINGS_CHANGED: u32 = WM_APP + 10;
const CAPTURED: u32 = WM_APP + 11;
const MODE: usize = 1001;
const RECORD: usize = 1002;
const HINT_DURATION: usize = 1003;
const SAVE: usize = 1;
const CANCEL: usize = 2;
const MODES: [CaptureMode; 3] = [CaptureMode::Double, CaptureMode::Chord, CaptureMode::Single];
static WINDOW: AtomicIsize = AtomicIsize::new(0);
thread_local! { static PANEL: RefCell<Option<Panel>> = const { RefCell::new(None) }; }

struct Panel {
    owner: HWND,
    hwnd: HWND,
    auto: HWND,
    startup: HWND,
    hint_duration: HWND,
    registration: Registration,
    startup_snapshot: Option<Value>,
    startup_available: bool,
    mode: HWND,
    shortcut: HWND,
    status: HWND,
    font: HFONT,
    draft: Shortcut,
    shared: Arc<Mutex<Runtime>>,
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}
unsafe fn text(hwnd: HWND, value: &str) {
    let value = wide(value);
    let _ = SetWindowTextW(hwnd, PCWSTR(value.as_ptr()));
}
unsafe fn send(hwnd: HWND, msg: u32, wp: usize, lp: isize) -> LRESULT {
    SendMessageW(hwnd, msg, Some(WPARAM(wp)), Some(LPARAM(lp)))
}

pub unsafe fn notify_capture() {
    let hwnd = WINDOW.load(Ordering::Relaxed);
    if hwnd != 0 {
        let _ = PostMessageW(Some(HWND(hwnd as *mut _)), CAPTURED, WPARAM(0), LPARAM(0));
    }
}
pub unsafe fn dialog_message(msg: &MSG) -> bool {
    let hwnd = WINDOW.load(Ordering::Relaxed);
    hwnd != 0 && IsDialogMessageW(HWND(hwnd as *mut _), msg).as_bool()
}

pub unsafe fn open(owner: HWND, shared: Arc<Mutex<Runtime>>) -> windows::core::Result<()> {
    let existing = WINDOW.load(Ordering::Relaxed);
    if existing != 0 {
        let _ = SetForegroundWindow(HWND(existing as *mut _));
        return Ok(());
    }
    let registration = Registration::current().map_err(|e| {
        windows::core::Error::new(E_FAIL, format!("Cannot read Windows Startup: {e}"))
    })?;
    open_with_startup(owner, shared, registration)
}

unsafe fn open_with_startup(
    owner: HWND,
    shared: Arc<Mutex<Runtime>>,
    registration: Registration,
) -> windows::core::Result<()> {
    let (startup_snapshot, startup_error) = match registration.read() {
        Ok(value) => (value, None),
        Err(error) => (None, Some(error.to_string())),
    };
    let startup_available = startup_error.is_none();
    let preferences = shared.lock().unwrap().preferences.clone();
    let instance: HINSTANCE = GetModuleHandleW(None)?.into();
    let class = WNDCLASSW {
        lpfnWndProc: Some(proc),
        hInstance: instance,
        lpszClassName: w!("ThaiFlipSettings"),
        hbrBackground: HBRUSH((COLOR_WINDOW.0 + 1) as *mut _),
        hCursor: LoadCursorW(None, IDC_ARROW)?,
        hIcon: LoadIconW(Some(instance), PCWSTR(1usize as *const u16))?,
        ..Default::default()
    };
    RegisterClassW(&class);
    let style = WS_CAPTION | WS_SYSMENU | WS_OVERLAPPED;
    let hwnd = CreateWindowExW(
        WS_EX_CONTROLPARENT,
        class.lpszClassName,
        w!("ThaiFlip settings"),
        style,
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        490,
        330,
        Some(owner),
        None,
        Some(instance),
        None,
    )?;
    let result = (|| -> windows::core::Result<Panel> {
        let dpi = GetDpiForWindow(hwnd).max(96);
        let scale = |n: i32| (n as i64 * dpi as i64 / 96) as i32;
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: scale(460),
            bottom: scale(352),
        };
        AdjustWindowRectExForDpi(&mut rect, style, false, WS_EX_CONTROLPARENT, dpi)?;
        SetWindowPos(
            hwnd,
            None,
            0,
            0,
            rect.right - rect.left,
            rect.bottom - rect.top,
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
        )?;
        let font = CreateFontW(
            -scale(14),
            0,
            0,
            0,
            400,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            DEFAULT_PITCH.0 as u32,
            w!("Segoe UI"),
        );
        let child = |class: PCWSTR,
                     label: PCWSTR,
                     style: WINDOW_STYLE,
                     id: usize,
                     x,
                     y,
                     width,
                     height|
         -> windows::core::Result<HWND> {
            let control = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class,
                label,
                WS_CHILD | WS_VISIBLE | style,
                scale(x),
                scale(y),
                scale(width),
                scale(height),
                Some(hwnd),
                Some(HMENU(id as *mut _)),
                Some(instance),
                None,
            )?;
            send(control, WM_SETFONT, font.0 as usize, 1);
            Ok(control)
        };
        let auto = child(
            w!("BUTTON"),
            w!("&Auto-correct after Space (offline)"),
            WS_TABSTOP | WINDOW_STYLE(BS_AUTOCHECKBOX as u32),
            10,
            18,
            18,
            420,
            28,
        )?;
        send(
            auto,
            BM_SETCHECK,
            if preferences.auto {
                BST_CHECKED.0 as usize
            } else {
                0
            },
            0,
        );
        let startup = child(
            w!("BUTTON"),
            w!("Start ThaiFlip with &Windows (current user)"),
            WS_TABSTOP | WINDOW_STYLE(BS_AUTOCHECKBOX as u32),
            15,
            18,
            54,
            420,
            28,
        )?;
        send(
            startup,
            BM_SETCHECK,
            if registration.is_enabled(&startup_snapshot) {
                BST_CHECKED.0 as usize
            } else {
                0
            },
            0,
        );
        if !startup_available {
            let _ = windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow(startup, false);
        }
        child(
            w!("STATIC"),
            w!("Shortcut &type"),
            WINDOW_STYLE(0),
            11,
            18,
            97,
            140,
            22,
        )?;
        let mode = child(
            w!("COMBOBOX"),
            w!(""),
            WS_TABSTOP | WS_VSCROLL | WINDOW_STYLE(CBS_DROPDOWNLIST as u32),
            MODE,
            160,
            93,
            280,
            180,
        )?;
        for label in [
            w!("Double tap a modifier"),
            w!("Key combination"),
            w!("Single tap a modifier"),
        ] {
            send(mode, CB_ADDSTRING, 0, label.0 as isize);
        }
        send(
            mode,
            CB_SETCURSEL,
            MODES
                .iter()
                .position(|mode| *mode == preferences.shortcut.capture_mode())
                .unwrap(),
            0,
        );
        child(
            w!("STATIC"),
            w!("&Shortcut"),
            WINDOW_STYLE(0),
            12,
            18,
            139,
            140,
            22,
        )?;
        let shortcut = child(
            w!("EDIT"),
            w!(""),
            WS_BORDER | WS_TABSTOP | WINDOW_STYLE(ES_READONLY as u32 | ES_AUTOHSCROLL as u32),
            13,
            160,
            135,
            160,
            27,
        )?;
        text(shortcut, &preferences.shortcut.label());
        child(
            w!("BUTTON"),
            w!("&Record..."),
            WS_TABSTOP,
            RECORD,
            330,
            134,
            110,
            28,
        )?;
        child(
            w!("STATIC"),
            w!("Hint &duration"),
            WINDOW_STYLE(0),
            16,
            18,
            185,
            140,
            22,
        )?;
        let hint_duration = child(
            w!("COMBOBOX"),
            w!(""),
            WS_TABSTOP | WS_VSCROLL | WINDOW_STYLE(CBS_DROPDOWNLIST as u32),
            HINT_DURATION,
            160,
            181,
            280,
            180,
        )?;
        for seconds in Preferences::HINT_DURATIONS {
            let label = wide(&format!("{seconds} seconds"));
            send(hint_duration, CB_ADDSTRING, 0, label.as_ptr() as isize);
        }
        send(
            hint_duration,
            CB_SETCURSEL,
            Preferences::HINT_DURATIONS
                .iter()
                .position(|s| *s == preferences.hint_seconds)
                .unwrap_or(2),
            0,
        );
        let status=child(w!("STATIC"),w!("Single/double tap: left Alt or left/right Ctrl/Shift.\nCombination: Ctrl/Alt + a key. Some apps may use the same shortcut."),WINDOW_STYLE(0),14,18,217,420,65)?;
        if let Some(error) = &startup_error {
            text(
                status,
                &format!("Windows Startup is unavailable. Other settings still work.\n{error}"),
            );
        }
        child(
            w!("BUTTON"),
            w!("&Save"),
            WS_TABSTOP | WINDOW_STYLE(BS_DEFPUSHBUTTON as u32),
            SAVE,
            235,
            306,
            100,
            30,
        )?;
        child(
            w!("BUTTON"),
            w!("Cancel"),
            WS_TABSTOP,
            CANCEL,
            345,
            306,
            95,
            30,
        )?;
        Ok(Panel {
            owner,
            hwnd,
            auto,
            startup,
            hint_duration,
            registration,
            startup_snapshot,
            startup_available,
            mode,
            shortcut,
            status,
            font,
            draft: preferences.shortcut,
            shared: shared.clone(),
        })
    })();
    match result {
        Ok(panel) => {
            PANEL.with_borrow_mut(|p| *p = Some(panel));
            WINDOW.store(hwnd.0 as isize, Ordering::Relaxed);
            #[cfg(not(test))]
            {
                let _ = ShowWindow(hwnd, SW_SHOW);
                let _ = SetForegroundWindow(hwnd);
                let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(hwnd));
            }
            Ok(())
        }
        Err(e) => {
            let _ = DestroyWindow(hwnd);
            Err(e)
        }
    }
}

fn save_changes(
    previous: &Preferences,
    next: &Preferences,
    registration: &Registration,
    snapshot: &Option<Value>,
    enabled: bool,
) -> Result<(), String> {
    let change = if enabled != registration.is_enabled(snapshot) {
        registration.set_enabled(enabled, snapshot).map_err(|e| {
            format!("Windows Startup was not saved. Other settings were not changed.\n{e}")
        })?
    } else {
        None
    };
    let startup_changed = change.is_some();
    if previous.auto != next.auto
        || previous.shortcut != next.shortcut
        || previous.hint_seconds != next.hint_seconds
    {
        if let Err(error) = next.save() {
            if let Some(change) = change {
                if let Err(rollback) = registration.rollback(change) {
                    return Err(format!("Settings were not saved, and Windows Startup could not be restored. Reopen Settings to check Startup.\nSave: {error}\nRestore: {rollback}"));
                }
            }
            return Err(format!(
                "Settings were not saved. {}\n{error}",
                if startup_changed {
                    "Windows Startup was restored."
                } else {
                    ""
                }
            ));
        }
    }
    Ok(())
}

pub unsafe fn close() {
    WINDOW.store(0, Ordering::Relaxed);
    let panel = PANEL.with_borrow_mut(Option::take);
    if let Some(panel) = panel {
        if let Ok(mut runtime) = panel.shared.lock() {
            runtime.keys.cancel_capture();
        }
        let _ = DestroyWindow(panel.hwnd);
        let _ = DeleteObject(panel.font.into());
    }
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_CLOSE {
        close();
        return LRESULT(0);
    }
    if msg == WM_ACTIVATE && wp.0 & 0xffff == WA_INACTIVE as usize {
        PANEL.with_borrow(|p| {
            if let Some(p) = p {
                if let Ok(mut r) = p.shared.lock() {
                    r.keys.cancel_capture();
                }
            }
        });
    }
    if msg == CAPTURED {
        PANEL.with_borrow_mut(|panel| {
            if let Some(panel) = panel {
                let result = panel
                    .shared
                    .lock()
                    .ok()
                    .and_then(|mut r| r.keys.take_capture());
                match result {
                    Some(Ok(shortcut)) => {
                        panel.draft = shortcut;
                        text(panel.shortcut, &shortcut.label());
                        text(
                            panel.status,
                            "Recorded. Release the keys, then click Save to apply.",
                        );
                    }
                    Some(Err(reason)) => text(panel.status, reason),
                    None => {}
                }
            }
        });
        return LRESULT(0);
    }
    if msg == WM_COMMAND {
        let command = wp.0 & 0xffff;
        if command == MODE && (wp.0 >> 16) == CBN_SELCHANGE as usize {
            PANEL.with_borrow_mut(|panel| {
                if let Some(panel) = panel {
                    if let Ok(mut runtime) = panel.shared.lock() {
                        runtime.keys.cancel_capture();
                    }
                    let selected = MODES.get(send(panel.mode, CB_GETCURSEL, 0, 0).0 as usize);
                    panel.draft = match (selected, panel.draft) {
                        (
                            Some(CaptureMode::Single),
                            Shortcut::Single(key) | Shortcut::Double(key),
                        ) => Shortcut::Single(key),
                        (
                            Some(CaptureMode::Double),
                            Shortcut::Single(key) | Shortcut::Double(key),
                        ) => Shortcut::Double(key),
                        _ => panel.draft,
                    };
                    text(panel.shortcut, &panel.draft.label());
                    text(
                        panel.status,
                        if selected == Some(&panel.draft.capture_mode()) {
                            "Click Save to apply the selected tap mode."
                        } else {
                            "Click Record to choose a shortcut of the selected type."
                        },
                    );
                }
            });
            return LRESULT(0);
        }
        if !matches!(command, RECORD | SAVE | CANCEL) {
            return DefWindowProcW(hwnd, msg, wp, lp);
        }
        if command == CANCEL {
            close();
            return LRESULT(0);
        }
        let mut saved_owner = None;
        let mut save_error = None;
        PANEL.with_borrow_mut(|panel| {
            let Some(panel) = panel else {
                return;
            };
            if command == RECORD {
                let Some(&mode) = MODES.get(send(panel.mode, CB_GETCURSEL, 0, 0).0 as usize) else {
                    return;
                };
                if let Ok(mut runtime) = panel.shared.lock() {
                    runtime.keys.begin_capture(mode);
                }
                text(
                    panel.status,
                    if mode != CaptureMode::Chord {
                        "Tap the desired Alt/Ctrl/Shift key once. Escape cancels."
                    } else {
                        "Press the desired combination (for example Ctrl+Space). Escape cancels."
                    },
                );
            } else if command == SAVE {
                let selected = MODES.get(send(panel.mode, CB_GETCURSEL, 0, 0).0 as usize);
                if selected != Some(&panel.draft.capture_mode()) {
                    text(
                        panel.status,
                        "Record a shortcut of the selected type before saving.",
                    );
                    return;
                }
                let Ok(runtime) = panel.shared.lock() else {
                    return;
                };
                let previous = runtime.preferences.clone();
                let mut preferences = previous.clone();
                drop(runtime);
                preferences.auto = send(panel.auto, BM_GETCHECK, 0, 0).0 == BST_CHECKED.0 as isize;
                preferences.shortcut = panel.draft;
                let selected = send(panel.hint_duration, CB_GETCURSEL, 0, 0).0 as usize;
                let Some(seconds) = Preferences::HINT_DURATIONS.get(selected) else {
                    text(panel.status, "Select a valid hint duration.");
                    return;
                };
                preferences.hint_seconds = *seconds;
                let enabled = panel.startup_available
                    && send(panel.startup, BM_GETCHECK, 0, 0).0 == BST_CHECKED.0 as isize;
                if let Err(error) = save_changes(
                    &previous,
                    &preferences,
                    &panel.registration,
                    &panel.startup_snapshot,
                    enabled,
                ) {
                    text(panel.status, "Save failed. Please check the error message.");
                    save_error = Some((panel.hwnd, error));
                    return;
                }
                if let Ok(mut runtime) = panel.shared.lock() {
                    runtime.keys.configure(preferences.shortcut);
                    runtime.preferences = preferences;
                    saved_owner = Some(panel.owner);
                }
            }
        });
        if let Some((owner, error)) = save_error {
            let error = wide(&error);
            MessageBoxW(
                Some(owner),
                PCWSTR(error.as_ptr()),
                w!("ThaiFlip settings"),
                MB_OK | MB_ICONERROR,
            );
        }
        if let Some(owner) = saved_owner {
            close();
            let _ = PostMessageW(Some(owner), SETTINGS_CHANGED, WPARAM(0), LPARAM(0));
        }
        return LRESULT(0);
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{input::CTRL, preferences::Preferences};
    static UI_TEST_LOCK: Mutex<()> = Mutex::new(());
    #[test]
    fn single_tap_selection_saves_and_reopens_without_rerecording() {
        let _guard = UI_TEST_LOCK.lock().unwrap();
        unsafe {
            let path = unique_path("single-tap-settings");
            let shared = Arc::new(Mutex::new(Runtime::new(
                Preferences::load(path.clone()).unwrap(),
            )));
            let registration = Registration::isolated().unwrap();
            open_with_startup(HWND::default(), shared.clone(), registration.clone()).unwrap();
            let (hwnd, mode) = PANEL.with_borrow(|p| {
                let p = p.as_ref().unwrap();
                (p.hwnd, p.mode)
            });
            send(mode, CB_SETCURSEL, 2, 0);
            send(hwnd, WM_COMMAND, MODE | ((CBN_SELCHANGE as usize) << 16), 0);
            send(hwnd, WM_COMMAND, SAVE, 0);
            assert_eq!(
                Preferences::load(path.clone()).unwrap().shortcut,
                Shortcut::Single(164)
            );
            assert_eq!(registration.read().unwrap(), None);
            open_with_startup(HWND::default(), shared, registration).unwrap();
            PANEL.with_borrow(|p| {
                let p = p.as_ref().unwrap();
                assert_eq!(send(p.mode, CB_GETCURSEL, 0, 0).0, 2);
                assert_eq!(p.draft, Shortcut::Single(164));
            });
            close();
            std::fs::remove_file(path).unwrap();
        }
    }
    #[test]
    fn settings_form_records_saves_and_reloads_without_changing_foreground() {
        let _guard = UI_TEST_LOCK.lock().unwrap();
        unsafe {
            let path = std::env::current_exe()
                .unwrap()
                .parent()
                .unwrap()
                .join(format!("thaiflip-settings-test-{}.ini", std::process::id()));
            assert!(!path.exists());
            let shared = Arc::new(Mutex::new(Runtime::new(
                Preferences::load(path.clone()).unwrap(),
            )));
            let foreground = GetForegroundWindow();
            open(HWND::default(), shared.clone()).unwrap();
            assert_eq!(GetForegroundWindow(), foreground);
            let (hwnd, auto, mode, shortcut, hint_duration) = PANEL.with_borrow(|p| {
                let p = p.as_ref().unwrap();
                (p.hwnd, p.auto, p.mode, p.shortcut, p.hint_duration)
            });
            send(mode, CB_SETCURSEL, 1, 0);
            {
                let mut runtime = shared.lock().unwrap();
                runtime.keys.begin_capture(CaptureMode::Chord);
                runtime.keys.event(162, true, 0, 1, true);
                runtime.keys.event(119, true, 1, 1, true);
                runtime.keys.event(119, false, 2, 1, true);
                runtime.keys.event(162, false, 3, 1, true);
            }
            send(hwnd, CAPTURED, 0, 0);
            let mut label = [0u16; 64];
            let count = GetWindowTextW(shortcut, &mut label);
            assert_eq!(
                String::from_utf16_lossy(&label[..count as usize]),
                "Ctrl+F8"
            );
            send(auto, BM_SETCHECK, BST_CHECKED.0 as usize, 0);
            send(hint_duration, CB_SETCURSEL, 6, 0);
            send(hwnd, WM_COMMAND, SAVE, 0);
            assert_eq!(WINDOW.load(Ordering::Relaxed), 0);
            let saved = Preferences::load(path.clone()).unwrap();
            assert!(saved.auto);
            assert_eq!(saved.hint_seconds, 30);
            assert_eq!(
                saved.shortcut,
                Shortcut::Chord {
                    modifiers: CTRL,
                    key: 119
                }
            );
            std::fs::remove_file(path).unwrap();
        }
    }

    #[test]
    fn startup_checkbox_saves_reloads_and_disables_from_real_registry_state() {
        let _guard = UI_TEST_LOCK.lock().unwrap();
        unsafe {
            let path = std::env::current_exe()
                .unwrap()
                .parent()
                .unwrap()
                .join(format!("thaiflip-startup-ui-{}.ini", std::process::id()));
            assert!(!path.exists());
            let shared = Arc::new(Mutex::new(Runtime::new(
                Preferences::load(path.clone()).unwrap(),
            )));
            let registration = Registration::isolated().unwrap();
            let foreground = GetForegroundWindow();
            open_with_startup(HWND::default(), shared.clone(), registration.clone()).unwrap();
            let (hwnd, startup) = PANEL.with_borrow(|p| {
                let p = p.as_ref().unwrap();
                (p.hwnd, p.startup)
            });
            assert_eq!(send(startup, BM_GETCHECK, 0, 0).0, 0);
            send(startup, BM_SETCHECK, BST_CHECKED.0 as usize, 0);
            send(hwnd, WM_COMMAND, SAVE, 0);
            assert_eq!(WINDOW.load(Ordering::Relaxed), 0);
            assert!(registration.is_enabled(&registration.read().unwrap()));
            assert!(!path.exists());
            open_with_startup(HWND::default(), shared, registration.clone()).unwrap();
            let (hwnd, startup) = PANEL.with_borrow(|p| {
                let p = p.as_ref().unwrap();
                (p.hwnd, p.startup)
            });
            assert_eq!(send(startup, BM_GETCHECK, 0, 0).0, BST_CHECKED.0 as isize);
            send(startup, BM_SETCHECK, 0, 0);
            send(hwnd, WM_COMMAND, SAVE, 0);
            assert_eq!(registration.read().unwrap(), None);
            assert_eq!(GetForegroundWindow(), foreground);
        }
    }

    #[test]
    fn unreadable_startup_does_not_block_other_settings() {
        let _guard = UI_TEST_LOCK.lock().unwrap();
        unsafe {
            let path = unique_path("unreadable-startup");
            let shared = Arc::new(Mutex::new(Runtime::new(
                Preferences::load(path.clone()).unwrap(),
            )));
            let registration = Registration::unreadable_for_test().unwrap();
            open_with_startup(HWND::default(), shared, registration.clone()).unwrap();
            let (hwnd, startup, auto) = PANEL.with_borrow(|p| {
                let p = p.as_ref().unwrap();
                (p.hwnd, p.startup, p.auto)
            });
            assert!(
                !windows::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled(startup).as_bool()
            );
            send(auto, BM_SETCHECK, BST_CHECKED.0 as usize, 0);
            send(hwnd, WM_COMMAND, SAVE, 0);
            assert_eq!(WINDOW.load(Ordering::Relaxed), 0);
            assert!(Preferences::load(path.clone()).unwrap().auto);
            assert!(registration.read().is_err());
            std::fs::remove_file(path).unwrap();
        }
    }

    fn unique_path(label: &str) -> std::path::PathBuf {
        std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .join(format!(
                "{label}-{}-{}.ini",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ))
    }

    #[test]
    fn failed_settings_save_rolls_startup_back_and_preserves_file() {
        let path = unique_path("startup-rollback");
        let previous = Preferences::load(path.clone()).unwrap();
        previous.save().unwrap();
        let original = std::fs::read(&path).unwrap();
        let temporary = path.with_extension(format!("txt.tmp-{}", std::process::id()));
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .unwrap();
        let registration = Registration::isolated().unwrap();
        let mut next = previous.clone();
        next.auto = true;
        assert!(save_changes(&previous, &next, &registration, &None, true).is_err());
        assert_eq!(registration.read().unwrap(), None);
        assert_eq!(std::fs::read(&path).unwrap(), original);
        std::fs::remove_file(temporary).unwrap();
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn startup_only_change_does_not_need_a_writable_settings_folder() {
        let path = unique_path("missing-settings-folder").join("settings.ini");
        let previous = Preferences::load(path.clone()).unwrap();
        let registration = Registration::isolated().unwrap();
        save_changes(&previous, &previous, &registration, &None, true).unwrap();
        assert!(registration.is_enabled(&registration.read().unwrap()));
        assert!(!path.exists());
    }

    #[test]
    fn changed_registry_snapshot_prevents_unrelated_preferences_from_being_saved() {
        let path = unique_path("startup-conflict");
        let previous = Preferences::load(path.clone()).unwrap();
        let mut next = previous.clone();
        next.auto = true;
        let registration = Registration::isolated().unwrap();
        registration.set_enabled(true, &None).unwrap();
        let external = registration.read().unwrap();
        assert!(save_changes(&previous, &next, &registration, &None, true).is_err());
        assert!(!path.exists());
        assert_eq!(registration.read().unwrap(), external);
    }
    #[test]
    fn duration_only_change_is_persisted_without_changing_startup() {
        let path = unique_path("hint-duration");
        let previous = Preferences::load(path.clone()).unwrap();
        let mut next = previous.clone();
        next.hint_seconds = 5;
        let registration = Registration::isolated().unwrap();
        save_changes(&previous, &next, &registration, &None, false).unwrap();
        assert_eq!(Preferences::load(path.clone()).unwrap().hint_seconds, 5);
        assert_eq!(registration.read().unwrap(), None);
        std::fs::remove_file(path).unwrap();
    }
}
