use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=assets/app.rc");
    println!("cargo:rerun-if-changed=assets/icon.ico");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows")
        || env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc")
    {
        return;
    }

    compile_windows_resource();
}

fn compile_windows_resource() {
    let rc_exe = find_rc_exe().unwrap_or_else(|| {
        panic!("Could not find rc.exe. Install the Windows SDK or add rc.exe to PATH.")
    });
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR should be set by Cargo"));
    let res_path = out_dir.join("app_icon.res");
    let status = Command::new(&rc_exe)
        .arg("/nologo")
        .arg(format!("/fo{}", res_path.display()))
        .arg("assets\\app.rc")
        .status()
        .unwrap_or_else(|error| panic!("Could not run {}: {error}", rc_exe.display()));

    if !status.success() {
        panic!("{} failed with status {status}", rc_exe.display());
    }

    println!("cargo:rustc-link-arg-bins={}", res_path.display());
}

fn find_rc_exe() -> Option<PathBuf> {
    env::var_os("RC")
        .map(PathBuf::from)
        .filter(|path| path.exists())
        .or_else(|| find_on_path("rc.exe"))
        .or_else(find_windows_sdk_rc)
}

fn find_on_path(file_name: &str) -> Option<PathBuf> {
    env::var_os("PATH")
        .into_iter()
        .flat_map(|path| env::split_paths(&path).collect::<Vec<_>>())
        .map(|path| path.join(file_name))
        .find(|path| path.exists())
}

fn find_windows_sdk_rc() -> Option<PathBuf> {
    let arch_dir = match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("x86") => "x86",
        Ok("aarch64") => "arm64",
        _ => "x64",
    };
    let base = env::var_os("ProgramFiles(x86)")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files (x86)"))
        .join("Windows Kits")
        .join("10")
        .join("bin");
    let mut candidates = Vec::new();

    if let Ok(entries) = fs::read_dir(&base) {
        for entry in entries.flatten() {
            let path = entry.path();

            if path.is_dir() {
                candidates.push(path.join(arch_dir).join("rc.exe"));
            }
        }
    }

    candidates.sort_by(|left, right| right.cmp(left));
    candidates.push(base.join(arch_dir).join("rc.exe"));
    candidates.into_iter().find(|path| path.exists())
}
