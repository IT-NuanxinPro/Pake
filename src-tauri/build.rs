fn main() {
    println!("cargo:rerun-if-changed=.pake/pake.json");
    println!("cargo:rerun-if-changed=.pake/tauri.conf.json");
    let manifest = tauri_build::AppManifest::new().commands(&[
        "wallpaper_capabilities",
        "wallpaper_apply",
        "wallpaper_restore",
        "wallpaper_configure_rotation",
        "wallpaper_rotation_status",
        "wallpaper_cache_list",
        "wallpaper_cache_thumbnail",
        "wallpaper_cache_apply",
        "wallpaper_cache_reveal",
        "wallpaper_cache_delete",
        "wallpaper_cache_clear_unused",
        "wallpaper_storage_choose",
        "wallpaper_startup",
    ]);
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(manifest))
        .unwrap_or_else(|error| panic!("Tauri application build failed: {error}"));
}
