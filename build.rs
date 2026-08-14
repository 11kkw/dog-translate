fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        // ScreenCaptureKit's Swift bridge references libswift_Concurrency via
        // @rpath. A dependency build script cannot reliably add an rpath to
        // this final executable, so add the system Swift runtime explicitly.
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    }
}
