fn main() {
    println!("cargo:rerun-if-changed=src/platform/macos/Bridge.swift");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        let target = match std::env::var("CARGO_CFG_TARGET_ARCH").unwrap().as_str() {
            "aarch64" => "arm64-apple-macosx14.0",
            "x86_64" => "x86_64-apple-macosx14.0",
            arch => panic!("Unsupported macOS architecture: {arch}"),
        };
        let output =
            std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("macos-bridge");
        let status = std::process::Command::new("xcrun")
            .args([
                "swiftc",
                "-swift-version",
                "5",
                "-O",
                "-parse-as-library",
                "-target",
                target,
                "src/platform/macos/Bridge.swift",
                "-o",
            ])
            .arg(output)
            .status()
            .expect("macOS builds require Xcode Command Line Tools (xcrun swiftc)");
        assert!(
            status.success(),
            "Failed to compile the macOS native helper"
        );
        println!("cargo:rustc-env=MACOSX_DEPLOYMENT_TARGET=14.0");
    }
    println!("cargo:rerun-if-changed=assets/windows.manifest");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
    {
        let path = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap())
            .join("assets/windows.manifest");
        println!("cargo:rustc-link-arg-bin=remote-window-bridge=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg-bin=remote-window-bridge=/MANIFESTINPUT:{}",
            path.display()
        );
    }
}
