use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=ui/app.slint");
    println!("cargo:rerun-if-changed=Cargo.toml");
    let configuration = slint_build::CompilerConfiguration::default().with_style("fluent".into());
    slint_build::compile_with_config("ui/app.slint", configuration)
        .expect("failed to compile the Slint UI");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
    {
        embed_windows_resources();
    }
}

fn embed_windows_resources() {
    println!("cargo:rerun-if-changed=assets/icons/rust-vlc.ico");
    for name in [
        "RC",
        "WindowsSdkDir",
        "WindowsSDKVersion",
        "WindowsSdkVerBinPath",
        "WindowsSdkBinPath",
    ] {
        println!("cargo:rerun-if-env-changed={name}");
    }

    let package_version = env::var("CARGO_PKG_VERSION").expect("Cargo package version is set");
    let major = env::var("CARGO_PKG_VERSION_MAJOR").expect("Cargo package major version is set");
    let minor = env::var("CARGO_PKG_VERSION_MINOR").expect("Cargo package minor version is set");
    let patch = env::var("CARGO_PKG_VERSION_PATCH").expect("Cargo package patch version is set");
    let icon =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("Cargo manifest directory is set"))
            .join("assets/icons/rust-vlc.ico");
    assert!(
        icon.is_file(),
        "Windows application icon is missing: {}",
        icon.display()
    );

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo output directory is set"));
    let resource_script = out_dir.join("rust-vlc.rc");
    let resource_file = out_dir.join("rust-vlc.res");
    fs::copy(&icon, out_dir.join("rust-vlc.ico")).expect("failed to copy Windows application icon");
    let source = format!(
        r#"1 ICON "rust-vlc.ico"

1 VERSIONINFO
 FILEVERSION {major},{minor},{patch},0
 PRODUCTVERSION {major},{minor},{patch},0
 FILEFLAGSMASK 0x3fL
 FILEFLAGS 0x0L
 FILEOS 0x00040004L
 FILETYPE 0x00000001L
 FILESUBTYPE 0x0L
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "Rust-VLC\0"
      VALUE "FileDescription", "Rust-VLC\0"
      VALUE "FileVersion", "{package_version}\0"
      VALUE "InternalName", "rust-vlc\0"
      VALUE "OriginalFilename", "rust-vlc.exe\0"
      VALUE "ProductName", "Rust-VLC\0"
      VALUE "ProductVersion", "{package_version}\0"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x0409, 1200
  END
END
"#
    );
    assert!(source.contains(&format!("FILEVERSION {major},{minor},{patch},0")));
    fs::write(&resource_script, source).expect("failed to write Windows resource script");

    let compiler = find_resource_compiler();
    let status = Command::new(&compiler)
        .arg("/nologo")
        .arg("/fo")
        .arg("rust-vlc.res")
        .arg("rust-vlc.rc")
        .current_dir(&out_dir)
        .status()
        .unwrap_or_else(|error| {
            panic!(
                "failed to run Windows resource compiler {}: {error}",
                compiler.display()
            )
        });
    assert!(
        status.success(),
        "Windows resource compiler failed with {status}"
    );
    assert!(
        resource_file.is_file(),
        "Windows resource compiler did not produce a .res file"
    );

    // The MSVC linker merges .res inputs into the executable's PE resources.
    println!(
        "cargo:rustc-link-arg-bin=rust-vlc={}",
        resource_file.display()
    );
}

fn find_resource_compiler() -> PathBuf {
    if let Some(path) = env::var_os("RC") {
        return PathBuf::from(path);
    }

    if Command::new("rc.exe").arg("/?").output().is_ok() {
        return PathBuf::from("rc.exe");
    }

    let mut candidates = Vec::new();
    for name in ["WindowsSdkVerBinPath", "WindowsSdkBinPath"] {
        if let Some(path) = env::var_os(name) {
            let path = PathBuf::from(path);
            candidates.push(path.join("x64/rc.exe"));
            candidates.push(path.join("rc.exe"));
        }
    }
    if let Some(sdk_dir) = env::var_os("WindowsSdkDir") {
        let sdk_dir = PathBuf::from(sdk_dir);
        if let Some(version) = env::var_os("WindowsSDKVersion") {
            candidates.push(sdk_dir.join("bin").join(version).join("x64/rc.exe"));
        }
        candidates.push(sdk_dir.join("bin/x64/rc.exe"));
    }
    if let Some(program_files) = env::var_os("ProgramFiles(x86)") {
        let sdk_bin = PathBuf::from(program_files).join("Windows Kits/10/bin");
        if let Ok(entries) = fs::read_dir(&sdk_bin) {
            let mut versions: Vec<_> = entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .collect();
            versions.sort();
            for version in versions.into_iter().rev() {
                candidates.push(version.join("x64/rc.exe"));
            }
        }
        candidates.push(sdk_bin.join("x64/rc.exe"));
    }

    candidates
        .into_iter()
        .find(|path| path.is_file())
        .unwrap_or_else(|| {
            panic!(
                "Windows MSVC builds require rc.exe from the Windows SDK; add it to PATH or set RC to its path"
            )
        })
}
