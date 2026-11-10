use super::{
    blocking, cache,
    platform::{ApplyFailure, Backend},
    policy,
    storage::Storage,
    RotationConfig, RotationStatus, WallpaperItem,
};
use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::{Mutex, MutexGuard};

mod library;
#[cfg(test)]
mod library_tests;
mod state;
#[cfg(test)]
use state::protected;
use state::{load, ManagedWallpaper, Persisted};

struct Session {
    data: Persisted,
    storage: Storage,
    backend: Backend,
    read_only: Option<String>,
}

impl Session {
    fn require_writable(&self) -> Result<(), String> {
        if self.storage.read_only {
            return Err(self
                .storage
                .warning
                .clone()
                .unwrap_or_else(|| "保存目录配置不可用，请更改位置".into()));
        }
        match &self.read_only {
            Some(warning) => Err(format!("本地壁纸管理只读：{warning}")),
            None => Ok(()),
        }
    }
}
pub struct Service {
    storage: Storage,
    session: Mutex<Option<Session>>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

async fn save(storage: &Storage, state: &mut Persisted) -> Result<(), String> {
    let scan_root = storage.image_root.clone();
    let ids = blocking(move || {
        Ok(cache::entries(&scan_root)?
            .into_iter()
            .filter_map(|(path, _, _)| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .map(str::to_owned)
            })
            .collect::<std::collections::HashSet<_>>())
    })
    .await?;
    state.catalogue.retain(|id, _| ids.contains(id));
    let root = storage.state_root.clone();
    let bytes = serde_json::to_vec(state).map_err(|e| e.to_string())?;
    if bytes.len() > state::MAX_STATE_BYTES {
        return Err("壁纸目录索引超过安全大小，设置未写入".into());
    }
    blocking(move || cache::atomic_write(&root.join("state.json"), &bytes)).await
}

fn previous(data: &Persisted) -> Result<ManagedWallpaper, String> {
    data.previous
        .clone()
        .ok_or("尚无可恢复的上一张已应用壁纸".into())
}

async fn apply_prepared(
    data: &mut Persisted,
    item: impl Into<ManagedWallpaper>,
    path: PathBuf,
    apply: impl FnOnce(&Path) -> Result<(), ApplyFailure> + Send + 'static,
) -> Result<(), String> {
    let item = item.into();
    let same_current = Persisted::matches(&data.current, &item.cache_id()?);
    let item = match &data.current {
        Some(current) if same_current && item.source().is_none() => current.clone(),
        _ => item,
    };
    if let Err(error) = blocking(move || Ok(apply(&path))).await? {
        if error.may_have_changed && !data.possibly_active.contains(&item) {
            data.possibly_active.push(item);
        }
        return Err(error.message);
    }
    data.remember(
        &item,
        Some(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX),
        ),
    )?;
    data.possibly_active.clear();
    if !same_current {
        data.previous = data.current.take();
    }
    data.current = Some(item);
    data.last_error = None;
    data.next_due = now().saturating_add(data.config.interval_minutes.saturating_mul(60));
    Ok(())
}

impl Service {
    pub fn new(storage: Storage) -> Self {
        Self {
            storage,
            session: Mutex::new(None),
        }
    }

    async fn session(&self) -> Result<MutexGuard<'_, Option<Session>>, String> {
        let mut guard = self.session.lock().await;
        if guard.is_none() {
            let storage = self.storage.clone();
            let (data, backend, read_only, storage) = blocking(move || {
                let storage = storage.initialize();
                let backend = Backend::detect();
                let (mut data, read_only) = match load(&storage.state_root) {
                    Ok(data) => (data, None),
                    Err(error) => {
                        eprintln!("[Pake] Wallpaper state: {error}");
                        (
                            Persisted {
                                last_error: Some(error.clone()),
                                ..Persisted::default()
                            },
                            Some(error),
                        )
                    }
                };
                if storage.migrated {
                    data.next_due =
                        now().saturating_add(data.config.interval_minutes.saturating_mul(60));
                }
                Ok((data, backend, read_only, storage))
            })
            .await?;
            *guard = Some(Session {
                data,
                backend,
                storage,
                read_only,
            });
        }
        Ok(guard)
    }

    async fn status_for(&self, session: &Session) -> Result<RotationStatus, String> {
        let storage = session.storage.clone();
        let count = blocking(move || Ok(storage.entries()?.len())).await?;
        Ok(RotationStatus {
            supported: session.backend.reason().is_none(),
            enabled: session.data.config.enabled,
            interval_minutes: session.data.config.interval_minutes,
            current: session
                .data
                .current
                .as_ref()
                .and_then(|item| item.source().cloned()),
            previous: session
                .data
                .previous
                .as_ref()
                .and_then(|item| item.source().cloned()),
            last_error: session.data.last_error.clone(),
            cache_count: count,
        })
    }

    pub async fn reason(&self) -> Result<Option<String>, String> {
        let guard = self.session().await?;
        Ok(guard.as_ref().ok_or("壁纸服务未就绪")?.backend.reason())
    }

    pub async fn status(&self) -> Result<RotationStatus, String> {
        let guard = self.session().await?;
        self.status_for(guard.as_ref().ok_or("壁纸服务未就绪")?)
            .await
    }

    async fn record_error(&self, session: &mut Session, error: String) -> String {
        if session.read_only.is_some() || session.storage.read_only {
            return error;
        }
        session.data.last_error = Some(error.clone());
        if let Err(write_error) = save(&session.storage, &mut session.data).await {
            eprintln!("[Pake] Wallpaper state: {write_error}");
            return format!("{error}；{write_error}");
        }
        error
    }

    async fn apply_item(
        &self,
        session: &mut Session,
        item: ManagedWallpaper,
    ) -> Result<(), String> {
        session.require_writable()?;
        if let Some(reason) = session.backend.reason() {
            return Err(reason);
        }
        if session.data.possibly_active.len() >= 50 && !session.data.possibly_active.contains(&item)
        {
            return Err("有过多未确认的系统壁纸状态；请重试已有图片以确认系统设置".into());
        }
        let path = match item.source() {
            Some(source) => {
                cache::prepare(session.storage.image_root.clone(), source.clone()).await?
            }
            None => {
                let root = session.storage.image_root.clone();
                let id = item.cache_id()?;
                blocking(move || super::library::image_path(&root, &id)).await?
            }
        };
        session.data.remember(&item, None)?;
        let backend = session.backend.clone();
        apply_prepared(&mut session.data, item, path, move |path| {
            backend.apply(path)
        })
        .await?;
        // Keep the successful OS outcome in memory even if persisting it fails.
        save(&session.storage, &mut session.data).await
    }

    pub async fn apply(&self, item: WallpaperItem) -> Result<RotationStatus, String> {
        let mut guard = self.session().await?;
        let session = guard.as_mut().ok_or("壁纸服务未就绪")?;
        if let Err(error) = self.apply_item(session, item.into()).await {
            return Err(self.record_error(session, error).await);
        }
        self.status_for(session).await
    }

    pub async fn restore(&self) -> Result<RotationStatus, String> {
        let mut guard = self.session().await?;
        let session = guard.as_mut().ok_or("壁纸服务未就绪")?;
        let item = match previous(&session.data) {
            Ok(item) => item,
            Err(error) => return Err(self.record_error(session, error).await),
        };
        if let Err(error) = self.apply_item(session, item).await {
            return Err(self.record_error(session, error).await);
        }
        self.status_for(session).await
    }

    pub async fn configure(&self, config: RotationConfig) -> Result<RotationStatus, String> {
        let mut guard = self.session().await?;
        let session = guard.as_mut().ok_or("壁纸服务未就绪")?;
        session.require_writable()?;
        let config = match policy::validate_config(config) {
            Ok(config) => config,
            Err(error) => return Err(self.record_error(session, error).await),
        };
        let mut next = session.data.clone();
        if config.enabled {
            if let Some(reason) = session.backend.reason() {
                return Err(self.record_error(session, reason).await);
            }
            next.config = config;
            next.next_index = 0;
            // Enabling is explicit opt-in. The first tick runs immediately.
            next.next_due = now();
        } else {
            next.config.enabled = false;
            next.config.items.clear();
            next.next_due = 0;
        }
        for item in next.config.items.clone() {
            next.remember(&item.into(), None)?;
        }
        next.last_error = None;
        save(&session.storage, &mut next).await?;
        session.data = next;
        self.status_for(session).await
    }

    pub async fn tick(&self) -> Result<(), String> {
        let mut guard = self.session().await?;
        let session = guard.as_mut().ok_or("壁纸服务未就绪")?;
        if session.read_only.is_some()
            || session.storage.read_only
            || !session.data.config.enabled
            || session.data.next_due > now()
        {
            return Ok(());
        }
        let items = &session.data.config.items;
        if items.is_empty() {
            return Err("轮换列表为空".into());
        }
        let index = session.data.next_index % items.len();
        let item = items[index].clone();
        let count = items.len();
        match self.apply_item(session, item.into()).await {
            Ok(()) => {
                session.data.next_index = (index + 1) % count;
                save(&session.storage, &mut session.data).await
            }
            Err(error) => {
                // Do not hammer a failed OS prompt or download every polling cycle.
                session.data.next_due =
                    now().saturating_add(session.data.config.interval_minutes.saturating_mul(60));
                session.data.next_index = (index + 1) % count;
                Err(self.record_error(session, error).await)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    fn item(name: &str) -> WallpaperItem {
        WallpaperItem {
            asset_key: format!("desktop:path:/wallpaper/desktop/{name}.jpg"),
            url: format!("https://wallpaper.061129.xyz/images/v1.2.3/wallpaper/desktop/{name}.jpg"),
            title: name.into(),
        }
    }
    #[test]
    fn restore_requires_a_previous_successful_managed_image() {
        assert!(previous(&Persisted::default()).is_err());
    }
    #[test]
    fn reapplying_local_identity_never_discards_known_source_or_previous() {
        tauri::async_runtime::block_on(async {
            let known = item("a");
            let local = ManagedWallpaper::local(cache::filename(&known).unwrap()).unwrap();
            let mut data = Persisted {
                current: Some(known.clone().into()),
                previous: Some(item("b").into()),
                ..Persisted::default()
            };
            apply_prepared(&mut data, local, PathBuf::from("fake"), |_| Ok(()))
                .await
                .unwrap();
            assert_eq!(data.current, Some(known.clone().into()));
            assert_eq!(data.previous, Some(item("b").into()));
            assert_eq!(
                data.catalogue[&cache::filename(&known).unwrap()].source,
                Some(known)
            );
        });
    }
    #[test]
    fn corrupt_state_never_evicts_existing_wallpapers() {
        tauri::async_runtime::block_on(async {
            let root = test_root();
            fs::write(root.join("state.json"), b"broken json").unwrap();
            let image = root.join(format!("{}.jpg", "a".repeat(64)));
            fs::File::create(&image)
                .unwrap()
                .set_len(policy::MAX_CACHE_BYTES + 1)
                .unwrap();
            let service = Service::new(Storage::legacy(root.clone()));
            service.status().await.unwrap();
            assert!(
                image.exists(),
                "untrusted state must not discard any cached image"
            );
            assert!(service.configure(RotationConfig::default()).await.is_err());
            assert_eq!(fs::read(root.join("state.json")).unwrap(), b"broken json");
            fs::remove_dir_all(root).unwrap();
        });
    }
    #[test]
    fn failed_platform_operation_never_advances_current_or_previous() {
        tauri::async_runtime::block_on(async {
            let mut data = Persisted {
                current: Some(item("a").into()),
                previous: Some(item("b").into()),
                ..Persisted::default()
            };
            let result = apply_prepared(&mut data, item("c"), PathBuf::from("unused"), |_| {
                Err("fake OS failure".to_string().into())
            })
            .await;
            assert!(result.is_err());
            assert_eq!(data.current, Some(item("a").into()));
            assert_eq!(data.previous, Some(item("b").into()));
            apply_prepared(&mut data, item("c"), PathBuf::from("unused"), |_| Ok(()))
                .await
                .unwrap();
            assert_eq!(data.current, Some(item("c").into()));
            assert_eq!(previous(&data).unwrap(), item("a").into());
        });
    }
    fn test_root() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "wallpaper-state-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }
    #[test]
    fn rollback_failure_persists_cache_protection_without_claiming_success() {
        tauri::async_runtime::block_on(async {
            let root = test_root();
            let mut data = Persisted {
                current: Some(item("a").into()),
                previous: Some(item("b").into()),
                ..Persisted::default()
            };
            let failed = item("c");
            assert!(
                apply_prepared(&mut data, failed.clone(), PathBuf::from("unused"), |_| Err(
                    ApplyFailure {
                        message: "rollback failed".into(),
                        may_have_changed: true
                    }
                ))
                .await
                .is_err()
            );
            assert_eq!(data.current, Some(item("a").into()));
            assert_eq!(data.previous, Some(item("b").into()));
            save(&Storage::legacy(root.clone()), &mut data)
                .await
                .unwrap();
            let mut loaded = load(&root).unwrap();
            let active_name = cache::filename(&failed).unwrap();
            assert!(protected(&loaded).contains(&active_name));
            fs::write(root.join(&active_name), b"protected").unwrap();
            let stale = root.join(format!("{}.jpg", "0".repeat(64)));
            fs::File::create(&stale)
                .unwrap()
                .set_len(policy::MAX_CACHE_BYTES)
                .unwrap();
            cache::reserve(&root, 1, &protected(&loaded)).unwrap();
            assert!(root.join(active_name).exists());
            assert!(!stale.exists());
            apply_prepared(&mut loaded, item("d"), PathBuf::from("unused"), |_| Ok(()))
                .await
                .unwrap();
            assert!(loaded.possibly_active.is_empty());
            fs::remove_dir_all(root).unwrap();
        });
    }
    #[test]
    fn disabled_config_persists_without_valid_queue_and_never_calls_platform() {
        tauri::async_runtime::block_on(async {
            let root = test_root();
            let service = Service {
                storage: Storage::legacy(root.clone()),
                session: Mutex::new(Some(Session {
                    storage: Storage::legacy(root.clone()),
                    backend: Backend::Unsupported("test backend".into()),
                    read_only: None,
                    data: Persisted {
                        config: RotationConfig {
                            enabled: true,
                            interval_minutes: 30,
                            items: vec![item("a")],
                        },
                        ..Persisted::default()
                    },
                })),
            };
            let disabled = service
                .configure(RotationConfig {
                    enabled: false,
                    interval_minutes: 0,
                    items: vec![],
                })
                .await
                .unwrap();
            assert!(!disabled.enabled);
            assert_eq!(disabled.interval_minutes, 30);
            service.tick().await.unwrap();
            let loaded = load(&root).unwrap();
            assert!(!loaded.config.enabled);
            assert!(loaded.config.items.is_empty());
            fs::remove_dir_all(root).unwrap();
        });
    }
    #[test]
    fn invalid_queue_preserves_existing_config_and_records_error() {
        tauri::async_runtime::block_on(async {
            let root = test_root();
            let service = Service {
                storage: Storage::legacy(root.clone()),
                session: Mutex::new(Some(Session {
                    storage: Storage::legacy(root.clone()),
                    backend: Backend::Unsupported("test backend".into()),
                    read_only: None,
                    data: Persisted {
                        config: RotationConfig {
                            enabled: true,
                            interval_minutes: 30,
                            items: vec![item("a")],
                        },
                        ..Persisted::default()
                    },
                })),
            };
            assert!(service
                .configure(RotationConfig {
                    enabled: true,
                    interval_minutes: 1,
                    items: vec![]
                })
                .await
                .is_err());
            let loaded = load(&root).unwrap();
            assert!(loaded.config.enabled);
            assert_eq!(loaded.config.interval_minutes, 30);
            assert_eq!(loaded.config.items, vec![item("a")]);
            assert!(loaded.last_error.is_some());
            fs::remove_dir_all(root).unwrap();
        });
    }
    #[test]
    fn persisted_invalid_identity_is_rejected_before_background_work() {
        let root = test_root();
        let mut data = Persisted {
            current: Some(item("a").into()),
            ..Persisted::default()
        };
        if let Some(ManagedWallpaper::Known(item)) = data.current.as_mut() {
            item.url = "https://evil.test/a.jpg".into();
        }
        cache::atomic_write(
            &root.join("state.json"),
            &serde_json::to_vec(&data).unwrap(),
        )
        .unwrap();
        assert!(load(&root).is_err());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn dto_uses_exact_camel_case_contract() {
        let data = serde_json::to_value(RotationStatus {
            supported: true,
            enabled: false,
            interval_minutes: 60,
            current: Some(item("a")),
            previous: None,
            last_error: None,
            cache_count: 1,
        })
        .unwrap();
        assert_eq!(data["intervalMinutes"], 60);
        assert_eq!(data["cacheCount"], 1);
        assert!(data["current"]["assetKey"].is_string());
        assert!(data.get("lastError").is_some());
    }
}
