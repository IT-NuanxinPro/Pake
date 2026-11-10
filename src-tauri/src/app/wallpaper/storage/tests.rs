use super::*;
use std::time::{SystemTime, UNIX_EPOCH};

fn test_root() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "wallpaper-storage-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&path).unwrap();
    fs::canonicalize(path).unwrap()
}

fn plan(root: &Path) -> Storage {
    Storage::new(
        root.join("legacy"),
        root.join("metadata"),
        Ok(root.join("System Pictures Redirected")),
    )
}

fn seed(root: &Path, state: Option<&[u8]>) -> String {
    fs::create_dir_all(root).unwrap();
    let id = format!("{}.png", "a".repeat(64));
    image::RgbImage::from_pixel(2, 2, image::Rgb([12, 34, 56]))
        .save(root.join(&id))
        .unwrap();
    fs::write(root.join("unrelated.txt"), b"keep").unwrap();
    if let Some(state) = state {
        fs::write(root.join("state.json"), state).unwrap();
    }
    id
}

#[test]
fn first_upgrade_verifies_copies_and_separates_state_without_touching_old_files() {
    let root = test_root();
    let storage = plan(&root);
    let state = br#"{"config":{"enabled":true,"intervalMinutes":60,"items":[]},"catalogue":{}}"#;
    let id = seed(&storage.legacy_root, Some(state));
    #[cfg(unix)]
    let link = {
        let id = format!("{}.png", "b".repeat(64));
        std::os::unix::fs::symlink(
            storage.legacy_root.join("unrelated.txt"),
            storage.legacy_root.join(&id),
        )
        .unwrap();
        id
    };
    let resolved = storage.initialize();
    assert!(resolved.persistent && !resolved.read_only);
    assert_eq!(
        resolved.image_root,
        root.join("System Pictures Redirected/Wallpaper Gallery")
    );
    assert_eq!(resolved.state_root, root.join("metadata"));
    assert_eq!(
        fs::read(resolved.state_root.join("state.json")).unwrap(),
        state
    );
    assert_eq!(
        fs::read(resolved.image_root.join(&id)).unwrap(),
        fs::read(resolved.legacy_root.join(&id)).unwrap()
    );
    assert!(resolved.legacy_root.join(&id).exists());
    assert!(!resolved.image_root.join("state.json").exists());
    assert!(!resolved.image_root.join("unrelated.txt").exists());
    #[cfg(unix)]
    assert!(!resolved.image_root.join(link).exists());
    let restarted = plan(&root).initialize();
    assert_eq!(restarted.image_root, resolved.image_root);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn missing_empty_and_corrupt_old_state_are_preserved_exactly() {
    for state in [None, Some(b"".as_slice()), Some(b"corrupt".as_slice())] {
        let root = test_root();
        let storage = plan(&root);
        seed(&storage.legacy_root, state);
        let migrated = storage.initialize();
        assert!(migrated.persistent);
        assert_eq!(
            read_file(&migrated.state_root.join("state.json"), MAX_STATE_BYTES).unwrap(),
            state.map(Vec::from)
        );
        assert_eq!(
            read_file(&migrated.legacy_root.join("state.json"), MAX_STATE_BYTES).unwrap(),
            state.map(Vec::from)
        );
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn conflicting_destination_or_mid_copy_failure_never_switches_or_removes_originals() {
    let root = test_root();
    let storage = plan(&root);
    let id = seed(&storage.legacy_root, Some(b"{}"));
    let active = storage.initialize();
    let config = fs::read(active.metadata_root.join("storage.json")).unwrap();
    let destination = root.join("different/Wallpaper Gallery");
    fs::create_dir_all(&destination).unwrap();
    fs::write(destination.join(&id), b"conflicting image").unwrap();
    assert!(active.migrate(destination.parent().unwrap()).is_err());
    assert_eq!(
        fs::read(active.metadata_root.join("storage.json")).unwrap(),
        config
    );
    assert_eq!(
        fs::read(destination.join(&id)).unwrap(),
        b"conflicting image"
    );
    assert!(active.image_root.join(&id).exists() && active.legacy_root.join(&id).exists());
    fs::remove_dir_all(root).unwrap();

    // The first image copies successfully, then the second copy is rejected.
    let root = test_root();
    let source = root.join("source");
    let target = root.join("target");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&target).unwrap();
    let first = target.join("first.png");
    fs::write(target.join("second.png"), b"other").unwrap();
    {
        let mut copies = Copies::default();
        copies.copy(&first, b"first", 32).unwrap();
        assert!(copies
            .copy(&target.join("second.png"), b"second", 32)
            .is_err());
    }
    assert!(!first.exists());
    assert_eq!(fs::read(target.join("second.png")).unwrap(), b"other");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn config_is_the_only_commit_point_and_failed_bootstrap_can_restart_safely() {
    let root = test_root();
    let storage = plan(&root);
    let id = seed(&storage.legacy_root, Some(b"{\"lastError\":\"preserve\"}"));
    fs::create_dir_all(&storage.metadata_root).unwrap();
    // A conflicting pending path makes the final atomic config write fail
    // after all image and state copies were verified.
    fs::create_dir(storage.metadata_root.join("storage.pending")).unwrap();
    let failed = storage.clone().initialize();
    assert_eq!(failed.image_root, storage.legacy_root);
    assert!(failed.warning.is_some());
    assert!(!storage.metadata_root.join("state.json").exists());
    assert!(!root
        .join("System Pictures Redirected/Wallpaper Gallery")
        .join(&id)
        .exists());
    assert!(storage.legacy_root.join(&id).exists());
    fs::remove_dir(storage.metadata_root.join("storage.pending")).unwrap();
    let resumed = storage.initialize();
    assert!(resumed.persistent);
    assert_eq!(
        fs::read(resumed.state_root.join("state.json")).unwrap(),
        b"{\"lastError\":\"preserve\"}"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn partial_metadata_is_never_overwritten_and_invalid_committed_config_is_read_only() {
    let root = test_root();
    let storage = plan(&root);
    seed(&storage.legacy_root, Some(b"{}"));
    fs::create_dir_all(&storage.metadata_root).unwrap();
    fs::write(
        storage.metadata_root.join("state.json"),
        b"different older metadata",
    )
    .unwrap();
    let failed = storage.clone().initialize();
    assert_eq!(failed.image_root, storage.legacy_root);
    assert_eq!(
        fs::read(storage.metadata_root.join("state.json")).unwrap(),
        b"different older metadata"
    );
    fs::write(
        storage.metadata_root.join("storage.json"),
        b"invalid config",
    )
    .unwrap();
    let failed = storage.initialize();
    assert!(failed.read_only && failed.warning.is_some());
    let alternative = root.join("alternative");
    fs::create_dir(&alternative).unwrap();
    assert!(failed.migrate(&alternative).is_err());
    assert_eq!(
        fs::read(failed.metadata_root.join("storage.json")).unwrap(),
        b"invalid config"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn unsupported_committed_config_never_replaces_an_unknown_source_with_legacy_files() {
    let root = test_root();
    let storage = plan(&root);
    seed(&storage.legacy_root, Some(b"{}"));
    fs::create_dir_all(&storage.metadata_root).unwrap();
    let config = serde_json::to_vec(
        &serde_json::json!({"version":2,"imageRoot":root.join("unrecognized/Wallpaper Gallery")}),
    )
    .unwrap();
    fs::write(storage.metadata_root.join("storage.json"), &config).unwrap();
    let offline = storage.initialize();
    assert!(offline.read_only);
    let alternative = root.join("alternative");
    fs::create_dir(&alternative).unwrap();
    assert!(offline.migrate(&alternative).is_err());
    assert_eq!(
        fs::read(offline.metadata_root.join("storage.json")).unwrap(),
        config
    );
    assert!(!alternative.join(LIBRARY_NAME).exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn selected_parent_is_persisted_and_same_location_does_not_rewrite_config() {
    let root = test_root();
    let storage = plan(&root);
    seed(&storage.legacy_root, Some(b"{}"));
    let active = storage.initialize();
    let parent = root.join("User selected pictures");
    fs::create_dir(&parent).unwrap();
    let chosen = active.migrate(&parent).unwrap();
    assert_eq!(chosen.image_root, parent.join(LIBRARY_NAME));
    assert!(active.image_root.is_dir());
    let before = fs::metadata(chosen.metadata_root.join("storage.json"))
        .unwrap()
        .modified()
        .unwrap();
    chosen.migrate(&parent).unwrap();
    assert_eq!(
        fs::metadata(chosen.metadata_root.join("storage.json"))
            .unwrap()
            .modified()
            .unwrap(),
        before
    );
    let restarted = plan(&root).initialize();
    assert_eq!(restarted.image_root, chosen.image_root);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn roots_nested_internal_and_symlink_locations_are_rejected() {
    let root = test_root();
    let storage = plan(&root);
    seed(&storage.legacy_root, Some(b"{}"));
    let active = storage.initialize();
    for path in [
        Path::new("/"),
        active.metadata_root.as_path(),
        active.legacy_root.as_path(),
        active.image_root.as_path(),
        Path::new("relative"),
    ] {
        assert!(active.migrate(path).is_err(), "rejected {path:?}");
    }
    #[cfg(unix)]
    {
        let linked = root.join("linked");
        std::os::unix::fs::symlink(&active.image_root, &linked).unwrap();
        assert!(active.migrate(&linked).is_err());
        let parent = root.join("another");
        fs::create_dir(&parent).unwrap();
        std::os::unix::fs::symlink(&active.image_root, parent.join(LIBRARY_NAME)).unwrap();
        assert!(active.migrate(&parent).is_err());
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn unavailable_system_picture_directory_retains_legacy_with_a_recoverable_warning() {
    let root = test_root();
    let legacy = root.join("legacy");
    seed(&legacy, Some(b"{}"));
    let storage = Storage::new(
        legacy.clone(),
        root.join("meta"),
        Err("system directory unavailable".into()),
    )
    .initialize();
    assert_eq!(storage.image_root, legacy);
    assert!(storage.warning.is_some() && !storage.read_only);
    assert!(!storage.metadata_root.join("storage.json").exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn windows_comparison_preserves_redirected_and_unc_path_identity_across_case() {
    for (a, b) in [
        (
            r"\\?\C:\Users\Name\OneDrive\Pictures\Wallpaper Gallery",
            r"\\?\c:\users\name\onedrive\pictures\wallpaper gallery",
        ),
        (
            r"\\?\UNC\Server\Share\Photos",
            r"\\?\unc\server\share\photos",
        ),
    ] {
        assert_eq!(
            comparable(PathBuf::from(a), true),
            comparable(PathBuf::from(b), true)
        );
    }
}

#[test]
fn unavailable_committed_library_cannot_migrate_from_incomplete_legacy_source() {
    let root = test_root();
    let storage = plan(&root);
    let original = seed(&storage.legacy_root, Some(b"{}"));
    let initialized = storage.clone().initialize();
    let parent_a = root.join("custom-a");
    fs::create_dir(&parent_a).unwrap();
    let committed = initialized.migrate(&parent_a).unwrap();
    let extra = format!("{}.png", "b".repeat(64));
    image::RgbImage::from_pixel(2, 2, image::Rgb([98, 76, 54]))
        .save(committed.image_root.join(&extra))
        .unwrap();
    assert!(!committed.legacy_root.join(&extra).exists());
    let config = fs::read(committed.metadata_root.join("storage.json")).unwrap();
    let disconnected = root.join("disconnected-a");
    fs::rename(&parent_a, &disconnected).unwrap();
    let offline = storage.clone().initialize();
    let parent_c = root.join("custom-c");
    fs::create_dir(&parent_c).unwrap();
    assert!(
        offline.migrate(&parent_c).is_err(),
        "unavailable committed source must never fall back to an incomplete legacy library"
    );
    assert_eq!(offline.image_root, parent_a.join(LIBRARY_NAME));
    assert!(offline.read_only);
    assert_eq!(
        fs::read(committed.metadata_root.join("storage.json")).unwrap(),
        config
    );
    assert!(!parent_a.join(LIBRARY_NAME).exists());
    assert!(!parent_c.join(LIBRARY_NAME).exists());
    fs::rename(&disconnected, &parent_a).unwrap();
    let restored = offline.migrate(&parent_c).unwrap();
    assert!(restored.image_root.join(&original).is_file());
    assert!(restored.image_root.join(&extra).is_file());
    assert!(parent_a.join(LIBRARY_NAME).join(&extra).is_file());
    let restarted = storage.initialize();
    assert_eq!(restarted.image_root, parent_c.join(LIBRARY_NAME));
    fs::remove_dir_all(root).unwrap();
}
