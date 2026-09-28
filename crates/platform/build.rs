fn main() {
    // The `screencapturekit` crate links a Swift bridge against
    // `@rpath/libswift_Concurrency.dylib`, and nothing adds the OS Swift
    // runtime to the rpath, so the test and probe binaries abort at load.
    // `rustc-link-arg` only reaches this package's own binaries; `kea-app`
    // repeats it in its build script.
    if std::env::var_os("CARGO_FEATURE_SYSTEM_AUDIO_SCK").is_some()
        && std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos")
    {
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    }
}
