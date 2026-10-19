fn main() {
    println!("cargo:rerun-if-changed=assets/windows.manifest");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
    {
        let path = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap())
            .join("assets/windows.manifest");
        println!("cargo:rustc-link-arg-bin=airemotecontrol=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg-bin=airemotecontrol=/MANIFESTINPUT:{}",
            path.display()
        );
    }
}
