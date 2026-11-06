fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut resource = winres::WindowsResource::new();
        resource.set_manifest_file("app.manifest");
        println!("cargo:rerun-if-changed=app.manifest");
        if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("gnu")
            && std::env::var("HOST").unwrap_or_default().contains("linux")
        {
            resource.set_windres_path("x86_64-w64-mingw32-windres");
        }
        resource
            .compile()
            .expect("failed to embed Windows visual-styles manifest");
        if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("gnu") {
            println!(
                "cargo:rustc-link-arg-bins={}/resource.o",
                std::env::var("OUT_DIR").expect("Cargo did not provide OUT_DIR")
            );
        }
    }
}
