use windows::{
    core::{Interface, BSTR},
    Win32::{
        Foundation::*,
        Graphics::Gdi::ClientToScreen,
        System::{Ole::*, Variant::*},
        UI::{Accessibility::*, Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
    },
};

const MAX_WORD: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ReadMode {
    Current,
    Completed,
}

pub unsafe fn native_message(hwnd: HWND, msg: u32, wp: usize, lp: isize) -> Option<usize> {
    let mut result = 0;
    (SendMessageTimeoutW(
        hwnd,
        msg,
        WPARAM(wp),
        LPARAM(lp),
        SMTO_ABORTIFHUNG | SMTO_BLOCK,
        300,
        Some(&mut result),
    )
    .0 != 0)
        .then_some(result)
}

pub unsafe fn native_snapshot(hwnd: HWND) -> Option<(Vec<u16>, usize)> {
    let length = native_message(hwnd, WM_GETTEXTLENGTH, 0, 0)?;
    if length > 65534 {
        return None;
    }
    let selection = native_message(hwnd, 0x00B0 /* EM_GETSEL */, 0, 0)?;
    let caret = selection & 0xffff;
    if caret != (selection >> 16) & 0xffff || caret > length {
        return None;
    }
    let mut text = vec![0u16; length + 1];
    if native_message(hwnd, WM_GETTEXT, text.len(), text.as_mut_ptr() as isize)? != length {
        return None;
    }
    text.truncate(length);
    Some((text, caret))
}

#[cfg(test)]
pub unsafe fn native_word(hwnd: HWND) -> Option<(Vec<u16>, usize, String)> {
    native_word_mode(hwnd, ReadMode::Current)
}

fn token(prefix: &str, mode: ReadMode) -> Option<&str> {
    let before = if mode == ReadMode::Completed {
        prefix.strip_suffix(' ')?
    } else {
        prefix
    };
    let word = before.rsplit(char::is_whitespace).next()?;
    if word.is_empty() || word.chars().count() > MAX_WORD {
        return None;
    }
    let start = before.len() - word.len();
    // A Thai combining mark can belong to the preceding space's grapheme cluster.
    let start = if before[..start].ends_with(' ') {
        start - 1
    } else {
        start
    };
    Some(&prefix[start..])
}

fn native_editor(class: &str) -> bool {
    let class = class.to_ascii_lowercase();
    class == "edit" || class.starts_with("richedit")
}

unsafe fn replace_native_selection(hwnd: HWND, replacement: &str) -> bool {
    if replacement.contains('\0') {
        return false;
    }
    let text: Vec<u16> = replacement.encode_utf16().chain(Some(0)).collect();
    // EM_REPLACESEL is marshalled by Windows and inserts the full UTF-16 span, including leading marks.
    native_message(hwnd, 0x00C2, 1, text.as_ptr() as isize).is_some()
}

unsafe fn native_word_mode(hwnd: HWND, mode: ReadMode) -> Option<(Vec<u16>, usize, String)> {
    if GetWindowLongPtrW(hwnd, GWL_STYLE) as u32 & (ES_PASSWORD as u32 | ES_READONLY as u32) != 0 {
        return None;
    }
    let (text, caret) = native_snapshot(hwnd)?;
    let prefix = String::from_utf16(&text[..caret]).ok()?;
    let word = token(&prefix, mode)?.to_owned();
    Some((text, caret, word))
}

enum Selection {
    Native {
        hwnd: HWND,
        snapshot: Vec<u16>,
        caret: usize,
    },
    Accessible {
        pattern: IUIAutomationTextPattern,
        selected: IUIAutomationTextRange,
        range: IUIAutomationTextRange,
    },
}

pub struct Word {
    pub text: String,
    focused: IUIAutomationElement,
    selection: Selection,
    mode: ReadMode,
}

unsafe fn gui_focus(foreground: HWND) -> Option<GUITHREADINFO> {
    let mut info = GUITHREADINFO {
        cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    GetGUIThreadInfo(GetWindowThreadProcessId(foreground, None), &mut info).ok()?;
    Some(info)
}

impl Word {
    pub unsafe fn replace_native(&self, replacement: &str) -> Option<bool> {
        match &self.selection {
            Selection::Native { hwnd, .. } => Some(replace_native_selection(*hwnd, replacement)),
            _ => None,
        }
    }
    pub unsafe fn matches_replacement(
        &self,
        automation: &IUIAutomation,
        after: &Word,
        replacement: &str,
    ) -> windows::core::Result<bool> {
        if after.text != replacement
            || !automation
                .CompareElements(&self.focused, &after.focused)?
                .as_bool()
        {
            return Ok(false);
        }
        match (&self.selection, &after.selection) {
            (
                Selection::Native {
                    hwnd,
                    snapshot,
                    caret,
                },
                Selection::Native {
                    hwnd: other,
                    snapshot: actual,
                    caret: position,
                },
            ) => Ok(hwnd == other
                && native_after(
                    snapshot,
                    *caret,
                    self.text.encode_utf16().count(),
                    replacement,
                )
                .is_some_and(|(text, caret)| text == *actual && caret == *position)),
            (Selection::Accessible { range, .. }, Selection::Accessible { range: actual, .. }) => {
                Ok(range.CompareEndpoints(
                    TextPatternRangeEndpoint_Start,
                    actual,
                    TextPatternRangeEndpoint_Start,
                )? == 0)
            }
            _ => Ok(false),
        }
    }

    pub unsafe fn is_current(
        &self,
        automation: &IUIAutomation,
        foreground: HWND,
    ) -> windows::core::Result<bool> {
        if GetForegroundWindow() != foreground
            || self.focused.CurrentIsPassword()?.as_bool()
            || !automation
                .CompareElements(&self.focused, &automation.GetFocusedElement()?)?
                .as_bool()
        {
            return Ok(false);
        }
        match &self.selection {
            Selection::Native {
                hwnd,
                snapshot,
                caret,
            } => {
                if gui_focus(foreground).is_none_or(|f| f.hwndFocus != *hwnd) {
                    return Ok(false);
                }
                Ok(
                    native_word_mode(*hwnd, self.mode).is_some_and(|(text, position, word)| {
                        text == *snapshot && position == *caret && word == self.text
                    }),
                )
            }
            Selection::Accessible {
                pattern,
                selected,
                range,
            } => {
                let current = pattern.GetSelection()?;
                if current.Length()? != 1 {
                    return Ok(false);
                }
                let current = current.GetElement(0)?;
                Ok(selected.CompareEndpoints(
                    TextPatternRangeEndpoint_Start,
                    &current,
                    TextPatternRangeEndpoint_Start,
                )? == 0
                    && selected.CompareEndpoints(
                        TextPatternRangeEndpoint_End,
                        &current,
                        TextPatternRangeEndpoint_End,
                    )? == 0
                    && range.GetText(-1)?.to_string() == self.text)
            }
        }
    }

    pub unsafe fn select_if_unchanged(
        &self,
        automation: &IUIAutomation,
        foreground: HWND,
    ) -> windows::core::Result<bool> {
        if !self.is_current(automation, foreground)?
            || [VK_SHIFT, VK_CONTROL, VK_MENU, VK_LWIN, VK_RWIN]
                .iter()
                .any(|k| GetAsyncKeyState(k.0 as i32) < 0)
        {
            return Ok(false);
        }
        match &self.selection {
            Selection::Native { hwnd, caret, .. } => {
                let start = *caret - self.text.encode_utf16().count();
                if native_message(*hwnd, 0x00B1 /* EM_SETSEL */, start, *caret as isize).is_none() {
                    return Ok(false);
                }
                Ok(native_message(*hwnd, 0x00B0, 0, 0)
                    .is_some_and(|s| s & 0xffff == start && (s >> 16) & 0xffff == *caret))
            }
            Selection::Accessible { range, pattern, .. } => {
                range.Select()?;
                let actual = pattern.GetSelection()?;
                Ok(actual.Length()? == 1
                    && actual.GetElement(0)?.GetText(-1)?.to_string() == self.text)
            }
        }
    }

    pub unsafe fn position(&self, foreground: HWND) -> Option<POINT> {
        // Prefer the real Windows caret; no fallback to the mouse pointer or an arbitrary corner.
        if let Some(info) = gui_focus(foreground) {
            if !info.hwndCaret.0.is_null() && info.rcCaret.bottom > info.rcCaret.top {
                let mut point = POINT {
                    x: info.rcCaret.right,
                    y: info.rcCaret.bottom,
                };
                if ClientToScreen(info.hwndCaret, &mut point).as_bool() {
                    return Some(point);
                }
            }
        }
        if let Selection::Accessible { selected, .. } = &self.selection {
            let range = selected.Clone().ok()?;
            // The preceding character has bounds even when a collapsed caret range has none.
            if range
                .MoveEndpointByUnit(TextPatternRangeEndpoint_Start, TextUnit_Character, -1)
                .ok()?
                == 0
            {
                return None;
            }
            return range_position(&range);
        }
        None
    }
}

fn native_after(
    snapshot: &[u16],
    caret: usize,
    before_length: usize,
    replacement: &str,
) -> Option<(Vec<u16>, usize)> {
    let start = caret.checked_sub(before_length)?;
    if caret > snapshot.len() {
        return None;
    }
    let mut after = snapshot[..start].to_vec();
    after.extend(replacement.encode_utf16());
    let position = after.len();
    after.extend_from_slice(&snapshot[caret..]);
    Some((after, position))
}

#[cfg(test)]
mod history_tests {
    use super::*;
    #[test]
    fn native_completed_word_and_long_input_use_real_edit_selection() {
        unsafe {
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                windows::core::w!("EDIT"),
                windows::core::w!("prefix c]h; "),
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
            native_message(hwnd, 0x00B1, 12, 12).unwrap();
            assert_eq!(
                native_word_mode(hwnd, ReadMode::Completed).unwrap().2,
                " c]h; "
            );
            assert!(native_word_mode(hwnd, ReadMode::Current).is_none());
            let mut long: Vec<u16> = "ก".repeat(512).encode_utf16().chain(Some(0)).collect();
            SetWindowTextW(hwnd, windows::core::PCWSTR(long.as_ptr())).unwrap();
            native_message(hwnd, 0x00B1, 512, 512).unwrap();
            assert_eq!(
                native_word_mode(hwnd, ReadMode::Current)
                    .unwrap()
                    .2
                    .chars()
                    .count(),
                512
            );
            long.insert(0, 'ก' as u16);
            SetWindowTextW(hwnd, windows::core::PCWSTR(long.as_ptr())).unwrap();
            native_message(hwnd, 0x00B1, 513, 513).unwrap();
            assert!(native_word_mode(hwnd, ReadMode::Current).is_none());
            let _ = DestroyWindow(hwnd);
        }
    }
    #[test]
    fn completed_words_keep_the_space_for_undo_and_long_words_are_bounded() {
        assert_eq!(token("prefix c]h; ", ReadMode::Completed), Some(" c]h; "));
        assert_eq!(token("hello  ", ReadMode::Completed), None);
        assert_eq!(token("hello\n", ReadMode::Completed), None);
        assert_eq!(token("hello ", ReadMode::Current), None);
        let long = "ก".repeat(512);
        assert_eq!(token(&long, ReadMode::Current), Some(long.as_str()));
        assert!(token(&format!("{long}ก"), ReadMode::Current).is_none());
    }
    #[test]
    fn undo_anchor_preserves_surrogates_and_text_after_caret() {
        let before: Vec<u16> = "😀 c]h; tail".encode_utf16().collect();
        let (after, caret) = native_after(&before, 7, 4, "แล้ว").unwrap();
        assert_eq!(String::from_utf16(&after).unwrap(), "😀 แล้ว tail");
        assert_eq!(caret, 7);
        assert_eq!(native_after(&before, 1, 4, "แล้ว"), None);
        assert_eq!(native_after(&before, 999, 4, "แล้ว"), None);
    }
    #[test]
    fn leading_marks_stay_with_the_space_cluster_in_both_read_modes() {
        assert_eq!(token("prefix ืนะรดั", ReadMode::Current), Some(" ืนะรดั"));
        assert_eq!(token("prefix ืนะรดั ", ReadMode::Completed), Some(" ืนะรดั "));
        assert_eq!(token("ืนะรดั", ReadMode::Current), Some("ืนะรดั"));
        assert_eq!(token("prefix\nืนะรดั", ReadMode::Current), Some("ืนะรดั"));
        assert!(native_editor("RichEditD2DPT"));
        assert!(native_editor("RICHEDIT50W"));
        assert!(!native_editor("Chrome_RenderWidgetHostHWND"));
    }
    #[test]
    fn real_richedit_preserves_notify_and_leading_thai_marks_when_replaced() {
        unsafe {
            let library = windows::Win32::System::LibraryLoader::LoadLibraryW(windows::core::w!(
                "Msftedit.dll"
            ))
            .unwrap();
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                windows::core::w!("RICHEDIT50W"),
                windows::core::w!(""),
                WINDOW_STYLE(ES_MULTILINE as u32 | ES_AUTOHSCROLL as u32),
                0,
                0,
                300,
                100,
                None,
                None,
                None,
                None,
            )
            .unwrap();
            for (before, expected) in [
                ("notify", "ืนะรดั"),
                ("ืนะรดั", "notify"),
                ("prefix notify", "prefix ืนะรดั"),
                ("prefix ืนะรดั", "prefix notify"),
                ("n", "ื"),
                ("ื", "n"),
                ("ิ", "b"),
            ] {
                let text: Vec<u16> = before.encode_utf16().chain(Some(0)).collect();
                SetWindowTextW(hwnd, windows::core::PCWSTR(text.as_ptr())).unwrap();
                let caret = before.encode_utf16().count();
                native_message(hwnd, 0x00B1, caret, caret as isize).unwrap();
                let (_, position, word) = native_word_mode(hwnd, ReadMode::Current).unwrap();
                let replacement = crate::convert::convert(&word).unwrap().0;
                let start = position - word.encode_utf16().count();
                native_message(hwnd, 0x00B1, start, position as isize).unwrap();
                assert!(replace_native_selection(hwnd, &replacement));
                let (actual, _) = native_snapshot(hwnd).unwrap();
                assert_eq!(String::from_utf16(&actual).unwrap(), expected, "{before:?}");
            }
            let _ = DestroyWindow(hwnd);
            let _ = FreeLibrary(library);
        }
    }
}

unsafe fn range_position(range: &IUIAutomationTextRange) -> Option<POINT> {
    let array = range.GetBoundingRectangles().ok()?;
    if array.is_null() {
        return None;
    }
    let result = (|| {
        if SafeArrayGetDim(array) != 1
            || SafeArrayGetElemsize(array) as usize != std::mem::size_of::<f64>()
        {
            return None;
        }
        let low = SafeArrayGetLBound(array, 1).ok()?;
        let high = SafeArrayGetUBound(array, 1).ok()?;
        let count = high.checked_sub(low)?.checked_add(1)?;
        if count < 4 || count % 4 != 0 {
            return None;
        }
        let mut rect = [0f64; 4];
        for (offset, value) in rect.iter_mut().enumerate() {
            let index = high - 3 + offset as i32;
            SafeArrayGetElement(array, &index, (value as *mut f64).cast()).ok()?;
        }
        if !rect.iter().all(|v| v.is_finite() && v.abs() < 1_000_000.0)
            || rect[2] < 0.0
            || rect[3] <= 0.0
        {
            return None;
        }
        Some(POINT {
            x: (rect[0] + rect[2]) as i32,
            y: (rect[1] + rect[3]) as i32,
        })
    })();
    let _ = SafeArrayDestroy(array);
    result
}

pub unsafe fn read_word(
    automation: &IUIAutomation,
    foreground: HWND,
) -> windows::core::Result<Option<Word>> {
    read_word_mode(automation, foreground, ReadMode::Current)
}

pub unsafe fn read_word_mode(
    automation: &IUIAutomation,
    foreground: HWND,
    mode: ReadMode,
) -> windows::core::Result<Option<Word>> {
    if foreground.0.is_null() || GetForegroundWindow() != foreground {
        return Ok(None);
    }
    let focused = automation.GetFocusedElement()?;
    if focused.CurrentIsPassword()?.as_bool() {
        return Ok(None);
    }
    let class = focused.CurrentClassName()?.to_string();
    if native_editor(&class) {
        let hwnd = focused.CurrentNativeWindowHandle()?;
        if !hwnd.0.is_null()
            && GetWindowLongPtrW(hwnd, GWL_STYLE) as u32 & (ES_PASSWORD as u32 | ES_READONLY as u32)
                != 0
        {
            return Ok(None);
        }
        if let Some((snapshot, caret, text)) = native_word_mode(hwnd, mode) {
            return Ok(Some(Word {
                mode,
                text,
                focused,
                selection: Selection::Native {
                    hwnd,
                    snapshot,
                    caret,
                },
            }));
        }
        if class.eq_ignore_ascii_case("edit") {
            return Ok(None);
        }
        // Large RichEdit documents can still use the bounded Accessibility path.
    }
    let pattern: IUIAutomationTextPattern = focused.GetCurrentPattern(UIA_TextPatternId)?.cast()?;
    let selections = pattern.GetSelection()?;
    if selections.Length()? != 1 {
        return Ok(None);
    }
    let selected = selections.GetElement(0)?;
    if !selected.GetText(1)?.is_empty() {
        return Ok(None);
    }
    let mut read_only = selected.GetAttributeValue(UIA_IsReadOnlyAttributeId)?;
    let writable = read_only.Anonymous.Anonymous.vt == VT_BOOL
        && read_only.Anonymous.Anonymous.Anonymous.boolVal == VARIANT_FALSE;
    VariantClear(&mut read_only)?;
    if !writable {
        return Ok(None);
    }
    let mut range = selected.Clone()?;
    let mut text = String::new();
    if mode == ReadMode::Completed {
        if range.MoveEndpointByUnit(TextPatternRangeEndpoint_Start, TextUnit_Character, -1)? == 0 {
            return Ok(None);
        }
        text = range.GetText(-1)?.to_string();
        if text != " " {
            return Ok(None);
        }
    }
    for _ in 0..=MAX_WORD {
        if range.MoveEndpointByUnit(TextPatternRangeEndpoint_Start, TextUnit_Character, -1)? == 0 {
            // Some providers have no caret stop before an initial combining mark.
            let prefix = pattern.DocumentRange()?.Clone()?;
            prefix.MoveEndpointByRange(
                TextPatternRangeEndpoint_End,
                &selected,
                TextPatternRangeEndpoint_Start,
            )?;
            let limit = (MAX_WORD * 2 + 4) as i32;
            let full = prefix.GetText(limit)?.to_string();
            if full.encode_utf16().count() < limit as usize
                && token(&full, mode) == Some(full.as_str())
                && full.ends_with(&text)
            {
                range = prefix;
                text = full;
            }
            break;
        }
        let candidate = range.GetText(-1)?.to_string();
        if candidate.chars().next().is_some_and(char::is_whitespace) {
            let Some(span) = token(&candidate, mode) else {
                return Ok(None);
            };
            // Do not advance one Character: a space plus a leading Thai mark may be one unit.
            if span != candidate {
                range = range.FindText(&BSTR::from(span), true, false)?;
                if range.GetText(-1)?.to_string() != span {
                    return Ok(None);
                }
            }
            text = span.to_owned();
            break;
        }
        text = candidate;
        if text.chars().count() > MAX_WORD + usize::from(mode == ReadMode::Completed) {
            return Ok(None);
        }
    }
    if token(&text, mode) != Some(text.as_str()) {
        return Ok(None);
    }
    Ok(Some(Word {
        mode,
        text,
        focused,
        selection: Selection::Accessible {
            pattern,
            selected,
            range,
        },
    }))
}
