use super::{apply_prepared, blocking, save, state::ManagedWallpaper, Persisted, Service, Session};
use crate::app::wallpaper::{
    cache,
    library::{self, CacheEntry, CacheSnapshot, ClearUnusedResult, StorageChoice},
};
use std::{fs, time::UNIX_EPOCH};

impl Service {
    pub async fn choose_storage(
        &self,
        parent: Option<std::path::PathBuf>,
    ) -> Result<StorageChoice, String> {
        let mut guard = self.session().await?;
        let session = guard.as_mut().ok_or("壁纸服务未就绪")?;
        let cancelled = parent.is_none();
        if let Some(parent) = parent {
            if let Some(reason) = &session.read_only {
                return Err(format!("壁纸状态只读，原有图片已保留：{reason}"));
            }
            let storage = session.storage.clone();
            let previous_root = storage.image_root.clone();
            session.storage = blocking(move || storage.migrate(&parent)).await?;
            // Copying paths never applies a wallpaper. A running rotation waits
            // for its next full interval before it can use the new location.
            if session.storage.image_root != previous_root {
                session.data.next_due = super::now()
                    .saturating_add(session.data.config.interval_minutes.saturating_mul(60));
            }
        }
        Ok(StorageChoice {
            cancelled,
            library: self.library_for(session).await?,
        })
    }

    async fn library_for(&self, session: &Session) -> Result<CacheSnapshot, String> {
        let root = session.storage.image_root.clone();
        let storage = session.storage.clone();
        let files = blocking(move || {
            storage
                .entries()?
                .into_iter()
                .map(|(path, bytes, modified)| {
                    let id = path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .ok_or("本地壁纸编号无效")?
                        .to_owned();
                    let valid = library::image_path(&root, &id).is_ok();
                    Ok((id, bytes, modified, valid))
                })
                .collect::<Result<Vec<_>, String>>()
        })
        .await?;
        let mut entries = Vec::with_capacity(files.len());
        for (id, bytes, modified, valid) in files {
            let metadata = session.data.catalogue.get(&id);
            let source = metadata.and_then(|metadata| metadata.source.as_ref());
            let title_known = source.is_some_and(|source| !source.title.trim().is_empty());
            let title = source
                .filter(|_| title_known)
                .map(|source| source.title.clone())
                .unwrap_or_else(|| format!("本地壁纸 · {}", &id[..8]));
            entries.push(CacheEntry {
                title,
                title_known,
                bytes,
                modified_at: (modified != UNIX_EPOCH)
                    .then_some(modified)
                    .and_then(|modified| {
                        modified
                            .duration_since(UNIX_EPOCH)
                            .ok()
                            .and_then(|time| u64::try_from(time.as_millis()).ok())
                    }),
                last_applied_at: metadata.and_then(|metadata| metadata.last_applied_at),
                is_current: Persisted::matches(&session.data.current, &id),
                is_previous: Persisted::matches(&session.data.previous, &id),
                in_rotation: session.data.in_rotation(&id),
                possibly_active: session
                    .data
                    .possibly_active
                    .iter()
                    .any(|item| item.cache_id().as_deref() == Ok(id.as_str())),
                can_apply: valid
                    && session.read_only.is_none()
                    && !session.storage.read_only
                    && session.backend.reason().is_none(),
                delete_blocked_reason: session
                    .read_only
                    .as_ref()
                    .map(|_| "壁纸状态损坏，暂时禁止删除以保留现有图片".into())
                    .or_else(|| {
                        session
                            .storage
                            .read_only
                            .then(|| "保存目录配置不可用，暂时禁止删除".into())
                    })
                    .or_else(|| session.data.delete_reason(&id)),
                cache_id: id,
            });
        }
        entries.sort_by(|a, b| {
            b.last_applied_at
                .cmp(&a.last_applied_at)
                .then_with(|| b.modified_at.cmp(&a.modified_at))
                .then_with(|| a.cache_id.cmp(&b.cache_id))
        });
        Ok(CacheSnapshot {
            storage_path: session.storage.image_root.to_string_lossy().into_owned(),
            total_bytes: entries.iter().map(|entry| entry.bytes).sum(),
            total_count: entries.len(),
            read_only: session.read_only.is_some() || session.storage.read_only,
            warning: session
                .read_only
                .clone()
                .or_else(|| session.storage.warning.clone()),
            status: self.status_for(session).await?,
            entries,
        })
    }

    pub async fn cache_list(&self) -> Result<CacheSnapshot, String> {
        let guard = self.session().await?;
        self.library_for(guard.as_ref().ok_or("壁纸服务未就绪")?)
            .await
    }

    pub async fn cache_thumbnail(&self, cache_id: String) -> Result<Vec<u8>, String> {
        let guard = self.session().await?;
        let root = guard
            .as_ref()
            .ok_or("壁纸服务未就绪")?
            .storage
            .image_root
            .clone();
        blocking(move || library::thumbnail(&root, &cache_id)).await
    }

    pub async fn cache_reveal(&self, cache_id: Option<String>) -> Result<(), String> {
        let guard = self.session().await?;
        let root = guard
            .as_ref()
            .ok_or("壁纸服务未就绪")?
            .storage
            .image_root
            .clone();
        blocking(move || library::reveal(&root, cache_id.as_deref())).await
    }

    pub async fn cache_apply(&self, cache_id: String) -> Result<CacheSnapshot, String> {
        let mut guard = self.session().await?;
        let session = guard.as_mut().ok_or("壁纸服务未就绪")?;
        session.require_writable()?;
        if let Some(reason) = session.backend.reason() {
            return Err(reason);
        }
        let root = session.storage.image_root.clone();
        let id = cache_id.clone();
        let path = blocking(move || library::image_path(&root, &id)).await?;
        let item = match session
            .data
            .catalogue
            .get(&cache_id)
            .and_then(|entry| entry.source.clone())
        {
            Some(source) => ManagedWallpaper::Known(source),
            None => ManagedWallpaper::local(cache_id)?,
        };
        if session.data.possibly_active.len() >= 50 && !session.data.possibly_active.contains(&item)
        {
            return Err("有过多未确认的系统壁纸状态；请重试已有图片以确认系统设置".into());
        }
        let backend = session.backend.clone();
        if let Err(error) = apply_prepared(&mut session.data, item, path, move |path| {
            backend.apply(path)
        })
        .await
        {
            return Err(self.record_error(session, error).await);
        }
        save(&session.storage, &mut session.data).await?;
        self.library_for(session).await
    }

    pub async fn cache_delete(&self, cache_id: String) -> Result<CacheSnapshot, String> {
        let mut guard = self.session().await?;
        let session = guard.as_mut().ok_or("壁纸服务未就绪")?;
        session.require_writable()?;
        cache::validate_id(&cache_id)?;
        if let Some(reason) = session.data.delete_reason(&cache_id) {
            return Err(reason);
        }
        let root = session.storage.image_root.clone();
        let id = cache_id.clone();
        blocking(move || {
            let path = library::file_path(&root, &id)?;
            fs::remove_file(path).map_err(|e| format!("删除本地壁纸失败：{e}"))
        })
        .await?;
        session.data.catalogue.remove(&cache_id);
        save(&session.storage, &mut session.data).await?;
        self.library_for(session).await
    }

    pub async fn cache_clear_unused(&self) -> Result<ClearUnusedResult, String> {
        let mut guard = self.session().await?;
        let session = guard.as_mut().ok_or("壁纸服务未就绪")?;
        session.require_writable()?;
        let root = session.storage.image_root.clone();
        let mut protected: Vec<_> = session
            .data
            .current
            .iter()
            .chain(session.data.previous.iter())
            .chain(session.data.possibly_active.iter())
            .filter_map(|item| item.cache_id().ok())
            .collect();
        protected.extend(
            session
                .data
                .config
                .items
                .iter()
                .filter_map(|item| cache::filename(item).ok()),
        );
        let removed = blocking(move || {
            let mut removed = Vec::new();
            let mut failure = None;
            for (path, bytes, _) in cache::entries(&root)? {
                let Some(id) = path.file_name().and_then(|name| name.to_str()) else {
                    continue;
                };
                if protected.iter().any(|name| name == id) {
                    continue;
                }
                let result = library::file_path(&root, id).and_then(|path| {
                    fs::remove_file(path).map_err(|e| format!("清理本地壁纸失败：{e}"))
                });
                match result {
                    Ok(()) => removed.push((id.to_owned(), bytes)),
                    Err(error) => {
                        failure = Some(error);
                        break;
                    }
                }
            }
            Ok((removed, failure))
        })
        .await?;
        for (id, _) in &removed.0 {
            session.data.catalogue.remove(id);
        }
        save(&session.storage, &mut session.data).await?;
        if let Some(error) = removed.1 {
            return Err(error);
        }
        Ok(ClearUnusedResult {
            removed_count: removed.0.len(),
            freed_bytes: removed.0.iter().map(|(_, bytes)| *bytes).sum(),
            library: self.library_for(session).await?,
        })
    }
}
