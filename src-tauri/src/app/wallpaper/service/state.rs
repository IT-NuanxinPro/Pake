use super::super::{cache, policy, RotationConfig, WallpaperItem};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, path::Path};

pub(super) const MAX_STATE_BYTES: usize = 8 * 1024 * 1024;

// Untagged identities read the original remote-item state without inventing URLs
// for older cached files whose source is no longer known.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub(super) enum ManagedWallpaper {
    Known(WallpaperItem),
    Local(LocalWallpaper),
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct LocalWallpaper {
    pub cache_id: String,
}

impl From<WallpaperItem> for ManagedWallpaper {
    fn from(item: WallpaperItem) -> Self {
        Self::Known(item)
    }
}

impl ManagedWallpaper {
    pub fn local(cache_id: String) -> Result<Self, String> {
        cache::validate_id(&cache_id)?;
        Ok(Self::Local(LocalWallpaper { cache_id }))
    }

    pub fn cache_id(&self) -> Result<String, String> {
        match self {
            Self::Known(item) => cache::filename(item),
            Self::Local(item) => {
                cache::validate_id(&item.cache_id)?;
                Ok(item.cache_id.clone())
            }
        }
    }

    pub fn source(&self) -> Option<&WallpaperItem> {
        match self {
            Self::Known(item) => Some(item),
            Self::Local(_) => None,
        }
    }
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub(super) struct CatalogueEntry {
    pub source: Option<WallpaperItem>,
    pub last_applied_at: Option<u64>,
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub(super) struct Persisted {
    pub config: RotationConfig,
    pub current: Option<ManagedWallpaper>,
    pub previous: Option<ManagedWallpaper>,
    pub next_due: u64,
    pub next_index: usize,
    pub last_error: Option<String>,
    pub possibly_active: Vec<ManagedWallpaper>,
    pub catalogue: BTreeMap<String, CatalogueEntry>,
}

impl Persisted {
    pub fn remember(
        &mut self,
        item: &ManagedWallpaper,
        applied_at: Option<u64>,
    ) -> Result<(), String> {
        let entry = self.catalogue.entry(item.cache_id()?).or_default();
        if let Some(source) = item.source() {
            entry.source = Some(source.clone());
        }
        if applied_at.is_some() {
            entry.last_applied_at = applied_at;
        }
        Ok(())
    }

    pub fn matches(item: &Option<ManagedWallpaper>, cache_id: &str) -> bool {
        item.as_ref()
            .is_some_and(|item| item.cache_id().as_deref() == Ok(cache_id))
    }

    pub fn in_rotation(&self, cache_id: &str) -> bool {
        self.config.enabled
            && self
                .config
                .items
                .iter()
                .any(|item| cache::filename(item).as_deref() == Ok(cache_id))
    }

    pub fn delete_reason(&self, cache_id: &str) -> Option<String> {
        if Self::matches(&self.current, cache_id) {
            Some("最近应用的壁纸受到保护".into())
        } else if Self::matches(&self.previous, cache_id) {
            Some("上一张壁纸受到保护".into())
        } else if self
            .possibly_active
            .iter()
            .any(|item| item.cache_id().as_deref() == Ok(cache_id))
        {
            Some("系统应用结果尚未确认，此壁纸可能仍在使用".into())
        } else if self.in_rotation(cache_id) {
            Some("壁纸正在轮换队列中，请先停止轮换".into())
        } else {
            None
        }
    }
}

pub(super) fn load(root: &Path) -> Result<Persisted, String> {
    cache::ensure_root(root)?;
    let path = root.join("state.json");
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Persisted::default())
        }
        Err(error) => return Err(format!("无法读取轮换设置：{error}")),
    };
    if !metadata.is_file() || metadata.len() > MAX_STATE_BYTES as u64 {
        return Err("轮换设置文件无效".into());
    }
    let mut state: Persisted = serde_json::from_slice(&fs::read(path).map_err(|e| e.to_string())?)
        .map_err(|_| "轮换设置损坏；本地图片已保留，管理功能只读")?;
    state.config = policy::validate_config(state.config)?;
    if state.possibly_active.len() > 50 {
        return Err("未确认的壁纸状态过多".into());
    }
    for (id, entry) in &state.catalogue {
        cache::validate_id(id)?;
        if let Some(source) = &entry.source {
            if cache::filename(source)? != *id {
                return Err("本地壁纸目录索引身份不一致".into());
            }
        }
    }
    let known: Vec<ManagedWallpaper> = state
        .config
        .items
        .iter()
        .cloned()
        .map(Into::into)
        .chain(state.possibly_active.iter().cloned())
        .chain(state.previous.iter().cloned())
        .chain(state.current.iter().cloned())
        .collect();
    for item in known {
        state.remember(&item, None)?;
    }
    if !state.config.enabled {
        state.config.items.clear();
    }
    Ok(state)
}

#[cfg(test)]
pub(super) fn protected(data: &Persisted) -> Vec<String> {
    data.current
        .iter()
        .chain(data.previous.iter())
        .chain(data.possibly_active.iter())
        .filter_map(|item| item.cache_id().ok())
        .collect()
}
