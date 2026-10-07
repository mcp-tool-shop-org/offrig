//! Embeds the application icon as the Windows executable's resource.
//! A no-op on every other target, so Linux CI builds need no resource compiler.

fn main() {
    println!("cargo:rerun-if-changed=assets/icon/offrig.ico");
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(windows)]
    embed_icon();
}

#[cfg(windows)]
fn embed_icon() {
    // The build script runs on the host; only embed when the target is Windows too.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/icon/offrig.ico");
    if let Err(e) = res.compile() {
        panic!("could not embed the application icon: {e}");
    }
}
