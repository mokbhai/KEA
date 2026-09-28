fn main() {
    // ScreenCaptureKit's Swift bridge loads `@rpath/libswift_Concurrency.dylib`;
    // without the OS Swift runtime on the rpath the app aborts at launch.
    // A dependency's `rustc-link-arg` does not reach this binary, so it is
    // repeated here (see crates/platform/build.rs).
    if std::env::var_os("CARGO_FEATURE_SYSTEM_AUDIO_SCK").is_some()
        && std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos")
    {
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    }
    tauri_build::build();
}
