fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        static_vcruntime::metabuild();
        let icon = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap())
            .join("icons/icon.ico");
        winresource::WindowsResource::new()
            .set_icon(icon.to_str().unwrap())
            .set("FileDescription", "CongSpeak Native")
            .compile()
            .expect("Windows resources");
    }
}
