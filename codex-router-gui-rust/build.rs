fn main() {
    if cfg!(target_os = "windows") {
        windres::Build::new().compile("assets/icon.rc").unwrap();
    }
}
