use super::*;
use std::fs;

fn root() -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "wallpaper-manager-test-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&root).unwrap();
    root
}

fn item(name: &str) -> WallpaperItem {
    WallpaperItem {
        asset_key: format!("desktop:path:/wallpaper/desktop/{name}.png"),
        url: format!("https://wallpaper.061129.xyz/images/v1.2.3/wallpaper/desktop/{name}.png"),
        title: format!("已知标题{name}"),
    }
}

fn image(root: &Path, id: &str) {
    image::RgbImage::from_pixel(4, 3, image::Rgb([123, 100, 12]))
        .save(root.join(id))
        .unwrap();
}

fn service(root: PathBuf, data: Persisted) -> Service {
    Service {
        storage: Storage::legacy(root.clone()),
        session: Mutex::new(Some(Session {
            data,
            storage: Storage::legacy(root),
            backend: Backend::Test,
            read_only: None,
        })),
    }
}

#[test]
fn legacy_state_recovers_known_titles_and_unknown_files_stay_local() {
    tauri::async_runtime::block_on(async {
        let root = root();
        for name in ["current", "previous", "queue", "uncertain"] {
            image(&root, &cache::filename(&item(name)).unwrap());
        }
        let unknown = format!("{}.png", "f".repeat(64));
        image(&root, &unknown);
        let legacy = serde_json::json!({
            "config": { "enabled":true, "intervalMinutes":60, "items":[item("queue")] },
            "current":item("current"), "previous":item("previous"), "possiblyActive":[item("uncertain")]
        });
        fs::write(
            root.join("state.json"),
            serde_json::to_vec(&legacy).unwrap(),
        )
        .unwrap();
        let data = load(&root).unwrap();
        let service = service(root.clone(), data);
        let value = serde_json::to_value(service.cache_list().await.unwrap()).unwrap();
        assert_eq!(value["totalCount"], 5);
        assert_eq!(value["storagePath"], root.to_str().unwrap());
        for entry in value["entries"].as_array().unwrap() {
            let id = entry["cacheId"].as_str().unwrap();
            if id == unknown {
                assert_eq!(entry["titleKnown"], false);
                assert_eq!(entry["title"], "本地壁纸 · ffffffff");
                assert_eq!(entry["canApply"], true);
                assert!(entry["deleteBlockedReason"].is_null());
            } else {
                assert_eq!(entry["titleKnown"], true);
                assert!(entry["title"].as_str().unwrap().starts_with("已知标题"));
                assert!(entry["deleteBlockedReason"].is_string());
            }
            assert!(entry["modifiedAt"].is_u64());
            assert!(entry["lastAppliedAt"].is_null());
        }
        fs::remove_dir_all(root).unwrap();
    });
}

#[test]
fn orphan_local_apply_and_restore_persist_identity_without_url_or_download() {
    tauri::async_runtime::block_on(async {
        let root = root();
        let unknown = format!("{}.png", "f".repeat(64));
        image(&root, &unknown);
        let known = item("old");
        image(&root, &cache::filename(&known).unwrap());
        let mut data = Persisted {
            current: Some(known.clone().into()),
            ..Persisted::default()
        };
        data.remember(&known.clone().into(), None).unwrap();
        let service = service(root.clone(), data);
        let applied = service.cache_apply(unknown.clone()).await.unwrap();
        assert!(applied.status.current.is_none());
        assert_eq!(applied.status.previous, Some(known.clone()));
        let entry = applied
            .entries
            .iter()
            .find(|entry| entry.cache_id == unknown)
            .unwrap();
        assert!(entry.is_current && entry.delete_blocked_reason.is_some());
        assert!(entry.last_applied_at.unwrap() > 1_700_000_000_000);
        let persisted = serde_json::from_slice::<serde_json::Value>(
            &fs::read(root.join("state.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(persisted["current"], serde_json::json!({"cacheId":unknown}));
        assert_eq!(
            load(&root).unwrap().current.unwrap().cache_id().unwrap(),
            unknown
        );
        assert!(service.cache_delete(unknown.clone()).await.is_err());
        service.restore().await.unwrap();
        let restored = service.cache_list().await.unwrap();
        assert_eq!(restored.status.current, Some(known));
        assert!(restored.status.previous.is_none());
        assert!(
            restored
                .entries
                .iter()
                .find(|entry| entry.cache_id == unknown)
                .unwrap()
                .is_previous
        );
        // A vanished local file is an error; local apply cannot fetch its old URL.
        let known_id = cache::filename(&item("old")).unwrap();
        fs::remove_file(root.join(&known_id)).unwrap();
        assert!(service.cache_apply(known_id).await.is_err());
        fs::remove_dir_all(root).unwrap();
    });
}

#[test]
fn learning_source_and_updating_title_for_current_file_preserve_previous() {
    tauri::async_runtime::block_on(async {
        let root = root();
        let known_a = item("a");
        let id_a = cache::filename(&known_a).unwrap();
        let known_b = item("b");
        image(&root, &id_a);
        image(&root, &cache::filename(&known_b).unwrap());
        let service = service(
            root.clone(),
            Persisted {
                current: Some(known_b.clone().into()),
                ..Persisted::default()
            },
        );
        let applied = service.cache_apply(id_a.clone()).await.unwrap();
        assert!(applied.status.current.is_none());
        assert_eq!(applied.status.previous, Some(known_b.clone()));
        let upgraded = service.apply(known_a.clone()).await.unwrap();
        assert_eq!(upgraded.current, Some(known_a.clone()));
        assert_eq!(upgraded.previous, Some(known_b.clone()));
        let mut retitled = known_a;
        retitled.title = "新标题".into();
        let upgraded = service.apply(retitled.clone()).await.unwrap();
        assert_eq!(upgraded.current, Some(retitled));
        assert_eq!(upgraded.previous, Some(known_b.clone()));
        assert_eq!(service.restore().await.unwrap().current, Some(known_b));
        fs::remove_dir_all(root).unwrap();
    });
}

#[test]
fn uppercase_current_alias_cannot_reach_any_cache_file_operation() {
    tauri::async_runtime::block_on(async {
        let root = root();
        let known = item("current");
        let id = cache::filename(&known).unwrap();
        let upper = format!("{}.png", id[..64].to_ascii_uppercase());
        assert_ne!(id, upper);
        image(&root, &id);
        let service = service(
            root.clone(),
            Persisted {
                current: Some(known.into()),
                ..Persisted::default()
            },
        );
        assert!(service.cache_delete(upper.clone()).await.is_err());
        assert!(service.cache_apply(upper.clone()).await.is_err());
        assert!(service.cache_thumbnail(upper.clone()).await.is_err());
        // Invalid input is rejected before launching any system reveal command.
        assert!(service.cache_reveal(Some(upper)).await.is_err());
        assert!(root.join(&id).is_file());
        let list = service.cache_list().await.unwrap();
        assert_eq!(list.total_count, 1);
        assert_eq!(list.entries[0].cache_id, id);
        assert_eq!(service.cache_clear_unused().await.unwrap().removed_count, 0);
        fs::remove_dir_all(root).unwrap();
    });
}

#[test]
fn persistent_library_cancel_and_location_change_never_apply_or_delete_old_files() {
    tauri::async_runtime::block_on(async {
        let raw = root();
        let root = fs::canonicalize(&raw).unwrap();
        let legacy = root.join("legacy");
        fs::create_dir(&legacy).unwrap();
        let known = item("current");
        let id = cache::filename(&known).unwrap();
        image(&legacy, &id);
        let data = Persisted {
            current: Some(known.clone().into()),
            ..Persisted::default()
        };
        fs::write(
            legacy.join("state.json"),
            serde_json::to_vec(&data).unwrap(),
        )
        .unwrap();
        let original = Storage::new(legacy.clone(), root.join("meta"), Ok(root.join("Pictures")));
        let active = original.clone().initialize();
        let service = Service {
            storage: original,
            session: Mutex::new(Some(Session {
                data,
                storage: active.clone(),
                backend: Backend::Test,
                read_only: None,
            })),
        };
        let before = fs::read(active.state_root.join("storage.json")).unwrap();
        let cancelled = service.choose_storage(None).await.unwrap();
        assert!(cancelled.cancelled);
        assert_eq!(
            cancelled.library.storage_path,
            active.image_root.to_string_lossy()
        );
        assert_eq!(
            fs::read(active.state_root.join("storage.json")).unwrap(),
            before
        );
        let selected = root.join("selected");
        fs::create_dir(&selected).unwrap();
        let chosen = service
            .choose_storage(Some(selected.clone()))
            .await
            .unwrap();
        assert!(!chosen.cancelled);
        assert_eq!(
            chosen.library.storage_path,
            selected.join("Wallpaper Gallery").to_string_lossy()
        );
        assert_eq!(chosen.library.status.current, Some(known.clone()));
        assert!(legacy.join(&id).is_file() && active.image_root.join(&id).is_file());
        // Explicit local apply reads the new file path; no picker or OS backend
        // is executed in this test, and migration itself leaves history intact.
        assert_eq!(
            service
                .cache_apply(id.clone())
                .await
                .unwrap()
                .status
                .current,
            Some(known)
        );
        assert!(!selected.join("Wallpaper Gallery/state.json").exists());
        let restarted =
            Storage::new(legacy, root.join("meta"), Ok(root.join("Pictures"))).initialize();
        assert_eq!(restarted.image_root, selected.join("Wallpaper Gallery"));
        fs::remove_dir_all(raw).unwrap();
    });
}

#[test]
fn choosing_current_location_preserves_due_time_state_and_config_timestamps() {
    tauri::async_runtime::block_on(async {
        let raw = root();
        let root = fs::canonicalize(&raw).unwrap();
        let legacy = root.join("legacy");
        fs::create_dir(&legacy).unwrap();
        let queued = item("queue");
        image(&legacy, &cache::filename(&queued).unwrap());
        let data = Persisted {
            config: RotationConfig {
                enabled: true,
                interval_minutes: 1440,
                items: vec![queued],
            },
            next_due: now() + 1,
            ..Persisted::default()
        };
        fs::write(
            legacy.join("state.json"),
            serde_json::to_vec(&data).unwrap(),
        )
        .unwrap();
        let active =
            Storage::new(legacy, root.join("meta"), Ok(root.join("Pictures"))).initialize();
        let expected_due = data.next_due;
        let state_bytes = fs::read(active.state_root.join("state.json")).unwrap();
        let config_bytes = fs::read(active.state_root.join("storage.json")).unwrap();
        let state_time = fs::metadata(active.state_root.join("state.json"))
            .unwrap()
            .modified()
            .unwrap();
        let config_time = fs::metadata(active.state_root.join("storage.json"))
            .unwrap()
            .modified()
            .unwrap();
        let service = Service {
            storage: active.clone(),
            session: Mutex::new(Some(Session {
                data,
                storage: active.clone(),
                backend: Backend::Test,
                read_only: None,
            })),
        };
        let result = service
            .choose_storage(active.image_root.parent().map(Path::to_owned))
            .await
            .unwrap();
        assert!(!result.cancelled);
        assert_eq!(
            service.session.lock().await.as_ref().unwrap().data.next_due,
            expected_due
        );
        assert_eq!(
            fs::read(active.state_root.join("state.json")).unwrap(),
            state_bytes
        );
        assert_eq!(
            fs::read(active.state_root.join("storage.json")).unwrap(),
            config_bytes
        );
        assert_eq!(
            fs::metadata(active.state_root.join("state.json"))
                .unwrap()
                .modified()
                .unwrap(),
            state_time
        );
        assert_eq!(
            fs::metadata(active.state_root.join("storage.json"))
                .unwrap()
                .modified()
                .unwrap(),
            config_time
        );
        fs::remove_dir_all(raw).unwrap();
    });
}

#[test]
fn listing_unavailable_committed_library_never_recreates_source_or_allows_incomplete_migration() {
    tauri::async_runtime::block_on(async {
        let raw = root();
        let root = fs::canonicalize(&raw).unwrap();
        let legacy = root.join("legacy");
        fs::create_dir(&legacy).unwrap();
        let id = cache::filename(&item("original")).unwrap();
        image(&legacy, &id);
        fs::write(legacy.join("state.json"), b"{}").unwrap();
        let storage = Storage::new(legacy, root.join("meta"), Ok(root.join("Pictures")));
        let active = storage.clone().initialize();
        let parent_a = root.join("custom-a");
        fs::create_dir(&parent_a).unwrap();
        let active = active.migrate(&parent_a).unwrap();
        let extra = cache::filename(&item("new-only-in-a")).unwrap();
        image(&active.image_root, &extra);
        let config = fs::read(active.state_root.join("storage.json")).unwrap();
        let disconnected = root.join("disconnected-a");
        fs::rename(&parent_a, &disconnected).unwrap();
        let service = Service::new(storage);
        let offline = service.cache_list().await.unwrap();
        assert!(offline.read_only && offline.warning.is_some());
        assert_eq!(
            offline.storage_path,
            parent_a.join("Wallpaper Gallery").to_string_lossy()
        );
        assert_eq!(offline.total_count, 0);
        assert!(
            !parent_a.exists(),
            "listing cannot recreate a disconnected library as an empty source"
        );
        assert!(service.cache_thumbnail(id.clone()).await.is_err());
        assert!(!parent_a.exists());
        let parent_c = root.join("custom-c");
        fs::create_dir(&parent_c).unwrap();
        assert!(service
            .choose_storage(Some(parent_c.clone()))
            .await
            .is_err());
        assert_eq!(
            fs::read(active.state_root.join("storage.json")).unwrap(),
            config
        );
        fs::rename(&disconnected, &parent_a).unwrap();
        let recovered = service
            .choose_storage(Some(parent_c.clone()))
            .await
            .unwrap();
        assert_eq!(recovered.library.total_count, 2);
        assert!(parent_c.join("Wallpaper Gallery").join(extra).exists());
        assert!(parent_a.join("Wallpaper Gallery").join(id).exists());
        fs::remove_dir_all(raw).unwrap();
    });
}

#[cfg(unix)]
#[test]
fn symlink_replacement_of_committed_library_cannot_fall_back_to_incomplete_legacy_source() {
    tauri::async_runtime::block_on(async {
        let raw = root();
        let root = fs::canonicalize(&raw).unwrap();
        let legacy = root.join("legacy");
        fs::create_dir(&legacy).unwrap();
        let original = cache::filename(&item("original")).unwrap();
        image(&legacy, &original);
        fs::write(legacy.join("state.json"), b"{}").unwrap();
        let storage = Storage::new(legacy, root.join("meta"), Ok(root.join("Pictures")));
        let initialized = storage.clone().initialize();
        let parent_a = root.join("custom-a");
        fs::create_dir(&parent_a).unwrap();
        let active = initialized.migrate(&parent_a).unwrap();
        let extra = cache::filename(&item("new-only-in-a")).unwrap();
        image(&active.image_root, &extra);
        let config = fs::read(active.state_root.join("storage.json")).unwrap();
        let state = fs::read(active.state_root.join("state.json")).unwrap();
        let preserved = root.join("preserved-a");
        fs::rename(&parent_a, &preserved).unwrap();
        std::os::unix::fs::symlink(&preserved, &parent_a).unwrap();

        let service = Service::new(storage);
        let list = service.cache_list().await.unwrap();
        assert_eq!(
            list.storage_path,
            parent_a.join("Wallpaper Gallery").to_string_lossy()
        );
        assert!(list.read_only && list.warning.is_some());
        assert_eq!(list.total_count, 0);
        let parent_c = root.join("custom-c");
        fs::create_dir(&parent_c).unwrap();
        assert!(service
            .choose_storage(Some(parent_c.clone()))
            .await
            .is_err());
        assert_eq!(
            fs::read(active.state_root.join("storage.json")).unwrap(),
            config
        );
        assert_eq!(
            fs::read(active.state_root.join("state.json")).unwrap(),
            state
        );
        assert!(preserved.join("Wallpaper Gallery").join(extra).is_file());
        assert!(!parent_c.join("Wallpaper Gallery").exists());
        fs::remove_dir_all(raw).unwrap();
    });
}

#[test]
fn persistent_library_startup_never_automatically_evicts_images_over_old_cache_budget() {
    tauri::async_runtime::block_on(async {
        let raw = root();
        let root = fs::canonicalize(&raw).unwrap();
        let storage = Storage::new(
            root.join("legacy"),
            root.join("meta"),
            Ok(root.join("Pictures")),
        );
        let active = storage.clone().initialize();
        let ids: Vec<_> = ["a", "b"]
            .map(|hash| format!("{}.png", hash.repeat(64)))
            .into_iter()
            .collect();
        for id in &ids {
            fs::File::create(active.image_root.join(id))
                .unwrap()
                .set_len(200 * 1024 * 1024)
                .unwrap();
        }
        let service = Service::new(storage);
        assert_eq!(service.status().await.unwrap().cache_count, 2);
        let list = service.cache_list().await.unwrap();
        assert_eq!(list.total_bytes, 400 * 1024 * 1024);
        for id in ids {
            assert!(active.image_root.join(id).exists());
        }
        fs::remove_dir_all(raw).unwrap();
    });
}

#[test]
fn corrupt_legacy_state_migrates_bytes_then_remains_read_only_in_persistent_library() {
    tauri::async_runtime::block_on(async {
        let raw = root();
        let root = fs::canonicalize(&raw).unwrap();
        let legacy = root.join("legacy");
        fs::create_dir(&legacy).unwrap();
        let id = format!("{}.png", "a".repeat(64));
        image(&legacy, &id);
        fs::write(legacy.join("state.json"), b"corrupt source state").unwrap();
        let service = Service::new(Storage::new(
            legacy.clone(),
            root.join("meta"),
            Ok(root.join("Pictures")),
        ));
        let list = service.cache_list().await.unwrap();
        assert!(list.read_only && list.warning.is_some());
        assert_eq!(
            list.storage_path,
            root.join("Pictures/Wallpaper Gallery").to_string_lossy()
        );
        assert!(service.cache_delete(id.clone()).await.is_err());
        assert!(service.cache_clear_unused().await.is_err());
        assert!(service.cache_thumbnail(id).await.is_ok());
        assert_eq!(
            fs::read(root.join("meta/state.json")).unwrap(),
            b"corrupt source state"
        );
        assert_eq!(
            fs::read(legacy.join("state.json")).unwrap(),
            b"corrupt source state"
        );
        fs::remove_dir_all(raw).unwrap();
    });
}

#[test]
fn upgrading_an_active_rotation_defers_the_first_tick_without_applying() {
    tauri::async_runtime::block_on(async {
        let raw = root();
        let root = fs::canonicalize(&raw).unwrap();
        let legacy = root.join("legacy");
        fs::create_dir(&legacy).unwrap();
        let queued = item("queue");
        image(&legacy, &cache::filename(&queued).unwrap());
        let data = Persisted {
            config: RotationConfig {
                enabled: true,
                interval_minutes: 60,
                items: vec![queued],
            },
            ..Persisted::default()
        };
        let old_state = serde_json::to_vec(&data).unwrap();
        fs::write(legacy.join("state.json"), &old_state).unwrap();
        let service = Service::new(Storage::new(
            legacy.clone(),
            root.join("meta"),
            Ok(root.join("Pictures")),
        ));
        service.status().await.unwrap();
        {
            let mut guard = service.session.lock().await;
            let session = guard.as_mut().unwrap();
            assert!(
                session.data.next_due > now(),
                "migration must not trigger the first rotation immediately"
            );
            session.backend = Backend::Test;
        }
        service.tick().await.unwrap();
        assert!(service.status().await.unwrap().current.is_none());
        assert_eq!(fs::read(legacy.join("state.json")).unwrap(), old_state);
        assert_eq!(fs::read(root.join("meta/state.json")).unwrap(), old_state);
        fs::remove_dir_all(raw).unwrap();
    });
}

#[test]
fn eviction_prunes_catalogue_without_protecting_entire_rotation_queue() {
    tauri::async_runtime::block_on(async {
        let root = root();
        let active = item("current");
        let queued = item("queue");
        let active_id = cache::filename(&active).unwrap();
        let queued_id = cache::filename(&queued).unwrap();
        image(&root, &active_id);
        fs::File::create(root.join(&queued_id))
            .unwrap()
            .set_len(crate::app::wallpaper::policy::MAX_CACHE_BYTES)
            .unwrap();
        let mut data = Persisted {
            current: Some(active.clone().into()),
            config: RotationConfig {
                enabled: true,
                interval_minutes: 60,
                items: vec![queued.clone()],
            },
            ..Persisted::default()
        };
        data.remember(&active.into(), None).unwrap();
        data.remember(&queued.into(), None).unwrap();
        cache::reserve(&root, 1, &protected(&data)).unwrap();
        assert!(root.join(&active_id).exists());
        assert!(!root.join(&queued_id).exists());
        save(&Storage::legacy(root.clone()), &mut data)
            .await
            .unwrap();
        assert!(!data.catalogue.contains_key(&queued_id));
        let persisted: serde_json::Value =
            serde_json::from_slice(&fs::read(root.join("state.json")).unwrap()).unwrap();
        assert!(persisted["catalogue"].get(&queued_id).is_none());
        fs::remove_dir_all(root).unwrap();
    });
}

#[test]
fn bounded_state_can_persist_catalogue_with_a_full_valid_long_identity_queue() {
    tauri::async_runtime::block_on(async {
        let root = root();
        let items: Vec<_> = (0..50)
            .map(|index| {
                let mut source = item(&format!("{}-{index}", "a".repeat(7000)));
                source.title = format!("合法长路径 {index}");
                source
            })
            .collect();
        let mut data = Persisted {
            config: RotationConfig {
                enabled: true,
                interval_minutes: 60,
                items: items.clone(),
            },
            ..Persisted::default()
        };
        for item in items {
            image(&root, &cache::filename(&item).unwrap());
            data.remember(&item.into(), None).unwrap();
        }
        save(&Storage::legacy(root.clone()), &mut data)
            .await
            .unwrap();
        assert_eq!(load(&root).unwrap().catalogue.len(), 50);
        fs::remove_dir_all(root).unwrap();
    });
}

#[test]
fn delete_and_clear_unused_protect_every_reference_and_prune_metadata_only_for_removed_files() {
    tauri::async_runtime::block_on(async {
        let root = root();
        let ids: Vec<_> = [
            "current",
            "previous",
            "queue",
            "uncertain",
            "unused",
            "single",
        ]
        .map(|name| cache::filename(&item(name)).unwrap())
        .into_iter()
        .collect();
        for id in &ids {
            image(&root, id);
        }
        fs::write(root.join("notes.txt"), b"keep").unwrap();
        let mut data = Persisted {
            current: Some(item("current").into()),
            previous: Some(item("previous").into()),
            possibly_active: vec![item("uncertain").into()],
            config: RotationConfig {
                enabled: true,
                interval_minutes: 60,
                items: vec![item("queue")],
            },
            ..Persisted::default()
        };
        for name in [
            "current",
            "previous",
            "queue",
            "uncertain",
            "unused",
            "single",
        ] {
            data.remember(&item(name).into(), None).unwrap();
        }
        save(&Storage::legacy(root.clone()), &mut data)
            .await
            .unwrap();
        let state_before = fs::read(root.join("state.json")).unwrap();
        #[cfg(unix)]
        let symlink_id = {
            let id = format!("{}.png", "e".repeat(64));
            std::os::unix::fs::symlink(root.join(&ids[4]), root.join(&id)).unwrap();
            id
        };
        let service = service(root.clone(), data);
        for id in &ids[..4] {
            assert!(service.cache_delete(id.clone()).await.is_err());
        }
        assert_eq!(fs::read(root.join("state.json")).unwrap(), state_before);
        #[cfg(unix)]
        assert!(service.cache_delete(symlink_id.clone()).await.is_err());
        let result = service.cache_delete(ids[5].clone()).await.unwrap();
        assert_eq!(result.total_count, 5);
        assert!(!load(&root).unwrap().catalogue.contains_key(&ids[5]));
        let unused_bytes = fs::metadata(root.join(&ids[4])).unwrap().len();
        let result = service.cache_clear_unused().await.unwrap();
        assert_eq!(result.removed_count, 1);
        assert_eq!(result.freed_bytes, unused_bytes);
        assert_eq!(result.library.total_count, 4);
        assert!(root.join("notes.txt").exists() && root.join("state.json").exists());
        assert!(!load(&root).unwrap().catalogue.contains_key(&ids[4]));
        #[cfg(unix)]
        assert!(fs::symlink_metadata(root.join(symlink_id))
            .unwrap()
            .file_type()
            .is_symlink());
        service.configure(RotationConfig::default()).await.unwrap();
        assert!(service.cache_delete(ids[2].clone()).await.is_ok());
        fs::remove_dir_all(root).unwrap();
    });
}

#[test]
fn corrupt_state_is_previewable_but_all_mutations_fail_closed() {
    tauri::async_runtime::block_on(async {
        let root = root();
        let id = format!("{}.png", "a".repeat(64));
        image(&root, &id);
        fs::write(root.join("state.json"), b"corrupt").unwrap();
        fs::write(root.join("state.pending"), b"preserve evidence").unwrap();
        let service = Service::new(Storage::legacy(root.clone()));
        let list = service.cache_list().await.unwrap();
        assert!(list.read_only && list.warning.is_some());
        assert!(!list.entries[0].can_apply && list.entries[0].delete_blocked_reason.is_some());
        assert!(service.cache_thumbnail(id.clone()).await.is_ok());
        assert!(service.cache_apply(id.clone()).await.is_err());
        assert!(service.cache_delete(id).await.is_err());
        assert!(service.cache_clear_unused().await.is_err());
        assert!(service.apply(item("remote")).await.is_err());
        assert!(service.restore().await.is_err());
        assert!(service.configure(RotationConfig::default()).await.is_err());
        service.tick().await.unwrap();
        assert_eq!(fs::read(root.join("state.json")).unwrap(), b"corrupt");
        assert!(root.join("state.pending").exists());
        fs::remove_dir_all(root).unwrap();
    });
}
