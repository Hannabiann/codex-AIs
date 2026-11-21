use std::{env, fs, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=assets/codex-AIs.ico");
    println!("cargo:rerun-if-env-changed=RC");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let icon =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("assets/codex-AIs.ico");
    let version = env::var("CARGO_PKG_VERSION").unwrap();
    let numbers = version
        .split('.')
        .chain(std::iter::repeat("0"))
        .take(4)
        .collect::<Vec<_>>()
        .join(",");
    let script = out.join("codex-AIs.rc");
    fs::write(
        &script,
        format!(
            r#"1 ICON "{}"
1 VERSIONINFO
 FILEVERSION {numbers}
 PRODUCTVERSION {numbers}
 FILEOS 0x40004
 FILETYPE 1
BEGIN
 BLOCK "StringFileInfo"
 BEGIN
  BLOCK "040904B0"
  BEGIN
   VALUE "FileDescription", "codex-AIs"
   VALUE "ProductName", "codex-AIs"
   VALUE "FileVersion", "{version}"
   VALUE "ProductVersion", "{version}"
   VALUE "OriginalFilename", "codex-AIs.exe"
   VALUE "LegalCopyright", "GPL-3.0-only"
  END
 END
 BLOCK "VarFileInfo"
 BEGIN
  VALUE "Translation", 0x409, 1200
 END
END
"#,
            icon.to_string_lossy().replace('\\', "/")
        ),
    )
    .unwrap();
    let msvc = env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    let resource = out.join(if msvc { "codex-AIs.res" } else { "codex-AIs.o" });
    let compiler = env::var_os("RC").map(PathBuf::from).unwrap_or_else(|| {
        if !msvc {
            return PathBuf::from("windres");
        }
        if Command::new("rc.exe").arg("/?").output().is_ok() {
            return PathBuf::from("rc.exe");
        }
        let root = env::var_os("WindowsSdkDir")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(
                    env::var_os("ProgramFiles(x86)").expect("Windows SDK or RC must be configured"),
                )
                .join("Windows Kits/10")
            });
        let mut versions = fs::read_dir(root.join("bin"))
            .expect("Install Windows SDK with Visual Studio Build Tools")
            .filter_map(Result::ok)
            .map(|e| e.path().join("x64/rc.exe"))
            .filter(|p| p.is_file())
            .collect::<Vec<_>>();
        versions.sort();
        versions.pop().expect("Windows SDK rc.exe missing; set RC")
    });
    let mut command = Command::new(compiler);
    if msvc {
        command
            .arg("/nologo")
            .arg("/fo")
            .arg(&resource)
            .arg(&script);
    } else {
        command.arg("-i").arg(&script).arg("-o").arg(&resource);
    }
    assert!(
        command
            .status()
            .expect("Could not run resource compiler")
            .success(),
        "Windows resource compilation failed"
    );
    println!("cargo:rustc-link-arg-bin=codex-AIs={}", resource.display());
}
