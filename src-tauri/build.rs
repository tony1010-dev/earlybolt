fn main() {
    // Type-checking the Windows code from a Mac (EARLYBOLT_CHECK_ONLY=1) skips the Windows resource step, which needs llvm-rc
    let windows = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    if windows && std::env::var("EARLYBOLT_CHECK_ONLY").is_ok() {
        println!("cargo:rustc-check-cfg=cfg(desktop)");
        println!("cargo:rustc-check-cfg=cfg(mobile)");
        println!("cargo:rustc-cfg=desktop");
        println!("cargo:rustc-env=TAURI_ENV_TARGET_TRIPLE=x86_64-pc-windows-msvc");
        return;
    }
    tauri_build::build()
}
