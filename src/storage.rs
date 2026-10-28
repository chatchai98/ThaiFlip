use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::Path,
};

pub fn write_atomic(path: &Path, content: &str) -> io::Result<()> {
    let temporary = path.with_extension(format!("txt.tmp-{}", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let result = (|| {
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}
