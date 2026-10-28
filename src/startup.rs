use std::{io, os::windows::ffi::OsStrExt, path::Path};
use windows::{
    core::{w, PCWSTR},
    Win32::{Foundation::*, System::Registry::*},
};

#[derive(Clone, Debug, PartialEq)]
pub struct Value {
    kind: REG_VALUE_TYPE,
    bytes: Vec<u8>,
}

#[derive(Clone)]
pub struct Registration {
    key: Vec<u16>,
    command: Value,
    #[cfg(test)]
    _cleanup: std::sync::Arc<TestKey>,
}

pub struct Change {
    before: Option<Value>,
    after: Option<Value>,
}
struct Key(HKEY);
impl Drop for Key {
    fn drop(&mut self) {
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

fn check(status: WIN32_ERROR) -> io::Result<()> {
    if status == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(status.0 as i32))
    }
}
fn conflict() -> io::Error {
    io::Error::other("Startup was changed elsewhere. Reopen Settings before changing it.")
}
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn executable_value(exe: &Path) -> io::Result<Value> {
    let raw: Vec<u16> = exe.as_os_str().encode_wide().collect();
    if !exe.is_absolute() || raw.is_empty() || raw.iter().any(|c| matches!(*c, 0 | 34)) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Startup requires an absolute executable path without quotes or NUL",
        ));
    }
    let command = std::iter::once(34).chain(raw).chain([34, 0]);
    Ok(Value {
        kind: REG_SZ,
        bytes: command.flat_map(u16::to_le_bytes).collect(),
    })
}

impl Registration {
    pub fn current() -> io::Result<Self> {
        #[cfg(test)]
        {
            Self::isolated()
        }
        #[cfg(not(test))]
        {
            Ok(Self {
                key: wide("Software\\Microsoft\\Windows\\CurrentVersion\\Run"),
                command: executable_value(&std::env::current_exe()?)?,
            })
        }
    }

    pub fn is_enabled(&self, value: &Option<Value>) -> bool {
        value.as_ref() == Some(&self.command)
    }

    pub fn read(&self) -> io::Result<Option<Value>> {
        unsafe {
            let mut handle = HKEY::default();
            let status = RegOpenKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(self.key.as_ptr()),
                None,
                KEY_QUERY_VALUE,
                &mut handle,
            );
            if status == ERROR_FILE_NOT_FOUND {
                return Ok(None);
            }
            check(status)?;
            let handle = Key(handle);
            let mut kind = REG_NONE;
            let mut size = 0;
            let status = RegQueryValueExW(
                handle.0,
                w!("TabThai"),
                None,
                Some(&mut kind),
                None,
                Some(&mut size),
            );
            if status == ERROR_FILE_NOT_FOUND {
                return Ok(None);
            }
            check(status)?;
            if size > 32768 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Startup value is unexpectedly large",
                ));
            }
            let mut bytes = vec![0; size as usize];
            check(RegQueryValueExW(
                handle.0,
                w!("TabThai"),
                None,
                Some(&mut kind),
                Some(bytes.as_mut_ptr()),
                Some(&mut size),
            ))?;
            bytes.truncate(size as usize);
            Ok(Some(Value { kind, bytes }))
        }
    }

    fn write(&self, value: &Option<Value>) -> io::Result<()> {
        unsafe {
            let mut handle = HKEY::default();
            if value.is_some() {
                check(RegCreateKeyExW(
                    HKEY_CURRENT_USER,
                    PCWSTR(self.key.as_ptr()),
                    None,
                    PCWSTR::null(),
                    REG_OPTION_NON_VOLATILE,
                    KEY_SET_VALUE,
                    None,
                    &mut handle,
                    None,
                ))?;
            } else {
                let status = RegOpenKeyExW(
                    HKEY_CURRENT_USER,
                    PCWSTR(self.key.as_ptr()),
                    None,
                    KEY_SET_VALUE,
                    &mut handle,
                );
                if status == ERROR_FILE_NOT_FOUND {
                    return Ok(());
                }
                check(status)?;
            }
            let handle = Key(handle);
            if let Some(value) = value {
                check(RegSetValueExW(
                    handle.0,
                    w!("TabThai"),
                    None,
                    value.kind,
                    Some(&value.bytes),
                ))
            } else {
                let status = RegDeleteValueW(handle.0, w!("TabThai"));
                if status == ERROR_FILE_NOT_FOUND {
                    Ok(())
                } else {
                    check(status)
                }
            }
        }
    }

    pub fn set_enabled(
        &self,
        enabled: bool,
        expected: &Option<Value>,
    ) -> io::Result<Option<Change>> {
        if self.read()? != *expected {
            return Err(conflict());
        }
        if enabled == self.is_enabled(expected) {
            return Ok(None);
        }
        // Windows documents a 260-character limit for Run command lines.
        if enabled && (self.command.bytes.len() / 2).saturating_sub(1) > 260 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Executable path is too long for Windows Startup. Move TabThai to a shorter path.",
            ));
        }
        let after = if enabled {
            Some(self.command.clone())
        } else {
            None
        };
        self.write(&after)?;
        Ok(Some(Change {
            before: expected.clone(),
            after,
        }))
    }

    pub fn rollback(&self, change: Change) -> io::Result<()> {
        if self.read()? != change.after {
            return Err(conflict());
        }
        self.write(&change.before)
    }

    #[cfg(test)]
    pub fn isolated() -> io::Result<Self> {
        let name = format!(
            "Software\\TabThaiStartupTest-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let key = wide(&name);
        Ok(Self {
            key: key.clone(),
            command: executable_value(&std::env::current_exe()?)?,
            _cleanup: std::sync::Arc::new(TestKey(key)),
        })
    }

    #[cfg(test)]
    pub fn unreadable_for_test() -> io::Result<Self> {
        let registration = Self::isolated()?;
        registration.write(&Some(Value {
            kind: REG_BINARY,
            bytes: vec![0; 32769],
        }))?;
        Ok(registration)
    }
}

#[cfg(test)]
struct TestKey(Vec<u16>);
#[cfg(test)]
impl Drop for TestKey {
    fn drop(&mut self) {
        unsafe {
            let name = String::from_utf16_lossy(&self.0[..self.0.len() - 1]);
            assert!(name.starts_with("Software\\TabThaiStartupTest-"));
            // This is a unique test key, not the real Run key; deletion is nonrecursive.
            let _ = RegDeleteKeyW(HKEY_CURRENT_USER, PCWSTR(self.0.as_ptr()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn executable_paths_are_quoted_and_invalid_paths_rejected() {
        let value = executable_value(Path::new("C:\\Program Files\\TabThai\\TabThai.exe")).unwrap();
        let text: Vec<_> = value
            .bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(
            String::from_utf16_lossy(&text),
            "\"C:\\Program Files\\TabThai\\TabThai.exe\"\0"
        );
        assert!(executable_value(Path::new("TabThai.exe")).is_err());
        assert!(executable_value(Path::new("C:\\bad\"name.exe")).is_err());
    }
    #[test]
    fn registration_can_enable_disable_and_detect_external_changes() {
        let registration = Registration::isolated().unwrap();
        assert_eq!(registration.read().unwrap(), None);
        registration.set_enabled(true, &None).unwrap();
        let saved = registration.read().unwrap();
        assert!(registration.is_enabled(&saved));
        assert!(registration.set_enabled(false, &None).is_err());
        registration.set_enabled(false, &saved).unwrap();
        assert_eq!(registration.read().unwrap(), None);
    }
    #[test]
    fn other_copies_are_not_removed_and_rollback_preserves_their_value() {
        let registration = Registration::isolated().unwrap();
        let other = Some(executable_value(Path::new("C:\\Other copy\\TabThai.exe")).unwrap());
        registration.write(&other).unwrap();
        assert!(!registration.is_enabled(&other));
        assert!(registration.set_enabled(false, &other).unwrap().is_none());
        assert_eq!(registration.read().unwrap(), other);
        let change = registration.set_enabled(true, &other).unwrap().unwrap();
        registration.rollback(change).unwrap();
        assert_eq!(registration.read().unwrap(), other);
    }
    #[test]
    fn rollback_does_not_overwrite_a_later_external_change() {
        let registration = Registration::isolated().unwrap();
        let change = registration.set_enabled(true, &None).unwrap().unwrap();
        let other = Some(executable_value(Path::new("C:\\New copy\\TabThai.exe")).unwrap());
        registration.write(&other).unwrap();
        assert!(registration.rollback(change).is_err());
        assert_eq!(registration.read().unwrap(), other);
    }
}
