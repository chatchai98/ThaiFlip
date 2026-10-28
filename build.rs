use std::{env, fs, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=assets/thaiflip.ico");
    println!("cargo:rerun-if-changed=assets/app.rc");
    if !env::var("TARGET").unwrap().ends_with("windows-msvc") {
        return;
    }
    let kits = PathBuf::from(env::var_os("ProgramFiles(x86)").expect("Windows SDK is required"))
        .join("Windows Kits/10/bin");
    let mut compilers: Vec<_> = fs::read_dir(kits)
        .expect("Install Windows SDK with C++ Build Tools")
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("x64/rc.exe"))
        .filter(|path| path.is_file())
        .collect();
    compilers.sort();
    let compiler = compilers
        .last()
        .expect("Windows SDK resource compiler rc.exe not found");
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let version = env::var("CARGO_PKG_VERSION").unwrap();
    let major = env::var("CARGO_PKG_VERSION_MAJOR").unwrap();
    let minor = env::var("CARGO_PKG_VERSION_MINOR").unwrap();
    let patch = env::var("CARGO_PKG_VERSION_PATCH").unwrap();
    fs::write(out.join("version.h"), format!(
        "#define APP_VERSION_COMMA {major},{minor},{patch},0\n#define APP_VERSION_STRING \"{version}\"\n"
    )).expect("Cannot write version resource header");
    let resource = out.join("thaiflip.res");
    let result = Command::new(compiler)
        .arg("/nologo")
        .arg("/I")
        .arg(&out)
        .arg(format!("/fo{}", resource.display()))
        .arg("assets/app.rc")
        .status()
        .expect("Cannot start resource compiler");
    assert!(result.success(), "Cannot compile application icon");
    println!("cargo:rustc-link-arg={}", resource.display());
}
