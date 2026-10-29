pub const SHIFT: u8 = 1;
pub const CTRL: u8 = 2;
pub const ALT: u8 = 4;
const WIN: u8 = 8;
const DOUBLE_MS: u64 = 400;

pub fn normalize_modifier(key: u16, scan: u32, extended: bool) -> u16 {
    match key {
        16 => {
            if scan == 0x36 {
                161
            } else {
                160
            }
        }
        17 => {
            if extended {
                163
            } else {
                162
            }
        }
        18 => {
            if extended {
                165
            } else {
                164
            }
        }
        _ => key,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shortcut {
    Single(u16),
    Double(u16),
    Chord { modifiers: u8, key: u16 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureMode {
    Single,
    Double,
    Chord,
}

pub fn modifier(vk: u16) -> u8 {
    match vk {
        16 | 160 | 161 => SHIFT,
        17 | 162 | 163 => CTRL,
        18 | 164 | 165 => ALT,
        91 | 92 => WIN,
        _ => 0,
    }
}

pub fn typing_key(vk: u16, modifiers: u8) -> bool {
    modifiers & (CTRL | ALT | WIN) == 0
        && matches!(vk, 8 | 32 | 46 | 48..=90 | 96..=111 | 186..=226 | 229)
}

fn key_name(vk: u16) -> Option<String> {
    Some(match vk {
        48..=57 | 65..=90 => char::from_u32(vk as u32)?.to_string(),
        112..=135 => format!("F{}", vk - 111),
        8 => "Backspace".into(),
        9 => "Tab".into(),
        13 => "Enter".into(),
        20 => "CapsLock".into(),
        27 => "Escape".into(),
        32 => "Space".into(),
        33 => "PageUp".into(),
        34 => "PageDown".into(),
        35 => "End".into(),
        36 => "Home".into(),
        37 => "Left".into(),
        38 => "Up".into(),
        39 => "Right".into(),
        40 => "Down".into(),
        45 => "Insert".into(),
        46 => "Delete".into(),
        160 => "LShift".into(),
        161 => "RShift".into(),
        162 => "LCtrl".into(),
        163 => "RCtrl".into(),
        164 => "LAlt".into(),
        186 => ";".into(),
        187 => "=".into(),
        188 => ",".into(),
        189 => "-".into(),
        190 => ".".into(),
        191 => "/".into(),
        192 => "`".into(),
        219 => "[".into(),
        220 => "\\".into(),
        221 => "]".into(),
        222 => "'".into(),
        _ => return None,
    })
}

impl Shortcut {
    pub fn capture_mode(self) -> CaptureMode {
        match self {
            Self::Single(_) => CaptureMode::Single,
            Self::Double(_) => CaptureMode::Double,
            Self::Chord { .. } => CaptureMode::Chord,
        }
    }
    pub fn valid(self) -> bool {
        match self {
            Self::Single(key) | Self::Double(key) => matches!(key, 160..=164),
            Self::Chord { modifiers, key } => {
                modifiers & !(SHIFT | CTRL | ALT) == 0
                    && modifiers & (CTRL | ALT) != 0
                    && modifier(key) == 0
                    && key_name(key).is_some()
                    && !(modifiers & ALT != 0 && matches!(key, 9 | 27 | 115))
                    && !(modifiers & CTRL != 0 && key == 27)
                    && !(modifiers & (CTRL | ALT) == CTRL | ALT && key == 46)
            }
        }
    }

    pub fn label(self) -> String {
        match self {
            Self::Single(key) => format!("{} ×1", key_name(key).unwrap_or_default()),
            Self::Double(key) => format!("{} ×2", key_name(key).unwrap_or_default()),
            Self::Chord { modifiers, key } => {
                let mut label = String::new();
                for (mask, name) in [(CTRL, "Ctrl+"), (ALT, "Alt+"), (SHIFT, "Shift+")] {
                    if modifiers & mask != 0 {
                        label.push_str(name);
                    }
                }
                label.push_str(&key_name(key).unwrap_or_default());
                label
            }
        }
    }

    pub fn encode(self) -> String {
        match self {
            Self::Single(key) => format!("single:{key}"),
            Self::Double(key) => format!("double:{key}"),
            Self::Chord { modifiers, key } => format!("chord:{modifiers}:{key}"),
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        let parts: Vec<_> = text.split(':').collect();
        let result = match parts.as_slice() {
            ["single", key] => Self::Single(key.parse().ok()?),
            ["double", key] => Self::Double(key.parse().ok()?),
            ["chord", modifiers, key] => Self::Chord {
                modifiers: modifiers.parse().ok()?,
                key: key.parse().ok()?,
            },
            _ => return None,
        };
        result.valid().then_some(result)
    }
}

#[derive(Default, Debug)]
pub struct Decision {
    pub consume: bool,
    pub trigger: bool,
    pub cancel_alt_menu: bool,
    pub invalidate: bool,
    pub typing: bool,
    pub completed_space: bool,
    pub recorded: bool,
}

pub struct KeyEngine {
    shortcut: Shortcut,
    pressed: [bool; 256],
    suppressed: [bool; 256],
    last_tap: Option<(u64, isize)>,
    down_at: u64,
    tap_window: isize,
    space_window: isize,
    clean_tap: bool,
    last_key: u16,
    capture: Option<CaptureMode>,
    captured: Option<Result<Shortcut, &'static str>>,
}

impl KeyEngine {
    pub fn new(shortcut: Shortcut) -> Self {
        Self {
            shortcut,
            pressed: [false; 256],
            suppressed: [false; 256],
            last_tap: None,
            down_at: 0,
            tap_window: 0,
            space_window: 0,
            clean_tap: false,
            last_key: 0,
            capture: None,
            captured: None,
        }
    }
    pub fn configure(&mut self, shortcut: Shortcut) {
        self.shortcut = shortcut;
        self.last_tap = None;
        self.capture = None;
        self.last_key = 0;
    }
    pub fn reset_tap(&mut self) {
        self.last_tap = None;
        self.clean_tap = false;
    }
    pub fn interrupt(&mut self) {
        self.reset_tap();
        self.last_key = 0;
        self.space_window = 0;
    }
    pub fn begin_capture(&mut self, mode: CaptureMode) {
        self.capture = Some(mode);
        self.captured = None;
        self.reset_tap();
    }
    pub fn cancel_capture(&mut self) {
        self.capture = None;
        self.captured = None;
    }
    pub fn take_capture(&mut self) -> Option<Result<Shortcut, &'static str>> {
        self.captured.take()
    }
    pub fn modifiers(&self) -> u8 {
        let mut mask = 0;
        for key in [160, 161, 162, 163, 164, 165, 91, 92] {
            if self.pressed[key] {
                mask |= modifier(key as u16);
            }
        }
        mask
    }

    pub fn event(
        &mut self,
        key: u16,
        down: bool,
        now: u64,
        foreground: isize,
        eligible: bool,
    ) -> Decision {
        let mut result = Decision::default();
        if key > 255 {
            return result;
        }
        let was_down = self.pressed[key as usize];
        self.pressed[key as usize] = down;
        let mods = self.modifiers();
        if self.suppressed[key as usize] {
            if !down {
                self.suppressed[key as usize] = false;
            }
            result.consume = true;
            return result;
        }
        if let Some(mode) = self.capture {
            result.consume = true;
            result.invalidate = down;
            if down {
                self.suppressed[key as usize] = true;
            }
            let candidate = match (mode, down, key) {
                (_, true, 27) => {
                    self.capture = None;
                    self.captured = Some(Err("Recording cancelled"));
                    result.recorded = true;
                    return result;
                }
                (CaptureMode::Single, true, _) => Some(Shortcut::Single(key)),
                (CaptureMode::Double, true, _) => Some(Shortcut::Double(key)),
                (CaptureMode::Chord, true, _) if modifier(key) == 0 => Some(Shortcut::Chord {
                    modifiers: mods,
                    key,
                }),
                _ => None,
            };
            if let Some(candidate) = candidate {
                self.capture = None;
                self.captured = Some(if candidate.valid() && !self.pressed[165] {
                    Ok(candidate)
                } else {
                    Err("Use left Alt or left/right Ctrl/Shift for single/double tap, or Ctrl/Alt + a key. Reserved shortcuts are unavailable.")
                });
                result.recorded = true;
            }
            return result;
        }
        let component = match self.shortcut {
            Shortcut::Single(vk) | Shortcut::Double(vk) => key == vk,
            Shortcut::Chord { modifiers, .. } => {
                modifier(key) != 0 && modifier(key) & modifiers != 0
            }
        };
        if down {
            result.typing = typing_key(key, mods);
            result.invalidate = !component;
            self.last_key = key;
            if key == 32 && !was_down {
                self.space_window = foreground;
            }
        }
        if !eligible || self.pressed[165] || mods & WIN != 0 {
            if down {
                result.invalidate = true;
            }
            self.reset_tap();
            self.last_key = 0;
            return result;
        }
        match self.shortcut {
            Shortcut::Single(vk) | Shortcut::Double(vk) => {
                if down && key == vk && !was_down {
                    self.clean_tap = mods == modifier(vk);
                    self.down_at = now;
                    self.tap_window = foreground;
                }
                if down && key != vk {
                    self.reset_tap();
                }
                if !down
                    && key == vk
                    && was_down
                    && self.clean_tap
                    && self.tap_window == foreground
                    && mods == 0
                    && now.saturating_sub(self.down_at) <= DOUBLE_MS
                {
                    result.cancel_alt_menu = vk == 164;
                    result.trigger = matches!(self.shortcut, Shortcut::Single(_))
                        || self.last_tap.is_some_and(|(time, window)| {
                            now.saturating_sub(time) <= DOUBLE_MS && window == foreground
                        });
                    self.last_tap = if result.trigger {
                        None
                    } else {
                        Some((now, foreground))
                    };
                    self.clean_tap = false;
                }
            }
            Shortcut::Chord { modifiers, key: vk } => {
                if down && key == vk && mods == modifiers {
                    result.consume = true;
                    self.suppressed[key as usize] = true;
                    result.trigger = !was_down;
                    result.cancel_alt_menu =
                        modifiers & ALT != 0 || modifiers & (CTRL | SHIFT) == (CTRL | SHIFT);
                    result.typing = false;
                    result.invalidate = false;
                }
            }
        }
        result.completed_space = !down
            && key == 32
            && was_down
            && self.last_key == 32
            && self.space_window == foreground
            && mods == 0
            && !result.consume;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hardware_generic_modifiers_keep_the_correct_side() {
        assert_eq!(normalize_modifier(18, 0, false), 164);
        assert_eq!(normalize_modifier(18, 0, true), 165);
        assert_eq!(normalize_modifier(17, 0, true), 163);
        assert_eq!(normalize_modifier(16, 0x36, false), 161);
    }
    fn tap(e: &mut KeyEngine, key: u16, time: u64, window: isize) -> Decision {
        e.event(key, true, time, window, true);
        e.event(key, false, time + 20, window, true)
    }
    #[test]
    fn double_alt_requires_clean_taps_same_window_and_deadline() {
        let mut e = KeyEngine::new(Shortcut::Double(164));
        assert!(!tap(&mut e, 164, 0, 1).trigger);
        assert!(tap(&mut e, 164, 200, 1).trigger);
        tap(&mut e, 164, 1000, 1);
        assert!(!tap(&mut e, 164, 1500, 1).trigger);
        assert!(!tap(&mut e, 164, 1600, 2).trigger);
        e.event(164, true, 1700, 2, true);
        assert!(!e.event(9, true, 1701, 2, true).consume);
        e.event(9, false, 1702, 2, true);
        let result = e.event(164, false, 1703, 2, true);
        assert!(!result.trigger && !result.cancel_alt_menu);
    }
    #[test]
    fn single_tap_fires_on_release_without_breaking_chords_or_focus_guards() {
        for key in 160..=164 {
            let mut e = KeyEngine::new(Shortcut::Single(key));
            let down = e.event(key, true, 0, 1, true);
            assert!(!down.trigger && !down.consume && !down.invalidate);
            assert!(!e.event(key, true, 1, 1, true).trigger);
            let up = e.event(key, false, 20, 1, true);
            assert!(up.trigger && !up.consume && !up.invalidate);
            assert_eq!(up.cancel_alt_menu, key == 164);
            assert!(!e.event(key, false, 21, 1, true).trigger);

            e.event(key, true, 100, 1, true);
            assert!(!e.event(65, true, 110, 1, true).consume);
            e.event(65, false, 120, 1, true);
            assert!(!e.event(key, false, 130, 1, true).trigger);

            e.event(key, true, 200, 1, true);
            assert!(!e.event(key, false, 220, 2, true).trigger);
            e.event(key, true, 300, 1, true);
            assert!(!e.event(key, false, 800, 1, true).trigger);
            e.event(key, true, 900, 1, true);
            e.interrupt();
            assert!(!e.event(key, false, 920, 1, true).trigger);
            e.event(key, true, 1000, 1, true);
            assert!(!e.event(key, false, 1020, 1, false).trigger);
        }
        let mut e = KeyEngine::new(Shortcut::Single(164));
        e.event(162, true, 0, 1, true);
        e.event(164, true, 1, 1, true);
        e.event(162, false, 2, 1, true);
        assert!(!e.event(164, false, 3, 1, true).trigger);
    }
    #[test]
    fn single_capture_and_configuration_round_trip() {
        let mut e = KeyEngine::new(Shortcut::Double(164));
        e.begin_capture(CaptureMode::Single);
        assert!(e.event(164, true, 0, 1, true).recorded);
        assert_eq!(e.take_capture(), Some(Ok(Shortcut::Single(164))));
        assert!(e.event(164, false, 1, 1, true).consume);
        e.configure(Shortcut::Single(164));
        assert!(tap(&mut e, 164, 100, 1).trigger);
        for key in 160..=164 {
            let shortcut = Shortcut::Single(key);
            assert_eq!(Shortcut::parse(&shortcut.encode()), Some(shortcut));
            assert!(shortcut.label().ends_with("×1"));
        }
        for value in ["single:165", "single:65", "single:999", "single:-1"] {
            assert!(Shortcut::parse(value).is_none());
        }
    }
    #[test]
    fn chord_ignores_repeat_preserves_other_chords_and_undo_generation() {
        let mut e = KeyEngine::new(Shortcut::Chord {
            modifiers: CTRL,
            key: 32,
        });
        assert!(!e.event(162, true, 0, 1, true).invalidate);
        let d = e.event(32, true, 1, 1, true);
        assert!(d.trigger && d.consume && !d.invalidate);
        assert!(!e.event(32, true, 2, 1, true).trigger);
        assert!(e.event(32, false, 3, 1, true).consume);
        assert!(e.event(67, true, 4, 1, true).invalidate);
    }
    #[test]
    fn typing_filter_and_space_completion_exclude_navigation_and_shortcuts() {
        assert!(typing_key(65, SHIFT));
        assert!(!typing_key(37, 0));
        assert!(!typing_key(65, CTRL));
        let mut e = KeyEngine::new(Shortcut::Double(164));
        e.event(32, true, 0, 1, true);
        assert!(e.event(32, false, 1, 1, true).completed_space);
        assert!(!tap(&mut e, 164, 2, 1).completed_space);
        e.event(32, true, 10, 1, true);
        assert!(!e.event(32, false, 11, 2, true).completed_space);
        e.event(32, true, 20, 1, true);
        e.interrupt();
        assert!(!e.event(32, false, 21, 1, true).completed_space);
    }
    #[test]
    fn captures_chord_and_rejects_reserved_and_altgr() {
        let mut e = KeyEngine::new(Shortcut::Double(164));
        e.begin_capture(CaptureMode::Chord);
        e.event(162, true, 0, 1, true);
        assert!(e.event(32, true, 1, 1, true).recorded);
        assert_eq!(
            e.take_capture(),
            Some(Ok(Shortcut::Chord {
                modifiers: CTRL,
                key: 32
            }))
        );
        assert!(e.event(32, false, 2, 1, true).consume);
        assert!(e.event(162, false, 3, 1, true).consume);
        assert!(!Shortcut::Chord {
            modifiers: ALT,
            key: 9
        }
        .valid());
        assert!(!Shortcut::Double(165).valid());
        for shortcut in [
            Shortcut::Double(164),
            Shortcut::Chord {
                modifiers: CTRL,
                key: 32,
            },
        ] {
            assert_eq!(Shortcut::parse(&shortcut.encode()), Some(shortcut));
        }
    }
    #[test]
    fn alt_chord_marks_menu_suppression_and_long_holds_are_not_double_taps() {
        let mut e = KeyEngine::new(Shortcut::Chord {
            modifiers: ALT,
            key: 81,
        });
        e.event(164, true, 0, 1, true);
        let result = e.event(81, true, 1, 1, true);
        assert!(result.trigger && result.cancel_alt_menu);
        let mut e = KeyEngine::new(Shortcut::Double(164));
        tap(&mut e, 164, 0, 1);
        e.event(164, true, 30, 1, true);
        assert!(!e.event(164, false, 500, 1, true).trigger);
    }
}
