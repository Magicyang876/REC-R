//! Compile the app icon into the executable so Explorer shows it.
//!
//! Only the `*-pc-windows-gnu` target is handled: a COFF resource object built by
//! `windres` is linked straight into the binary. When windres is unavailable the
//! build still succeeds (the tray icon is embedded separately at runtime).

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn find_windres() -> Option<PathBuf> {
    let exe = if cfg!(windows) { "windres.exe" } else { "windres" };
    if let Some(path) = env::var_os("PATH") {
        for dir in env::split_paths(&path) {
            let candidate = dir.join(exe);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    let fallbacks = [
        r"C:\msys64\mingw64\bin\windres.exe",
        r"C:\ProgramData\chocolatey\bin\windres.exe",
    ];
    fallbacks
        .iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
        .or_else(|| {
            // scoop installs live under the user profile
            let home = env::var_os("USERPROFILE")?;
            let candidate =
                PathBuf::from(home).join(r"scoop\apps\mingw\current\bin\windres.exe");
            candidate.is_file().then_some(candidate)
        })
}

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.rc");
    println!("cargo:rerun-if-changed=assets/rec-r.ico");
    println!("cargo:rerun-if-changed=build.rs");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows")
        || env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("gnu")
    {
        return;
    }

    let Some(windres) = find_windres() else {
        println!("cargo:warning=windres not found: rec-r.exe will not carry the app icon");
        return;
    };

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let object = out_dir.join("rec-r-icon.o");
    let status = Command::new(&windres)
        .arg("--input")
        .arg(Path::new("assets").join("icon.rc"))
        .arg("--output")
        .arg(&object)
        .arg("--output-format")
        .arg("coff")
        .arg("--include-dir")
        .arg("assets")
        .status();
    match status {
        Ok(s) if s.success() => {
            println!("cargo:rustc-link-arg={}", object.display());
        }
        other => println!("cargo:warning=windres failed ({other:?}): no app icon in the exe"),
    }
}
