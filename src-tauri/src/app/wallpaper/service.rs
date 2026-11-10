use super::{
    blocking, cache,
    platform::{ApplyFailure, Backend},
    policy, RotationConfig, RotationStatus, WallpaperItem,
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::{Mutex, MutexGuard};

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
struct Persisted {
    config: RotationConfig,
    current: Option<WallpaperItem>,
    previous: Option<WallpaperItem>,
    next_due: u64,
    next_index: usize,
    last_error: Option<String>,
    possibly_active: Vec<WallpaperItem>,
}

struct Session {
    data: Persisted,
    backend: Backend,
}
pub struct Service {
    root: PathBuf,
    session: Mutex<Option<Session>>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn load(root: &Path) -> Result<Persisted, String> {
    fs::create_dir_all(root).map_err(|e| format!("无法创建壁纸缓存：{e}"))?;
    let path = root.join("state.json");
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Persisted::default())
        }
        Err(error) => return Err(format!("无法读取轮换设置：{error}")),
    };
    if !metadata.is_file() || metadata.len() > 1024 * 1024 {
        return Err("轮换设置文件无效".into());
    }
    let mut state: Persisted = serde_json::from_slice(&fs::read(path).map_err(|e| e.to_string())?)
        .map_err(|_| "轮换设置损坏；未自动启用")?;
    state.config = policy::validate_config(state.config)?;
    if !state.config.enabled {
        state.config.items.clear();
    }
    if state.possibly_active.len() > 50 {
        return Err("未确认的壁纸状态过多".into());
    }
    for item in [&state.current, &state.previous]
        .into_iter()
        .flatten()
        .chain(state.possibly_active.iter())
    {
        policy::validate_item(item)?;
    }
    Ok(state)
}

fn protected(data: &Persisted) -> Vec<String> {
    [&data.current, &data.previous]
        .into_iter()
        .flatten()
        .chain(data.possibly_active.iter())
        .filter_map(|item| cache::filename(item).ok())
        .collect()
}

async fn save(root: &Path, state: &Persisted) -> Result<(), String> {
    let root = root.to_owned();
    let bytes = serde_json::to_vec(state).map_err(|e| e.to_string())?;
    blocking(move || cache::atomic_write(&root.join("state.json"), &bytes)).await
}

fn previous(data: &Persisted) -> Result<WallpaperItem, String> {
    data.previous
        .clone()
        .ok_or("尚无可恢复的上一张已应用壁纸".into())
}

async fn apply_prepared(
    data: &mut Persisted,
    item: WallpaperItem,
    path: PathBuf,
    apply: impl FnOnce(&Path) -> Result<(), ApplyFailure> + Send + 'static,
) -> Result<(), String> {
    if let Err(error) = blocking(move || Ok(apply(&path))).await? {
        if error.may_have_changed && !data.possibly_active.contains(&item) {
            data.possibly_active.push(item);
        }
        return Err(error.message);
    }
    data.possibly_active.clear();
    if data.current.as_ref() != Some(&item) {
        data.previous = data.current.replace(item);
    }
    data.last_error = None;
    data.next_due = now().saturating_add(data.config.interval_minutes.saturating_mul(60));
    Ok(())
}

impl Service {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            session: Mutex::new(None),
        }
    }

    async fn session(&self) -> Result<MutexGuard<'_, Option<Session>>, String> {
        let mut guard = self.session.lock().await;
        if guard.is_none() {
            let root = self.root.clone();
            let (data, backend) = blocking(move || {
                let backend = Backend::detect();
                let data = match load(&root) {
                    Ok(data) => data,
                    Err(error) => {
                        eprintln!("[Pake] Wallpaper state: {error}");
                        Persisted {
                            last_error: Some(error),
                            ..Persisted::default()
                        }
                    }
                };
                fs::create_dir_all(&root).map_err(|e| e.to_string())?;
                cache::clear_interrupted_writes(&root)?;
                cache::reserve(&root, 0, &protected(&data))?;
                Ok((data, backend))
            })
            .await?;
            *guard = Some(Session { data, backend });
        }
        Ok(guard)
    }

    async fn status_for(&self, session: &Session) -> Result<RotationStatus, String> {
        let root = self.root.clone();
        let count = blocking(move || Ok(cache::entries(&root)?.len())).await?;
        Ok(RotationStatus {
            supported: session.backend.reason().is_none(),
            enabled: session.data.config.enabled,
            interval_minutes: session.data.config.interval_minutes,
            current: session.data.current.clone(),
            previous: session.data.previous.clone(),
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
        session.data.last_error = Some(error.clone());
        if let Err(write_error) = save(&self.root, &session.data).await {
            eprintln!("[Pake] Wallpaper state: {write_error}");
            return format!("{error}；{write_error}");
        }
        error
    }

    async fn apply_item(&self, session: &mut Session, item: WallpaperItem) -> Result<(), String> {
        if let Some(reason) = session.backend.reason() {
            return Err(reason);
        }
        if session.data.possibly_active.len() >= 50 && !session.data.possibly_active.contains(&item)
        {
            return Err("有过多未确认的系统壁纸状态；请重试已有图片以确认系统设置".into());
        }
        let path =
            cache::prepare(self.root.clone(), item.clone(), protected(&session.data)).await?;
        let backend = session.backend.clone();
        apply_prepared(&mut session.data, item, path, move |path| {
            backend.apply(path)
        })
        .await?;
        // Keep the successful OS outcome in memory even if persisting it fails.
        save(&self.root, &session.data).await
    }

    pub async fn apply(&self, item: WallpaperItem) -> Result<RotationStatus, String> {
        let mut guard = self.session().await?;
        let session = guard.as_mut().ok_or("壁纸服务未就绪")?;
        if let Err(error) = self.apply_item(session, item).await {
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
        next.last_error = None;
        save(&self.root, &next).await?;
        session.data = next;
        self.status_for(session).await
    }

    pub async fn tick(&self) -> Result<(), String> {
        let mut guard = self.session().await?;
        let session = guard.as_mut().ok_or("壁纸服务未就绪")?;
        if !session.data.config.enabled || session.data.next_due > now() {
            return Ok(());
        }
        let items = &session.data.config.items;
        if items.is_empty() {
            return Err("轮换列表为空".into());
        }
        let index = session.data.next_index % items.len();
        let item = items[index].clone();
        let count = items.len();
        match self.apply_item(session, item).await {
            Ok(()) => {
                session.data.next_index = (index + 1) % count;
                save(&self.root, &session.data).await
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
    fn failed_platform_operation_never_advances_current_or_previous() {
        tauri::async_runtime::block_on(async {
            let mut data = Persisted {
                current: Some(item("a")),
                previous: Some(item("b")),
                ..Persisted::default()
            };
            let result = apply_prepared(&mut data, item("c"), PathBuf::from("unused"), |_| {
                Err("fake OS failure".to_string().into())
            })
            .await;
            assert!(result.is_err());
            assert_eq!(data.current, Some(item("a")));
            assert_eq!(data.previous, Some(item("b")));
            apply_prepared(&mut data, item("c"), PathBuf::from("unused"), |_| Ok(()))
                .await
                .unwrap();
            assert_eq!(data.current, Some(item("c")));
            assert_eq!(previous(&data).unwrap(), item("a"));
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
                current: Some(item("a")),
                previous: Some(item("b")),
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
            assert_eq!(data.current, Some(item("a")));
            assert_eq!(data.previous, Some(item("b")));
            save(&root, &data).await.unwrap();
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
                root: root.clone(),
                session: Mutex::new(Some(Session {
                    backend: Backend::Unsupported("test backend".into()),
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
                root: root.clone(),
                session: Mutex::new(Some(Session {
                    backend: Backend::Unsupported("test backend".into()),
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
            current: Some(item("a")),
            ..Persisted::default()
        };
        data.current.as_mut().unwrap().url = "https://evil.test/a.jpg".into();
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
