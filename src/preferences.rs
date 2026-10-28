use crate::{
    input::{KeyEngine, Shortcut},
    storage::write_atomic,
};
use std::{
    fs::File,
    io::{self, Read},
    path::PathBuf,
};

#[derive(Clone)]
pub struct Preferences {
    pub auto: bool,
    pub shortcut: Shortcut,
    pub hint_seconds: u32,
    path: PathBuf,
}
pub struct Runtime {
    pub preferences: Preferences,
    pub keys: KeyEngine,
}

impl Runtime {
    pub fn new(preferences: Preferences) -> Self {
        Self {
            keys: KeyEngine::new(preferences.shortcut),
            preferences,
        }
    }
}

impl Preferences {
    pub const HINT_DURATIONS: [u32; 7] = [1, 2, 3, 5, 10, 15, 30];
    pub fn load(path: PathBuf) -> io::Result<Self> {
        let mut text = String::new();
        match File::open(&path) {
            Ok(file) => {
                file.take(4097).read_to_string(&mut text)?;
                if text.len() > 4096 {
                    return Err(invalid());
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        Self::parse(path, &text)
    }
    fn parse(path: PathBuf, text: &str) -> io::Result<Self> {
        let mut result = Self {
            auto: false,
            shortcut: Shortcut::Double(164),
            hint_seconds: 3,
            path,
        };
        let mut seen = std::collections::HashSet::new();
        for line in text
            .trim_start_matches('\u{feff}')
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty() && !s.starts_with('#'))
        {
            let (key, value) = line.split_once('=').ok_or_else(invalid)?;
            if !seen.insert(key.trim()) {
                return Err(invalid());
            }
            match key.trim() {
                "auto" => {
                    result.auto = match value.trim() {
                        "true" => true,
                        "false" => false,
                        _ => return Err(invalid()),
                    }
                }
                "shortcut" => {
                    result.shortcut = Shortcut::parse(value.trim()).ok_or_else(invalid)?
                }
                "hint_seconds" => {
                    result.hint_seconds = value.trim().parse().map_err(|_| invalid())?;
                    if !Self::HINT_DURATIONS.contains(&result.hint_seconds) {
                        return Err(invalid());
                    }
                }
                _ => return Err(invalid()),
            }
        }
        Ok(result)
    }
    pub fn save(&self) -> io::Result<()> {
        if !self.shortcut.valid() || !Self::HINT_DURATIONS.contains(&self.hint_seconds) {
            return Err(invalid());
        }
        write_atomic(
            &self.path,
            &format!(
                "# ThaiFlip preferences\nauto={}\nshortcut={}\nhint_seconds={}\n",
                self.auto,
                self.shortcut.encode(),
                self.hint_seconds
            ),
        )
    }
}
fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Invalid settings.ini: check auto, shortcut, and hint_seconds (1, 2, 3, 5, 10, 15, 30)",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_and_validation_do_not_enable_auto_accidentally() {
        let p = Preferences::parse(PathBuf::new(), "").unwrap();
        assert!(!p.auto);
        assert_eq!(p.hint_seconds, 3);
        assert_eq!(p.shortcut, Shortcut::Double(164));
        assert!(
            Preferences::parse(PathBuf::new(), "auto=true\nshortcut=chord:2:32")
                .unwrap()
                .auto
        );
        for text in [
            "auto=yes",
            "auto=true\nauto=false",
            "shortcut=chord:4:9",
            "unknown=1",
            "hint_seconds=0",
            "hint_seconds=9999999999",
            "hint_seconds=bad",
        ] {
            assert!(Preferences::parse(PathBuf::new(), text).is_err());
        }
        for seconds in Preferences::HINT_DURATIONS {
            assert_eq!(
                Preferences::parse(PathBuf::new(), &format!("hint_seconds={seconds}"))
                    .unwrap()
                    .hint_seconds,
                seconds
            );
        }
    }
}
