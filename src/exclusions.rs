use std::{
    collections::BTreeSet,
    fs::File,
    io::{self, Read},
    path::{Path, PathBuf},
};
use windows::{
    core::PWSTR,
    Win32::{Foundation::*, System::Threading::*, UI::WindowsAndMessaging::*},
};

pub struct Exclusions {
    path: PathBuf,
    names: BTreeSet<String>,
}

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Expected at most 128 executable names, one per line",
    )
}

fn normalize(name: &str) -> io::Result<String> {
    let name = name.trim().to_lowercase();
    if name.starts_with('#')
        || !name.ends_with(".exe")
        || name.len() > 255
        || name
            .chars()
            .any(|c| c.is_control() || "\\/:*?\"<>|".contains(c))
    {
        return Err(invalid());
    }
    Ok(name)
}

fn parse(text: &str) -> io::Result<BTreeSet<String>> {
    let mut names = BTreeSet::new();
    for line in text.trim_start_matches('\u{feff}').lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        names.insert(normalize(line)?);
        if names.len() > 128 {
            return Err(invalid());
        }
    }
    Ok(names)
}

impl Exclusions {
    pub fn load(path: PathBuf) -> io::Result<Self> {
        let mut text = String::new();
        match File::open(&path) {
            Ok(file) => {
                file.take(65537).read_to_string(&mut text)?;
                if text.len() > 65536 {
                    return Err(invalid());
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        Ok(Self {
            path,
            names: parse(&text)?,
        })
    }

    pub fn names(&self) -> impl Iterator<Item = &String> {
        self.names.iter()
    }
    pub fn contains(&self, name: &str) -> bool {
        self.names.contains(&name.trim().to_lowercase())
    }

    pub fn set(&mut self, name: &str, excluded: bool) -> io::Result<()> {
        let name = normalize(name)?;
        let mut next = self.names.clone();
        if excluded {
            next.insert(name);
        } else {
            next.remove(&name);
        }
        if next.len() > 128 {
            return Err(invalid());
        }
        if next == self.names {
            return Ok(());
        }
        let content = format!(
            "# Excluded executable names (case insensitive).\n{}\n",
            next.iter().cloned().collect::<Vec<_>>().join("\n")
        );
        crate::storage::write_atomic(&self.path, &content)?;
        self.names = next;
        Ok(())
    }
}

pub unsafe fn process_name(foreground: HWND) -> Option<String> {
    if foreground.0.is_null() {
        return None;
    }
    let mut pid = 0;
    GetWindowThreadProcessId(foreground, Some(&mut pid));
    if pid == 0 {
        return None;
    }
    let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
    let mut buffer = vec![0u16; 32768];
    let mut length = buffer.len() as u32;
    let result = QueryFullProcessImageNameW(
        handle,
        PROCESS_NAME_WIN32,
        PWSTR(buffer.as_mut_ptr()),
        &mut length,
    );
    let _ = CloseHandle(handle);
    result.ok()?;
    let path = String::from_utf16(&buffer[..length as usize]).ok()?;
    normalize(Path::new(&path).file_name()?.to_str()?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[test]
    fn resolves_executable_name_from_a_native_window() {
        unsafe {
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                windows::core::w!("STATIC"),
                windows::core::w!("process test"),
                WINDOW_STYLE(0),
                0,
                0,
                1,
                1,
                None,
                None,
                None,
                None,
            )
            .unwrap();
            let expected = std::env::current_exe()
                .unwrap()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_lowercase();
            assert_eq!(process_name(hwnd), Some(expected));
            let _ = DestroyWindow(hwnd);
            assert_eq!(process_name(HWND::default()), None);
        }
    }
    #[test]
    fn parses_comments_case_duplicates_and_rejects_paths() {
        let names = parse("\u{feff}# apps\nCode.exe\ncode.EXE\n WindowsTerminal.exe \n").unwrap();
        assert_eq!(names.len(), 2);
        assert!(names.contains("code.exe"));
        for invalid in ["../app.exe", "C:\\app.exe", "app.exe\tother.exe", "app.txt"] {
            assert!(parse(invalid).is_err());
        }
    }
    #[test]
    fn saves_additions_and_removals_and_preserves_state_on_failure() {
        let folder = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .join(format!(
                "tabthai-test-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        fs::create_dir(&folder).unwrap();
        let path = folder.join("excluded-apps.txt");
        let mut settings = Exclusions::load(path.clone()).unwrap();
        settings.set("Code.exe", true).unwrap();
        settings.set("WindowsTerminal.exe", true).unwrap();
        assert!(Exclusions::load(path.clone()).unwrap().contains("CODE.EXE"));
        let before = fs::read(&path).unwrap();
        let temporary = path.with_extension(format!("txt.tmp-{}", std::process::id()));
        fs::write(&temporary, "reserved").unwrap();
        assert!(settings.set("other.exe", true).is_err());
        assert!(!settings.contains("other.exe"));
        assert_eq!(fs::read(&path).unwrap(), before);
        fs::remove_file(temporary).unwrap();
        settings.set("code.exe", false).unwrap();
        let reloaded = Exclusions::load(path).unwrap();
        assert!(!reloaded.contains("code.exe"));
        assert!(reloaded.contains("windowsterminal.exe"));
        fs::remove_file(folder.join("excluded-apps.txt")).unwrap();
        fs::remove_dir(folder).unwrap();
    }
}
