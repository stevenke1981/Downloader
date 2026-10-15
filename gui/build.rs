fn main() {
    let config = slint_build::CompilerConfiguration::new()
        // Element metadata keeps geometry/accessibility regression tests available
        // in the same release profile used for distribution and CI screenshots.
        .with_debug_info(true)
        .with_style("fluent-light".into())
        .with_bundled_translations("lang")
        .with_default_translation_context(slint_build::DefaultTranslationContext::None);
    slint_build::compile_with_config("ui/app.slint", config).expect("Slint build failed");
    println!("cargo:rerun-if-changed=lang");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=assets/downloader.ico");
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/downloader.ico")
            .set("ProductName", "Downloader")
            .set("FileDescription", "Downloader - download accelerator")
            .set("LegalCopyright", "MIT License");
        res.compile().expect("failed to embed Windows resources");
    }
}
