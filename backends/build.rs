use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn emit_optional_include(name: &str, path: Option<PathBuf>, output: &mut String) {
    match path {
        Some(path) if path.exists() => {
            let abs = path.canonicalize().unwrap_or(path);
            println!("cargo:rerun-if-changed={}", abs.display());
            output.push_str(&format!(
                "#[allow(dead_code)]\npub const {}: Option<&'static [u8]> = Some(include_bytes!(r#\"{}\"#));\n",
                name,
                abs.display()
            ));
        },
        Some(path) => {
            println!("cargo:warning=embedded asset missing for {}: {}", name, path.display());
            output.push_str(&format!(
                "#[allow(dead_code)]\npub const {}: Option<&'static [u8]> = None;\n",
                name
            ));
        },
        None => {
            output.push_str(&format!(
                "#[allow(dead_code)]\npub const {}: Option<&'static [u8]> = None;\n",
                name
            ));
        },
    }
}

fn target_adb_path(manifest_dir: &Path, target_os: &str, target_arch: &str) -> Option<PathBuf> {
    let rel = match (target_os, target_arch) {
        ("macos", "aarch64") => "binaries/adb-aarch64-apple-darwin",
        ("macos", "x86_64") => "binaries/adb-x86_64-apple-darwin",
        ("windows", "x86_64") => "binaries/adb-x86_64-pc-windows-msvc.exe",
        _ => return None,
    };
    Some(manifest_dir.join(rel))
}

fn main() {
    tauri_build::build();

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("missing manifest dir"));
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("missing out dir"));
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();

    let mut output = String::new();
    emit_optional_include(
        "EMBEDDED_ADB_BYTES",
        target_adb_path(&manifest_dir, &target_os, &target_arch),
        &mut output,
    );
    emit_optional_include(
        "EMBEDDED_SCRCPY_SERVER_BYTES",
        Some(manifest_dir.join("resources/scrcpy-server")),
        &mut output,
    );
    emit_optional_include(
        "EMBEDDED_ADB_WIN_API_BYTES",
        (target_os == "windows").then(|| manifest_dir.join("resources/windows/AdbWinApi.dll")),
        &mut output,
    );
    emit_optional_include(
        "EMBEDDED_ADB_WIN_USB_BYTES",
        (target_os == "windows").then(|| manifest_dir.join("resources/windows/AdbWinUsbApi.dll")),
        &mut output,
    );

    fs::write(out_dir.join("embedded_assets.rs"), output).expect("write embedded assets");
}
