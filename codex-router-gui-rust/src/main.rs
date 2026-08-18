#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

// This bin is self-contained and does not use the `codex_router_lib` crate,
// so rustc will not propagate the native resource library the lib carries.
// Link the icon/manifest/version resource library directly (the build script
// places it on the native search path) or this executable would ship without
// any Windows version resource.
#[cfg(windows)]
#[link(name = "codex-router.res", kind = "static")]
extern "C" {}

#[cfg(windows)]
include!("windows_main.rs");

#[cfg(not(windows))]
fn main() {
    let version = env!("CARGO_PKG_VERSION");
    println!("CodexRouter v{version}");
    println!("The current supported runtime is Windows 10/11 x64.");
    std::process::exit(2);
}
