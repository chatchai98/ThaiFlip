use windows::{
    core::{w, PCWSTR},
    Win32::{
        Foundation::*,
        Graphics::Gdi::*,
        UI::{
            HiDpi::{GetDpiForMonitor, GetDpiForWindow, MDT_EFFECTIVE_DPI},
            WindowsAndMessaging::*,
        },
    },
};

pub struct Hint {
    pub foreground: isize,
    pub focus: isize,
    pub revision: u64,
    pub point: POINT,
    pub label: String,
    pub suspicious: bool,
}

pub struct Overlay {
    hwnd: HWND,
    pub foreground: isize,
    pub focus: isize,
    pub revision: u64,
    pub suspicious: bool,
    layout: Option<Layout>,
    label: String,
    missing_since: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Layout {
    caret: POINT,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    dpi: u32,
    work: RECT,
}

fn stable_layout(
    previous: Option<Layout>,
    caret: POINT,
    width: i32,
    height: i32,
    dpi: u32,
    work: RECT,
) -> Option<Layout> {
    let scale = |n: i32| (n as i64 * dpi as i64 / 96) as i32;
    let previous = previous.filter(|old| {
        old.dpi == dpi
            && old.work == work
            && (old.caret.x as i64 - caret.x as i64).abs() <= scale(128) as i64
            && (old.caret.y as i64 - caret.y as i64).abs() <= scale(6) as i64
    });
    // Keep one anchor and never shrink within the same typing line.
    let width = previous
        .map_or(width, |old| old.width.max(width))
        .min(work.right - work.left);
    let (anchor, x, y) = if let Some(old) = previous {
        if width <= 0 || height <= 0 || height > work.bottom - work.top {
            return None;
        }
        (
            old.caret,
            old.x.clamp(work.left, work.right - width),
            old.y.clamp(work.top, work.bottom - height),
        )
    } else {
        let (x, y) = placement(caret, width, height, work, scale(8))?;
        (caret, x, y)
    };
    Some(Layout {
        caret: anchor,
        x,
        y,
        width,
        height,
        dpi,
        work,
    })
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

unsafe fn font(dpi: u32) -> HFONT {
    CreateFontW(
        -((13 * dpi / 96) as i32),
        0,
        0,
        0,
        600,
        0,
        0,
        0,
        DEFAULT_CHARSET,
        OUT_DEFAULT_PRECIS,
        CLIP_DEFAULT_PRECIS,
        CLEARTYPE_QUALITY,
        DEFAULT_PITCH.0 as u32,
        w!("Segoe UI"),
    )
}

impl Overlay {
    pub unsafe fn new(
        instance: HINSTANCE,
        owner: HWND,
        icon: HICON,
    ) -> windows::core::Result<Self> {
        let class = WNDCLASSW {
            style: CS_DROPSHADOW,
            lpfnWndProc: Some(popup_proc),
            hInstance: instance,
            hIcon: icon,
            lpszClassName: w!("TabThaiCaretHint"),
            ..Default::default()
        };
        RegisterClassW(&class);
        let hwnd = CreateWindowExW(
            WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_TRANSPARENT | WS_EX_LAYERED,
            class.lpszClassName,
            w!("TabThai hint"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            Some(owner),
            None,
            Some(instance),
            None,
        )?;
        if let Err(error) = SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA) {
            let _ = DestroyWindow(hwnd);
            return Err(error);
        }
        Ok(Self {
            hwnd,
            foreground: 0,
            focus: 0,
            revision: 0,
            suspicious: false,
            layout: None,
            label: String::new(),
            missing_since: None,
        })
    }

    pub unsafe fn hide(&mut self) {
        if self.foreground == 0 && self.label.is_empty() {
            return;
        }
        let _ = ShowWindow(self.hwnd, SW_HIDE);
        // Remove the preview from the hidden window as well as from the screen.
        let _ = SetWindowTextW(self.hwnd, w!(""));
        self.foreground = 0;
        self.focus = 0;
        self.suspicious = false;
        self.layout = None;
        self.label.clear();
        self.missing_since = None;
    }

    unsafe fn set_content(&mut self, label: &str, suspicious: bool) -> bool {
        if self.label == label && self.suspicious == suspicious {
            return true;
        }
        let text = wide(label);
        if SetWindowTextW(self.hwnd, PCWSTR(text.as_ptr())).is_err() {
            return false;
        }
        SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, suspicious as isize);
        self.label = label.to_owned();
        self.suspicious = suspicious;
        let _ = InvalidateRect(Some(self.hwnd), None, false);
        true
    }

    pub unsafe fn clear_warning(&mut self, language: &str, revision: u64) {
        if self.foreground != 0 && self.suspicious {
            if !self.set_content(language, false) {
                self.hide();
                return;
            }
            self.revision = revision;
        }
    }

    pub fn note_unavailable(&mut self, now: u64) {
        if self.foreground != 0 {
            self.missing_since.get_or_insert(now);
        }
    }

    pub unsafe fn expire_unavailable(&mut self, now: u64) {
        if self
            .missing_since
            .is_some_and(|since| now.saturating_sub(since) >= 250)
        {
            self.hide();
        }
    }

    pub unsafe fn show(&mut self, hint: &Hint) {
        let monitor = MonitorFromPoint(hint.point, MONITOR_DEFAULTTONULL);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if monitor.0.is_null() || !GetMonitorInfoW(monitor, &mut info).as_bool() {
            self.hide();
            return;
        }
        // Query the target monitor without first moving the visible popup onto the caret.
        let mut dpi = 96;
        let mut dpi_y = 96;
        if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi, &mut dpi_y).is_err() {
            dpi = GetDpiForWindow(HWND(hint.foreground as *mut _)).max(96);
        }
        let dpi = dpi.max(96);
        let scale = |n: i32| (n as i64 * dpi as i64 / 96) as i32;
        let text = wide(&hint.label);
        let dc = GetDC(Some(self.hwnd));
        if dc.0.is_null() {
            self.hide();
            return;
        }
        let font = font(dpi);
        let old = SelectObject(dc, font.into());
        let mut size = SIZE::default();
        let _ = GetTextExtentPoint32W(dc, &text[..text.len() - 1], &mut size);
        SelectObject(dc, old);
        let _ = DeleteObject(font.into());
        ReleaseDC(Some(self.hwnd), dc);
        let width = (size.cx + scale(44)).min(scale(340));
        let height = scale(32);
        let previous = if self.foreground == hint.foreground && self.focus == hint.focus {
            self.layout
        } else {
            None
        };
        let Some(layout) = stable_layout(previous, hint.point, width, height, dpi, info.rcWork)
        else {
            self.hide();
            return;
        };
        let resized = previous.is_none_or(|old| {
            old.width != layout.width || old.height != layout.height || old.dpi != dpi
        });
        let moved = previous.is_none_or(|old| old.x != layout.x || old.y != layout.y);
        if !self.set_content(&hint.label, hint.suspicious) {
            self.hide();
            return;
        }
        if resized {
            let region = CreateRoundRectRgn(
                0,
                0,
                layout.width + 1,
                layout.height + 1,
                scale(12),
                scale(12),
            );
            if !region.0.is_null() && SetWindowRgn(self.hwnd, Some(region), false) == 0 {
                let _ = DeleteObject(region.into());
            }
        }
        if moved || resized || self.foreground == 0 {
            if SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                layout.x,
                layout.y,
                layout.width,
                layout.height,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            )
            .is_err()
            {
                self.hide();
                return;
            }
        }
        if resized {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
        self.layout = Some(layout);
        self.foreground = hint.foreground;
        self.focus = hint.focus;
        self.revision = hint.revision;
        self.missing_since = None;
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

fn placement(caret: POINT, width: i32, height: i32, work: RECT, gap: i32) -> Option<(i32, i32)> {
    if width <= 0
        || height <= 0
        || width > work.right - work.left
        || height > work.bottom - work.top
    {
        return None;
    }
    let x = (caret.x + gap).clamp(work.left, work.right - width);
    let y = if caret.y + gap + height <= work.bottom {
        caret.y + gap
    } else {
        caret.y - height - gap
    };
    Some((x, y.clamp(work.top, work.bottom - height)))
}

unsafe extern "system" fn popup_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_NCHITTEST => return LRESULT(HTTRANSPARENT as isize),
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        WM_ERASEBKGND => return LRESULT(1),
        WM_PAINT => {
            let mut paint = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut paint);
            let warning = GetWindowLongPtrW(hwnd, GWLP_USERDATA) != 0;
            let mut bounds = RECT::default();
            let _ = GetClientRect(hwnd, &mut bounds);
            let brush = CreateSolidBrush(if warning {
                COLORREF(0x00C7EEFF)
            } else {
                COLORREF(0x00312218)
            });
            FillRect(dc, &bounds, brush);
            let _ = DeleteObject(brush.into());
            SetBkMode(dc, TRANSPARENT);
            SetTextColor(
                dc,
                if warning {
                    COLORREF(0x00192842)
                } else {
                    COLORREF(0x00F6F3ED)
                },
            );
            let dpi = GetDpiForWindow(hwnd).max(96);
            let scale = |n: i32| (n as i64 * dpi as i64 / 96) as i32;
            let icon = HICON(GetClassLongPtrW(hwnd, GCLP_HICON) as *mut _);
            let _ = DrawIconEx(
                dc,
                scale(8),
                scale(8),
                icon,
                scale(16),
                scale(16),
                0,
                None,
                DI_NORMAL,
            );
            let font = font(dpi);
            let old = SelectObject(dc, font.into());
            let mut text = vec![0u16; GetWindowTextLengthW(hwnd).max(0) as usize + 1];
            let length = GetWindowTextW(hwnd, &mut text);
            bounds.left = scale(32);
            bounds.right -= scale(10);
            DrawTextW(
                dc,
                &mut text[..length as usize],
                &mut bounds,
                DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX | DT_END_ELLIPSIS,
            );
            SelectObject(dc, old);
            let _ = DeleteObject(font.into());
            let _ = EndPaint(hwnd, &paint);
            return LRESULT(0);
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn screen() -> RECT {
        RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1040,
        }
    }
    #[test]
    fn typing_and_small_caret_noise_keep_one_anchor() {
        let first = stable_layout(None, POINT { x: 100, y: 100 }, 70, 32, 96, screen()).unwrap();
        for point in [
            POINT { x: 101, y: 99 },
            POINT { x: 105, y: 102 },
            POINT { x: 125, y: 97 },
            POINT { x: 220, y: 100 },
        ] {
            let next = stable_layout(Some(first), point, 68, 32, 96, screen()).unwrap();
            assert_eq!(
                (next.x, next.y, next.caret),
                (first.x, first.y, first.caret)
            );
        }
        let line =
            stable_layout(Some(first), POINT { x: 100, y: 122 }, 70, 32, 96, screen()).unwrap();
        assert_eq!(line.y, 130);
        let far =
            stable_layout(Some(first), POINT { x: 300, y: 100 }, 70, 32, 96, screen()).unwrap();
        assert_eq!(far.x, 308);
    }
    #[test]
    fn warning_width_changes_do_not_oscillate_at_the_right_edge() {
        let first = stable_layout(None, POINT { x: 1850, y: 100 }, 70, 32, 96, screen()).unwrap();
        let wide = stable_layout(
            Some(first),
            POINT { x: 1852, y: 100 },
            300,
            32,
            96,
            screen(),
        )
        .unwrap();
        let neutral =
            stable_layout(Some(wide), POINT { x: 1855, y: 101 }, 60, 32, 96, screen()).unwrap();
        assert_eq!((wide.x, wide.width), (1620, 300));
        assert_eq!(
            (neutral.x, neutral.y, neutral.width),
            (wide.x, wide.y, wide.width)
        );
    }
    #[test]
    fn monitor_and_dpi_changes_reset_the_anchor_safely() {
        let first = stable_layout(None, POINT { x: 100, y: 100 }, 70, 32, 96, screen()).unwrap();
        let work = RECT {
            left: -1920,
            top: 0,
            right: 0,
            bottom: 1040,
        };
        let next =
            stable_layout(Some(first), POINT { x: -10, y: 1020 }, 300, 48, 144, work).unwrap();
        assert_eq!((next.x, next.y), (-300, 960));
        assert_eq!(next.dpi, 144);
    }
    #[test]
    fn warning_clear_preserves_geometry_and_transient_missing_reads_have_a_grace_period() {
        unsafe {
            let instance: HINSTANCE = windows::Win32::System::LibraryLoader::GetModuleHandleW(None)
                .unwrap()
                .into();
            let icon = LoadIconW(Some(instance), PCWSTR(1usize as *const u16)).unwrap();
            let mut overlay = Overlay::new(instance, HWND::default(), icon).unwrap();
            let layout =
                stable_layout(None, POINT { x: 100, y: 100 }, 200, 32, 96, screen()).unwrap();
            overlay.layout = Some(layout);
            overlay.foreground = 1;
            assert!(overlay.set_content("TH? แล้ว", true));
            overlay.clear_warning("EN", 5);
            assert!(!overlay.suspicious);
            assert_eq!(overlay.label, "EN");
            assert_eq!(overlay.layout, Some(layout));
            overlay.note_unavailable(1000);
            overlay.expire_unavailable(1249);
            assert_eq!(overlay.foreground, 1);
            overlay.expire_unavailable(1250);
            assert_eq!(overlay.foreground, 0);
            assert!(overlay.layout.is_none());
            assert_eq!(GetWindowTextLengthW(overlay.hwnd), 0);
        }
    }
    #[test]
    fn embedded_icon_and_popup_do_not_require_keyboard_focus() {
        unsafe {
            let instance: HINSTANCE = windows::Win32::System::LibraryLoader::GetModuleHandleW(None)
                .unwrap()
                .into();
            let icon = LoadIconW(Some(instance), PCWSTR(1usize as *const u16)).unwrap();
            let foreground = GetForegroundWindow();
            let overlay = Overlay::new(instance, HWND::default(), icon).unwrap();
            let style = GetWindowLongPtrW(overlay.hwnd, GWL_EXSTYLE) as u32;
            let required = WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TRANSPARENT | WS_EX_LAYERED;
            assert_eq!(style & required.0, required.0);
            assert_eq!(GetForegroundWindow(), foreground);
            assert!(!IsWindowVisible(overlay.hwnd).as_bool());
        }
    }
    #[test]
    fn placement_handles_screen_edges_and_negative_monitors() {
        let work = RECT {
            left: -1920,
            top: 0,
            right: 0,
            bottom: 1040,
        };
        assert_eq!(
            placement(POINT { x: -4, y: 1025 }, 200, 32, work, 8),
            Some((-200, 985))
        );
        assert_eq!(
            placement(POINT { x: -1919, y: 0 }, 200, 32, work, 8),
            Some((-1911, 8))
        );
        assert_eq!(placement(POINT { x: 0, y: 0 }, 3000, 32, work, 8), None);
    }
}
