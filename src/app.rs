use crate::convert::{convert, Target};
use crate::{
    detector::Detector,
    exclusions::{process_name, Exclusions},
    overlay::{Hint, Overlay},
    preferences::{Preferences, Runtime},
    settings_window,
};
use std::{
    cell::RefCell,
    sync::{
        atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering},
        mpsc, Arc, Mutex, OnceLock,
    },
    thread,
    time::{Duration, Instant},
};
use windows::{
    core::{w, PCWSTR, PWSTR},
    Win32::{
        Foundation::*,
        System::{
            Com::*, LibraryLoader::GetModuleHandleW, SystemInformation::GetTickCount64,
            Threading::CreateMutexW,
        },
        UI::{
            Accessibility::*, Controls::Dialogs::*, HiDpi::*, Input::KeyboardAndMouse::*, Shell::*,
            WindowsAndMessaging::*,
        },
    },
};

const TRAY: u32 = WM_APP + 1;
const PAUSE: u32 = 1;
const EXIT: u32 = 2;

static PAUSED: AtomicBool = AtomicBool::new(false);
static AUTO_ENABLED: AtomicBool = AtomicBool::new(false);
static OPTIONS: OnceLock<Arc<Mutex<Runtime>>> = OnceLock::new();
#[derive(Clone, Copy)]
struct ActionRequest {
    foreground: isize,
    focus: isize,
    epoch: u64,
    automatic: bool,
}
static JOBS: OnceLock<mpsc::Sender<ActionRequest>> = OnceLock::new();
static HINTS: OnceLock<Mutex<mpsc::Receiver<Option<Hint>>>> = OnceLock::new();
static LAST_INPUT: AtomicU64 = AtomicU64::new(0);
static HINT_DURATION_MS: AtomicU64 = AtomicU64::new(3000);
static REVISION: AtomicU64 = AtomicU64::new(0);
static SHOW_HINTS: AtomicBool = AtomicBool::new(true);
static TRACK_MOUSE: AtomicBool = AtomicBool::new(false);
static EDIT_EPOCH: AtomicU64 = AtomicU64::new(0);
static ALLOWED_FOREGROUND: AtomicIsize = AtomicIsize::new(0);
static SETTINGS: OnceLock<Mutex<Exclusions>> = OnceLock::new();
const TOGGLE_HINTS: u32 = 3;
const ADD_EXCLUSION: u32 = 4;
const OPEN_SETTINGS: u32 = 5;
const ABOUT: u32 = 6;
const REMOVE_EXCLUSION: u32 = 100;
const OWN_INPUT: usize = 0x54415448;
thread_local! { static OVERLAY: RefCell<Option<Overlay>> = const { RefCell::new(None) }; }

unsafe fn input_focus(foreground: HWND) -> isize {
    if foreground.0.is_null() {
        return 0;
    }
    let mut info = GUITHREADINFO {
        cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    if GetGUIThreadInfo(GetWindowThreadProcessId(foreground, None), &mut info).is_ok() {
        info.hwndFocus.0 as isize
    } else {
        0
    }
}

unsafe fn process_allowed(foreground: HWND) -> bool {
    let Some(name) = process_name(foreground) else {
        return false;
    };
    SETTINGS
        .get()
        .and_then(|s| s.try_lock().ok())
        .is_some_and(|settings| !settings.contains(&name))
}

unsafe fn update_foreground_policy() {
    let foreground = GetForegroundWindow();
    ALLOWED_FOREGROUND.store(
        if process_allowed(foreground) {
            foreground.0 as isize
        } else {
            0
        },
        Ordering::Relaxed,
    );
}

unsafe fn hints_active() -> bool {
    !PAUSED.load(Ordering::Relaxed)
        && ALLOWED_FOREGROUND.load(Ordering::Relaxed) == GetForegroundWindow().0 as isize
        && SHOW_HINTS.load(Ordering::Relaxed)
        && LAST_INPUT.load(Ordering::Relaxed) != 0
        && GetTickCount64().saturating_sub(LAST_INPUT.load(Ordering::Relaxed))
            < HINT_DURATION_MS.load(Ordering::Relaxed)
}

unsafe fn update_overlay() {
    let mut newest = None;
    if let Some(receiver) = HINTS.get().and_then(|r| r.try_lock().ok()) {
        while let Ok(hint) = receiver.try_recv() {
            newest = Some(hint);
        }
    }
    let foreground = GetForegroundWindow();
    let focus = input_focus(foreground);
    let revision = REVISION.load(Ordering::Relaxed);
    let now = GetTickCount64();
    let active = hints_active();
    let language =
        match GetKeyboardLayout(GetWindowThreadProcessId(foreground, None)).0 as usize & 0xffff {
            0x041e => "TH",
            0x0409 => "EN",
            _ => "TH / EN",
        };
    OVERLAY.with_borrow_mut(|slot| {
        let Some(overlay) = slot.as_mut() else {
            return;
        };
        if !active || overlay.foreground != foreground.0 as isize || overlay.focus != focus {
            overlay.hide();
        }
        if let Some(hint) = newest {
            match hint {
                Some(hint)
                    if active
                        && hint.foreground == foreground.0 as isize
                        && hint.focus == focus
                        && hint.revision == revision =>
                {
                    overlay.show(&hint)
                }
                None => {
                    overlay.clear_warning(language, revision);
                    overlay.note_unavailable(now);
                }
                _ => {}
            }
        }
        if overlay.suspicious && overlay.revision != revision {
            overlay.clear_warning(language, revision);
        }
        overlay.expire_unavailable(now);
    });
}

unsafe extern "system" fn mouse_hook(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code >= 0
        && matches!(
            wp.0 as u32,
            WM_LBUTTONDOWN
                | WM_RBUTTONDOWN
                | WM_MBUTTONDOWN
                | WM_XBUTTONDOWN
                | WM_MOUSEWHEEL
                | WM_MOUSEHWHEEL
        )
    {
        LAST_INPUT.store(0, Ordering::Relaxed);
        REVISION.fetch_add(1, Ordering::Relaxed);
        EDIT_EPOCH.fetch_add(1, Ordering::Relaxed);
        if let Some(mut state) = OPTIONS.get().and_then(|s| s.try_lock().ok()) {
            state.keys.interrupt();
        }
    }
    CallNextHookEx(None, code, wp, lp)
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

unsafe fn message(text: &str) {
    let text = wide(text);
    MessageBoxW(
        None,
        windows::core::PCWSTR(text.as_ptr()),
        w!("ThaiFlip"),
        MB_OK | MB_ICONINFORMATION,
    );
}

unsafe extern "system" fn hook(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code < 0 {
        return CallNextHookEx(None, code, wp, lp);
    }
    let event = &*(lp.0 as *const KBDLLHOOKSTRUCT);
    if event.flags.contains(LLKHF_INJECTED) {
        if event.dwExtraInfo != OWN_INPUT {
            LAST_INPUT.store(0, Ordering::Relaxed);
            EDIT_EPOCH.fetch_add(1, Ordering::Relaxed);
            REVISION.fetch_add(1, Ordering::Relaxed);
            if let Some(mut state) = OPTIONS.get().and_then(|s| s.try_lock().ok()) {
                state.keys.interrupt();
            }
        }
        return CallNextHookEx(None, code, wp, lp);
    }
    let down = wp.0 == WM_KEYDOWN as usize || wp.0 == WM_SYSKEYDOWN as usize;
    let foreground = GetForegroundWindow();
    let focus = input_focus(foreground);
    let eligible = !PAUSED.load(Ordering::Relaxed)
        && focus != 0
        && !foreground.0.is_null()
        && ALLOWED_FOREGROUND.load(Ordering::Relaxed) == foreground.0 as isize;
    let Some(mut state) = OPTIONS.get().and_then(|s| s.try_lock().ok()) else {
        return CallNextHookEx(None, code, wp, lp);
    };
    let decision = state.keys.event(
        crate::input::normalize_modifier(
            event.vkCode as u16,
            event.scanCode,
            event.flags.contains(LLKHF_EXTENDED),
        ),
        down,
        GetTickCount64(),
        focus,
        eligible,
    );
    drop(state);
    if decision.invalidate {
        EDIT_EPOCH.fetch_add(1, Ordering::Relaxed);
    }
    if down {
        REVISION.fetch_add(1, Ordering::Relaxed);
        LAST_INPUT.store(
            if decision.typing && !decision.consume {
                GetTickCount64()
            } else {
                0
            },
            Ordering::Relaxed,
        );
    }
    if decision.recorded {
        settings_window::notify_capture();
    }
    if decision.cancel_alt_menu {
        // A neutral key while Alt is held prevents a bare Alt tap from focusing the menu bar.
        send(&[
            key(VK_F24, KEYBD_EVENT_FLAGS(0), 0),
            key(VK_F24, KEYEVENTF_KEYUP, 0),
        ]);
    }
    if decision.trigger
        || (decision.completed_space
            && AUTO_ENABLED.load(Ordering::Relaxed)
            && TRACK_MOUSE.load(Ordering::Relaxed))
    {
        LAST_INPUT.store(0, Ordering::Relaxed);
        let request = ActionRequest {
            foreground: foreground.0 as isize,
            focus,
            epoch: EDIT_EPOCH.load(Ordering::Relaxed),
            automatic: !decision.trigger,
        };
        let _ = JOBS.get().is_some_and(|tx| tx.send(request).is_ok());
    }
    if decision.consume {
        return LRESULT(1);
    }
    CallNextHookEx(None, code, wp, lp)
}

fn key(vk: VIRTUAL_KEY, flags: KEYBD_EVENT_FLAGS, scan: u16) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: scan,
                dwFlags: flags,
                dwExtraInfo: OWN_INPUT,
                ..Default::default()
            },
        },
    }
}

unsafe fn send(inputs: &[INPUT]) -> bool {
    SendInput(inputs, std::mem::size_of::<INPUT>() as i32) == inputs.len() as u32
}

unsafe fn target_layout(target: Target) -> Option<HKL> {
    let id = match target {
        Target::Thai => 0x041e041eu32,
        Target::English => 0x04090409u32,
    };
    let count = GetKeyboardLayoutList(None);
    if count <= 0 {
        return None;
    }
    let mut layouts = vec![HKL::default(); count as usize];
    let count = GetKeyboardLayoutList(Some(&mut layouts));
    layouts[..count.max(0) as usize]
        .iter()
        .copied()
        .find(|k| k.0 as u32 == id)
}

unsafe fn emit_replacement(
    foreground: HWND,
    word: &crate::caret::Word,
    replacement: &str,
    layout: HKL,
) -> bool {
    let delivered = match word.replace_native(replacement) {
        Some(delivered) => delivered,
        None => {
            let inputs: Vec<_> = replacement
                .encode_utf16()
                .flat_map(|unit| {
                    [
                        key(VIRTUAL_KEY(0), KEYEVENTF_UNICODE, unit),
                        key(VIRTUAL_KEY(0), KEYEVENTF_UNICODE | KEYEVENTF_KEYUP, unit),
                    ]
                })
                .collect();
            send(&inputs)
        }
    };
    if !delivered {
        message(
            "Windows blocked all or part of the replacement. Check the text before continuing.",
        );
        return false;
    }
    if PostMessageW(
        Some(foreground),
        WM_INPUTLANGCHANGEREQUEST,
        WPARAM(0),
        LPARAM(layout.0 as isize),
    )
    .is_err()
    {
        message("Text converted, but the application did not accept the keyboard layout request.");
    }
    true
}

struct UndoStamp {
    epoch: u64,
    deadline: Instant,
}
impl UndoStamp {
    fn valid(&self, epoch: u64, now: Instant) -> bool {
        self.epoch == epoch && now < self.deadline
    }
}

struct Undo {
    before: String,
    layout: HKL,
    foreground: isize,
    after: crate::caret::Word,
    stamp: UndoStamp,
}

// Both correction and undo validate the same field, caret, text and input generation.
unsafe fn handle_action(
    automation: &IUIAutomation,
    detector: &Detector,
    request: ActionRequest,
    undo: &mut Option<Undo>,
) -> windows::core::Result<bool> {
    let foreground = HWND(request.foreground as *mut _);
    if EDIT_EPOCH.load(Ordering::Relaxed) != request.epoch
        || GetForegroundWindow() != foreground
        || input_focus(foreground) != request.focus
    {
        *undo = None;
        return Ok(false);
    }
    if PAUSED.load(Ordering::Relaxed) || !process_allowed(foreground) {
        *undo = None;
        return Ok(false);
    }
    if request.automatic
        && (!AUTO_ENABLED.load(Ordering::Relaxed) || !TRACK_MOUSE.load(Ordering::Relaxed))
    {
        return Ok(false);
    }
    let release_deadline = Instant::now() + Duration::from_millis(800);
    while [VK_SHIFT, VK_CONTROL, VK_MENU, VK_LWIN, VK_RWIN]
        .iter()
        .any(|k| GetAsyncKeyState(k.0 as i32) < 0)
    {
        if Instant::now() >= release_deadline
            || EDIT_EPOCH.load(Ordering::Relaxed) != request.epoch
            || GetForegroundWindow() != foreground
        {
            return Ok(false);
        }
        thread::sleep(Duration::from_millis(10));
    }
    if let Some(previous) = undo.take() {
        if previous.stamp.valid(request.epoch, Instant::now())
            && !request.automatic
            && previous.foreground == request.foreground
            && TRACK_MOUSE.load(Ordering::Relaxed)
        {
            if !previous.after.is_current(automation, foreground)?
                || !process_allowed(foreground)
                || EDIT_EPOCH.load(Ordering::Relaxed) != request.epoch
            {
                return Ok(false);
            }
            if previous.after.select_if_unchanged(automation, foreground)? {
                if EDIT_EPOCH.load(Ordering::Relaxed) != request.epoch
                    || GetForegroundWindow() != foreground
                    || input_focus(foreground) != request.focus
                {
                    return Ok(false);
                }
                emit_replacement(
                    foreground,
                    &previous.after,
                    &previous.before,
                    previous.layout,
                );
                return Ok(true);
            }
            return Ok(false);
        }
    }
    let mut mode = if request.automatic {
        crate::caret::ReadMode::Completed
    } else {
        crate::caret::ReadMode::Current
    };
    let deadline = Instant::now() + Duration::from_millis(150);
    let word = loop {
        if EDIT_EPOCH.load(Ordering::Relaxed) != request.epoch
            || GetForegroundWindow() != foreground
        {
            return Ok(false);
        }
        if let Some(word) = crate::caret::read_word_mode(automation, foreground, mode)? {
            break word;
        }
        if !request.automatic {
            if let Some(word) = crate::caret::read_word_mode(
                automation,
                foreground,
                crate::caret::ReadMode::Completed,
            )? {
                mode = crate::caret::ReadMode::Completed;
                break word;
            }
        }
        if !request.automatic || Instant::now() >= deadline {
            return Ok(false);
        }
        thread::sleep(Duration::from_millis(10));
    };
    let Some((replacement, target)) = detector.replacement(&word.text, request.automatic) else {
        return Ok(false);
    };
    let Some(layout) = target_layout(target) else {
        return Ok(false);
    };
    let original_layout = GetKeyboardLayout(GetWindowThreadProcessId(foreground, None));
    if !process_allowed(foreground)
        || EDIT_EPOCH.load(Ordering::Relaxed) != request.epoch
        || input_focus(foreground) != request.focus
        || !word.select_if_unchanged(automation, foreground)?
    {
        return Ok(false);
    }
    if EDIT_EPOCH.load(Ordering::Relaxed) != request.epoch
        || GetForegroundWindow() != foreground
        || input_focus(foreground) != request.focus
    {
        return Ok(false);
    }
    if !emit_replacement(foreground, &word, &replacement, layout) {
        return Ok(true);
    }
    if !TRACK_MOUSE.load(Ordering::Relaxed) {
        return Ok(true);
    }
    let deadline = Instant::now() + Duration::from_millis(250);
    while Instant::now() < deadline
        && EDIT_EPOCH.load(Ordering::Relaxed) == request.epoch
        && GetForegroundWindow() == foreground
        && process_allowed(foreground)
    {
        if let Some(after) = crate::caret::read_word_mode(automation, foreground, mode)
            .ok()
            .flatten()
        {
            if word
                .matches_replacement(automation, &after, &replacement)
                .unwrap_or(false)
                && after.is_current(automation, foreground).unwrap_or(false)
            {
                *undo = Some(Undo {
                    before: word.text,
                    layout: original_layout,
                    foreground: request.foreground,
                    after,
                    stamp: UndoStamp {
                        epoch: request.epoch,
                        deadline: Instant::now() + Duration::from_secs(10),
                    },
                });
                break;
            }
        }
        thread::sleep(Duration::from_millis(10));
    }
    Ok(true)
}

unsafe fn inspect_word(automation: &IUIAutomation, detector: &Detector) -> Option<Hint> {
    if !hints_active() {
        return None;
    }
    let foreground = GetForegroundWindow();
    let focus = input_focus(foreground);
    if focus == 0 {
        return None;
    }
    if !process_allowed(foreground) {
        return None;
    }
    let revision = REVISION.load(Ordering::Relaxed);
    let word = match crate::caret::read_word(automation, foreground).ok()? {
        Some(word) => word,
        None => {
            crate::caret::read_word_mode(automation, foreground, crate::caret::ReadMode::Completed)
                .ok()??
        }
    };
    let (replacement, target) = convert(&word.text)?;
    target_layout(target)?;
    let point = word.position(foreground)?;
    let suspicious = detector.suspicious(&word.text, &replacement, target);
    let current = GetKeyboardLayout(GetWindowThreadProcessId(foreground, None)).0 as usize & 0xffff;
    let language = match current {
        0x041e => "TH",
        0x0409 => "EN",
        _ => return None,
    };
    let label = if suspicious {
        let short: String = replacement.trim().chars().take(32).collect();
        let shortcut = OPTIONS
            .get()
            .and_then(|p| p.try_lock().ok())
            .map(|p| p.preferences.shortcut.label())?;
        format!(
            "{}?  {} · {}{}",
            if target == Target::Thai { "TH" } else { "EN" },
            shortcut,
            short,
            if replacement.trim().chars().count() > 32 {
                "…"
            } else {
                ""
            }
        )
    } else {
        language.to_owned()
    };
    if !process_allowed(foreground)
        || input_focus(foreground) != focus
        || !word.is_current(automation, foreground).ok()?
        || !hints_active()
        || GetForegroundWindow() != foreground
        || REVISION.load(Ordering::Relaxed) != revision
    {
        return None;
    }
    Some(Hint {
        foreground: foreground.0 as isize,
        focus,
        revision,
        point,
        label,
        suspicious,
    })
}

unsafe fn set_exclusion(name: &str, excluded: bool) {
    let Some(mut settings) = SETTINGS.get().and_then(|s| s.lock().ok()) else {
        message("Exclusion settings are unavailable.");
        return;
    };
    if let Err(error) = settings.set(name, excluded) {
        message(&format!(
            "Cannot save excluded-apps.txt. The list was not changed.\n{error}"
        ));
    }
    drop(settings);
    EDIT_EPOCH.fetch_add(1, Ordering::Relaxed);
    update_foreground_policy();
    update_overlay();
}

unsafe fn choose_exclusion(owner: HWND) {
    let mut filename = vec![0u16; 32768];
    let filter = wide("Applications (*.exe)\0*.exe\0");
    let mut dialog = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: owner,
        lpstrFile: PWSTR(filename.as_mut_ptr()),
        nMaxFile: filename.len() as u32,
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrTitle: w!("Exclude an application from ThaiFlip"),
        Flags: OFN_FILEMUSTEXIST
            | OFN_PATHMUSTEXIST
            | OFN_NOCHANGEDIR
            | OFN_DONTADDTORECENT
            | OFN_EXPLORER,
        ..Default::default()
    };
    if GetOpenFileNameW(&mut dialog).as_bool() {
        let end = filename
            .iter()
            .position(|c| *c == 0)
            .unwrap_or(filename.len());
        if let Ok(path) = String::from_utf16(&filename[..end]) {
            if let Some(name) = std::path::Path::new(&path)
                .file_name()
                .and_then(|n| n.to_str())
            {
                set_exclusion(name, true);
            }
        }
    } else if CommDlgExtendedError().0 != 0 {
        message("Cannot open the application picker.");
    }
}

unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == settings_window::SETTINGS_CHANGED {
        if let Some(runtime) = OPTIONS.get().and_then(|s| s.lock().ok()) {
            HINT_DURATION_MS.store(
                u64::from(runtime.preferences.hint_seconds) * 1000,
                Ordering::Relaxed,
            );
            AUTO_ENABLED.store(
                runtime.preferences.auto && TRACK_MOUSE.load(Ordering::Relaxed),
                Ordering::Relaxed,
            );
        }
        EDIT_EPOCH.fetch_add(1, Ordering::Relaxed);
        REVISION.fetch_add(1, Ordering::Relaxed);
        LAST_INPUT.store(0, Ordering::Relaxed);
        update_overlay();
        return LRESULT(0);
    }
    if msg == WM_TIMER {
        update_foreground_policy();
        update_overlay();
        return LRESULT(0);
    }
    if msg == TRAY && (lp.0 as u32 == WM_RBUTTONUP || lp.0 as u32 == WM_LBUTTONUP) {
        let previous_foreground = GetForegroundWindow();
        let exclusions: Vec<String> = SETTINGS
            .get()
            .and_then(|s| s.lock().ok())
            .map(|s| s.names().cloned().collect())
            .unwrap_or_default();
        if let Ok(menu) = CreatePopupMenu() {
            let label = if PAUSED.load(Ordering::Relaxed) {
                w!("Resume conversion")
            } else {
                w!("Pause conversion")
            };
            let _ = AppendMenuW(menu, MF_STRING, OPEN_SETTINGS as usize, w!("Settings..."));
            let _ = AppendMenuW(menu, MF_STRING, PAUSE as usize, label);
            let label = if SHOW_HINTS.load(Ordering::Relaxed) {
                w!("Hide caret hints")
            } else {
                w!("Show caret hints")
            };
            let _ = AppendMenuW(menu, MF_STRING, TOGGLE_HINTS as usize, label);
            if let Ok(submenu) = CreatePopupMenu() {
                let _ = AppendMenuW(
                    submenu,
                    MF_STRING,
                    ADD_EXCLUSION as usize,
                    w!("Add application..."),
                );
                for (index, name) in exclusions.iter().enumerate() {
                    let label = wide(&format!("Remove: {}", name.replace('&', "&&")));
                    let _ = AppendMenuW(
                        submenu,
                        MF_STRING,
                        REMOVE_EXCLUSION as usize + index,
                        PCWSTR(label.as_ptr()),
                    );
                }
                let _ = AppendMenuW(menu, MF_POPUP, submenu.0 as usize, w!("Excluded apps"));
            }
            let _ = AppendMenuW(menu, MF_STRING, ABOUT as usize, w!("About ThaiFlip"));
            let _ = AppendMenuW(menu, MF_STRING, EXIT as usize, w!("Exit ThaiFlip"));
            let mut point = POINT::default();
            let _ = GetCursorPos(&mut point);
            let _ = SetForegroundWindow(hwnd);
            let command = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_NONOTIFY,
                point.x,
                point.y,
                None,
                hwnd,
                None,
            );
            if command.0 as u32 == PAUSE {
                PAUSED.fetch_xor(true, Ordering::Relaxed);
                update_overlay();
            } else if command.0 as u32 == TOGGLE_HINTS {
                SHOW_HINTS.fetch_xor(true, Ordering::Relaxed);
                update_overlay();
            } else if command.0 as u32 == ADD_EXCLUSION {
                choose_exclusion(hwnd);
            } else if command.0 as u32 == OPEN_SETTINGS {
                if let Some(shared) = OPTIONS.get() {
                    if let Err(error) = settings_window::open(hwnd, shared.clone()) {
                        message(&format!("Cannot open settings: {error}"));
                    }
                }
            } else if command.0 as u32 == ABOUT {
                message(&format!("ThaiFlip v{}\n\nAuthor: {}\nLicense: {}\n\nOffline Thai / English keyboard layout correction.",
                    env!("CARGO_PKG_VERSION"),env!("CARGO_PKG_AUTHORS"),env!("CARGO_PKG_LICENSE")));
            } else if let Some(name) = (command.0 as u32)
                .checked_sub(REMOVE_EXCLUSION)
                .and_then(|i| exclusions.get(i as usize))
            {
                set_exclusion(name, false);
            } else if command.0 as u32 == EXIT {
                PostQuitMessage(0);
            }
            let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
            let _ = DestroyMenu(menu);
            if GetForegroundWindow() == hwnd && !previous_foreground.0.is_null() {
                let _ = SetForegroundWindow(previous_foreground);
            }
        }
        return LRESULT(0);
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}

pub fn run() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let Ok(mutex) = CreateMutexW(None, false, w!("Local\\TabThai-0.1")) else {
            message("Cannot create application lock.");
            return;
        };
        if GetLastError() == ERROR_ALREADY_EXISTS {
            message("ThaiFlip is already running. See the system tray.");
            let _ = CloseHandle(mutex);
            return;
        }
        let settings = std::env::current_exe()
            .and_then(|path| Exclusions::load(path.with_file_name("excluded-apps.txt")));
        match settings {
            Ok(settings) => {
                let _ = SETTINGS.set(Mutex::new(settings));
            }
            Err(error) => {
                message(&format!(
                    "Cannot read excluded-apps.txt. Fix it and restart ThaiFlip.\n{error}"
                ));
                let _ = CloseHandle(mutex);
                return;
            }
        }
        let preferences = std::env::current_exe()
            .and_then(|p| Preferences::load(p.with_file_name("settings.ini")));
        match preferences {
            Ok(preferences) => {
                AUTO_ENABLED.store(preferences.auto, Ordering::Relaxed);
                HINT_DURATION_MS.store(
                    u64::from(preferences.hint_seconds) * 1000,
                    Ordering::Relaxed,
                );
                let _ = OPTIONS.set(Arc::new(Mutex::new(Runtime::new(preferences))));
            }
            Err(error) => {
                message(&format!(
                    "Cannot read settings.ini. Fix it and restart.\n{error}"
                ));
                let _ = CloseHandle(mutex);
                return;
            }
        }
        let (tx, rx) = mpsc::channel::<ActionRequest>();
        let (hint_tx, hint_rx) = mpsc::sync_channel(1);
        let _ = HINTS.set(Mutex::new(hint_rx));
        let (ready_tx, ready_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            let initialized = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
            let automation: windows::core::Result<IUIAutomation> = if initialized {
                CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)
            } else {
                Err(windows::core::Error::from_hresult(E_FAIL))
            };
            let _ = ready_tx.send(automation.is_ok());
            if let Ok(automation) = automation {
                let detector = Detector::new();
                let mut undo = None;
                loop {
                    match rx.recv_timeout(Duration::from_millis(120)) {
                        Ok(request) => {
                            let foreground = HWND(request.foreground as *mut _);
                            if GetForegroundWindow() != foreground {
                                continue;
                            }
                            let _ = handle_action(&automation, &detector, request, &mut undo);
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            if undo.as_ref().is_some_and(|u| {
                                !u.stamp
                                    .valid(EDIT_EPOCH.load(Ordering::Relaxed), Instant::now())
                                    || u.foreground != GetForegroundWindow().0 as isize
                            }) {
                                undo = None;
                            }
                            let revision = REVISION.load(Ordering::Relaxed);
                            let hint = inspect_word(&automation, &detector);
                            // Superseded reads are not evidence that the active text field disappeared.
                            if REVISION.load(Ordering::Relaxed) == revision {
                                let _ = hint_tx.try_send(hint);
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
                drop(detector);
                drop(automation);
            }
            if initialized {
                CoUninitialize();
            }
        });
        if ready_rx.recv() != Ok(true) {
            message("Windows Accessibility is unavailable.");
            let _ = worker.join();
            let _ = CloseHandle(mutex);
            return;
        }
        let _ = JOBS.set(tx);
        let instance: HINSTANCE = GetModuleHandleW(None).unwrap_or_default().into();
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            hIcon: LoadIconW(Some(instance), windows::core::PCWSTR(1usize as *const u16))
                .unwrap_or_default(),
            lpszClassName: w!("ThaiFlipHiddenWindow"),
            ..Default::default()
        };
        RegisterClassW(&class);
        let Ok(hwnd) = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class.lpszClassName,
            w!("ThaiFlip"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance),
            None,
        ) else {
            message("Cannot create tray window.");
            return;
        };
        let mut tray = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: 1,
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
            uCallbackMessage: TRAY,
            hIcon: class.hIcon,
            ..Default::default()
        };
        let tip = wide("ThaiFlip: TH / EN. Click for settings, pause or exit.");
        tray.szTip[..tip.len()].copy_from_slice(&tip);
        if !Shell_NotifyIconW(NIM_ADD, &tray).as_bool() {
            message("Cannot add system tray icon.");
            return;
        }
        let overlay = Overlay::new(instance, hwnd, class.hIcon);
        if let Ok(overlay) = overlay {
            OVERLAY.with_borrow_mut(|slot| *slot = Some(overlay));
        } else {
            message("Caret hints could not be created. Shortcut conversion is still available.");
        }
        update_foreground_policy();
        if SetTimer(Some(hwnd), 1, 100, None) == 0 {
            message("Cannot start the foreground application timer.");
            let _ = Shell_NotifyIconW(NIM_DELETE, &tray);
            let _ = DestroyWindow(hwnd);
            let _ = CloseHandle(mutex);
            return;
        }
        let mouse_handle =
            SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), Some(instance.into()), 0);
        TRACK_MOUSE.store(mouse_handle.is_ok(), Ordering::Relaxed);
        if mouse_handle.is_err() {
            SHOW_HINTS.store(false, Ordering::Relaxed);
            AUTO_ENABLED.store(false, Ordering::Relaxed);
            message("Caret hints, Auto, and undo are disabled because mouse focus changes could not be tracked.");
        }
        let hook_handle = SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook), Some(instance.into()), 0);
        if let Ok(hook_handle) = hook_handle {
            let mut msg = MSG::default();
            loop {
                let status = GetMessageW(&mut msg, None, 0, 0).0;
                if status <= 0 {
                    break;
                }
                if !settings_window::dialog_message(&msg) {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            let _ = UnhookWindowsHookEx(hook_handle);
        } else {
            message("Cannot register keyboard hook.");
        }
        let _ = Shell_NotifyIconW(NIM_DELETE, &tray);
        if let Ok(mouse_handle) = mouse_handle {
            let _ = UnhookWindowsHookEx(mouse_handle);
        }
        let _ = KillTimer(Some(hwnd), 1);
        settings_window::close();
        OVERLAY.with_borrow_mut(|overlay| *overlay = None);
        let _ = DestroyWindow(hwnd);
        let _ = CloseHandle(mutex);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn undo_expires_and_rejects_intervening_input() {
        let now = Instant::now();
        let stamp = UndoStamp {
            epoch: 7,
            deadline: now + Duration::from_secs(10),
        };
        assert!(stamp.valid(7, now));
        assert!(!stamp.valid(8, now));
        assert!(!stamp.valid(7, now + Duration::from_secs(10)));
    }

    unsafe fn pump() {
        let mut msg = MSG::default();
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    #[test]
    fn native_selection_and_sensitive_field_guards() {
        unsafe {
            let edit = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("EDIT"),
                w!("prefix c]h;"),
                WINDOW_STYLE(ES_AUTOHSCROLL as u32),
                0,
                0,
                200,
                40,
                None,
                None,
                None,
                None,
            )
            .unwrap();
            SendMessageW(edit, 0x00B1, Some(WPARAM(11)), Some(LPARAM(11)));
            assert_eq!(
                crate::caret::native_snapshot(edit),
                Some(("prefix c]h;".encode_utf16().collect(), 11))
            );
            SendMessageW(edit, 0x00B1, Some(WPARAM(0)), Some(LPARAM(11)));
            assert_eq!(crate::caret::native_snapshot(edit), None);
            let _ = DestroyWindow(edit);
            for style in [ES_PASSWORD, ES_READONLY] {
                let edit = CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    w!("EDIT"),
                    w!("c]h;"),
                    WINDOW_STYLE(style as u32),
                    0,
                    0,
                    200,
                    40,
                    None,
                    None,
                    None,
                    None,
                )
                .unwrap();
                assert_eq!(crate::caret::native_word(edit), None);
                let _ = DestroyWindow(edit);
            }
        }
    }

    #[test]
    #[ignore = "requires interactive Windows desktop with US and Thai Kedmanee installed"]
    fn desktop_replacement_and_guards() {
        unsafe {
            TRACK_MOUSE.store(true, Ordering::Relaxed);
            let original = GetKeyboardLayout(0);
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                w!("ThaiFlip integration test"),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                50,
                50,
                500,
                200,
                None,
                None,
                None,
                None,
            )
            .unwrap();
            let deadline = Instant::now() + Duration::from_secs(45);
            while GetForegroundWindow() != hwnd && Instant::now() < deadline {
                pump();
                thread::sleep(Duration::from_millis(10));
            }
            if GetForegroundWindow() != hwnd {
                let _ = DestroyWindow(hwnd);
                panic!("Interactive desktop unavailable: the test window could not receive focus");
            }
            let _ = SETTINGS.set(Mutex::new(
                Exclusions::load(
                    std::env::temp_dir()
                        .join(format!("thaiflip-empty-test-{}.txt", std::process::id())),
                )
                .unwrap(),
            ));
            let (tx, rx) = mpsc::channel::<(isize, bool)>();
            let (done_tx, done_rx) = mpsc::channel();
            let worker = thread::spawn(move || {
                CoInitializeEx(None, COINIT_MULTITHREADED).ok().unwrap();
                let automation: IUIAutomation =
                    CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).unwrap();
                let detector = Detector::new();
                let mut undo = None;
                for (hwnd, reset) in rx {
                    if reset {
                        undo = None;
                    }
                    let request = ActionRequest {
                        foreground: hwnd,
                        focus: input_focus(HWND(hwnd as *mut _)),
                        epoch: EDIT_EPOCH.load(Ordering::Relaxed),
                        automatic: false,
                    };
                    let result = handle_action(&automation, &detector, request, &mut undo)
                        .map_err(|e| e.to_string());
                    done_tx.send(result).unwrap();
                }
                drop(automation);
                CoUninitialize();
            });
            let cases = [
                ("prefix c]h;", 0, false, "prefix แล้ว", true),
                ("แล้ว", 0, false, "c]h;", true),
                ("hello", 0, false, "้ำสสน", true),
                ("้ำสสน", 0, false, "hello", true),
                ("c]h;", ES_PASSWORD as u32, false, "c]h;", false),
                ("c]h;", ES_READONLY as u32, false, "c]h;", false),
                ("c]h;", 0, true, "c]h;", false),
                ("123", 0, false, "123", false),
                ("ไทยabc", 0, false, "ไทยabc", false),
                ("", 0, false, "", false),
            ];
            let mut failures = Vec::new();
            for (text, flags, select_all, expected, changed) in cases {
                let case_layout = GetKeyboardLayout(0);
                let text_w = wide(text);
                let edit = CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    w!("EDIT"),
                    windows::core::PCWSTR(text_w.as_ptr()),
                    WS_CHILD
                        | WS_VISIBLE
                        | WS_TABSTOP
                        | WINDOW_STYLE(ES_MULTILINE as u32 | ES_AUTOHSCROLL as u32 | flags),
                    10,
                    10,
                    450,
                    80,
                    Some(hwnd),
                    None,
                    None,
                    None,
                )
                .unwrap();
                let _ = SetForegroundWindow(hwnd);
                let _ = SetFocus(Some(edit));
                let end = text.encode_utf16().count();
                SendMessageW(
                    edit,
                    0x00B1, /* EM_SETSEL */
                    Some(WPARAM(if select_all { 0 } else { end })),
                    Some(LPARAM(end as isize)),
                );
                pump();
                tx.send((hwnd.0 as isize, true)).unwrap();
                let deadline = Instant::now() + Duration::from_secs(10);
                let result = loop {
                    pump();
                    if let Ok(result) = done_rx.try_recv() {
                        break result;
                    }
                    if Instant::now() > deadline {
                        panic!("Accessibility provider did not respond");
                    }
                    thread::sleep(Duration::from_millis(5));
                };
                // SendInput queues input; allow the test window to consume it before reading its text.
                let deadline = Instant::now() + Duration::from_millis(100);
                while Instant::now() < deadline {
                    pump();
                    thread::sleep(Duration::from_millis(2));
                }
                let mut buffer = vec![0u16; 512];
                let length = GetWindowTextW(edit, &mut buffer);
                let actual = String::from_utf16_lossy(&buffer[..length as usize]);
                if result != Ok(changed) || actual != expected {
                    failures.push(format!("{text:?}, style={flags}, selected={select_all}: {result:?}, actual={actual:?}, expected={expected:?}"));
                }
                if changed {
                    let expected_lang =
                        if text.chars().any(|c| ('\u{0e00}'..='\u{0e7f}').contains(&c)) {
                            0x0409
                        } else {
                            0x041e
                        };
                    if GetKeyboardLayout(0).0 as usize & 0xffff != expected_lang {
                        failures.push("Keyboard layout did not switch".into());
                    }
                    tx.send((hwnd.0 as isize, false)).unwrap();
                    let deadline = Instant::now() + Duration::from_secs(10);
                    loop {
                        pump();
                        if let Ok(result) = done_rx.try_recv() {
                            if result != Ok(true) {
                                failures.push(format!("Undo was not handled: {result:?}"));
                            }
                            break;
                        }
                        if Instant::now() > deadline {
                            panic!("Undo did not respond");
                        }
                        thread::sleep(Duration::from_millis(5));
                    }
                    let deadline = Instant::now() + Duration::from_millis(100);
                    while Instant::now() < deadline {
                        pump();
                        thread::sleep(Duration::from_millis(2));
                    }
                    let length = GetWindowTextW(edit, &mut buffer);
                    if String::from_utf16_lossy(&buffer[..length as usize]) != text {
                        failures.push("Undo did not restore the exact original text".into());
                    }
                    if GetKeyboardLayout(0) != case_layout {
                        failures.push("Undo did not restore the original keyboard layout".into());
                    }
                }
                let _ = DestroyWindow(edit);
            }
            drop(tx);
            worker.join().unwrap();
            let _ = ActivateKeyboardLayout(original, ACTIVATE_KEYBOARD_LAYOUT_FLAGS(0));
            let _ = DestroyWindow(hwnd);
            assert!(failures.is_empty(), "{}", failures.join("\n"));
        }
    }
}
