// Tell the linker where mpv.lib is (Windows): the `lib/` folder in the project root.
// Can be overridden with the MPV_LIB_DIR environment variable.
fn main() {
    println!("cargo:rerun-if-env-changed=MPV_LIB_DIR");
    if let Ok(dir) = std::env::var("MPV_LIB_DIR") {
        println!("cargo:rustc-link-search=native={dir}");
    }
    let local = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("lib");
    if local.exists() {
        println!("cargo:rustc-link-search=native={}", local.display());
    }

    // exe icon (needs rc.exe from Visual Studio Build Tools; without it the exe just has no icon)
    println!("cargo:rerun-if-changed=assets/scloud.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/scloud.ico");
        if let Err(e) = res.compile() {
            println!("cargo:warning=exe icon was not embedded: {e}");
        }
    }
}
