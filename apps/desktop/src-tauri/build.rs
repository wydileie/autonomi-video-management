fn main() {
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(
        tauri_build::AppManifest::new().commands(&[
            "desktop_setup_status",
            "desktop_save_setup",
            "desktop_start_stack",
            "desktop_open_in_browser",
        ]),
    ))
    .expect("failed to build Tauri app manifest");
}
