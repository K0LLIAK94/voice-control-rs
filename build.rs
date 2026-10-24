use std::{env, fs, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=assets/icon-idle.ico");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let icon = root.join("assets/icon-idle.ico");
    if !icon.is_file() {
        panic!("Missing assets/icon-idle.ico");
    }
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let resource = output.join("voice-control.rc");
    let compiled = output.join("voice-control.res");
    let icon = icon.to_string_lossy().replace('\\', "/");
    fs::write(
        &resource,
        format!(
            r#"1 ICON "{icon}"
1 VERSIONINFO
 FILEVERSION 0,1,0,0
 PRODUCTVERSION 0,1,0,0
 FILEFLAGSMASK 0x3fL
 FILEFLAGS 0x0L
 FILEOS 0x40004L
 FILETYPE 0x1L
BEGIN
 BLOCK "StringFileInfo"
 BEGIN
  BLOCK "040904b0"
  BEGIN
   VALUE "FileDescription", "Voice Control"
   VALUE "ProductName", "Voice Control"
   VALUE "FileVersion", "0.1.0"
   VALUE "ProductVersion", "0.1.0"
  END
 END
 BLOCK "VarFileInfo"
 BEGIN
  VALUE "Translation", 0x409, 1200
 END
END
"#
        ),
    )
    .unwrap();
    if env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("gnu") {
        let compiled = output.join("voice-control.o");
        let windres = env::var_os("WINDRES").unwrap_or_else(|| "windres.exe".into());
        let status = Command::new(windres)
            .arg("-i")
            .arg(&resource)
            .args(["-O", "coff", "-o"])
            .arg(&compiled)
            .status()
            .expect("MinGW windres.exe not found; add the MinGW bin directory to PATH");
        assert!(status.success(), "GNU resource compiler failed");
        println!("cargo:rustc-link-arg-bins={}", compiled.display());
        return;
    }
    let rc=env::var_os("RC").map(PathBuf::from).or_else(|| {
        Command::new("where.exe").arg("rc.exe").output().ok().filter(|r|r.status.success())
            .and_then(|r|String::from_utf8(r.stdout).ok()).and_then(|s|s.lines().next().map(PathBuf::from))
    }).or_else(||{
        let base=PathBuf::from(env::var_os("ProgramFiles(x86)")?).join("Windows Kits/10/bin");
        let mut versions:Vec<_>=fs::read_dir(base).ok()?.flatten().map(|e|e.path().join("x64/rc.exe")).filter(|p|p.is_file()).collect();
        versions.sort();versions.pop()
    }).expect("Windows SDK resource compiler rc.exe not found; install Visual Studio C++ Build Tools / Windows SDK");
    let status = Command::new(rc)
        .args(["/nologo", "/fo"])
        .arg(&compiled)
        .arg(&resource)
        .status()
        .expect("Could not start rc.exe");
    assert!(status.success(), "Resource compiler failed");
    println!("cargo:rustc-link-arg-bins={}", compiled.display());
}
