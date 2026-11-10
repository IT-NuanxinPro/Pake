fn main() {
    println!("cargo:rerun-if-changed=.pake/pake.json");
    println!("cargo:rerun-if-changed=.pake/tauri.conf.json");
    let manifest = tauri_build::AppManifest::new().commands(&[
        "wallpaper_capabilities",
        "wallpaper_apply",
        "wallpaper_restore",
        "wallpaper_configure_rotation",
        "wallpaper_rotation_status",
    ]);
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(manifest))
        .unwrap_or_else(|error| panic!("Tauri application build failed: {error}"));
}
