use super::{cache, policy};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

const LIBRARY_NAME: &str = "Wallpaper Gallery";
const MAX_STATE_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Clone)]
pub(super) struct Storage {
    pub image_root: PathBuf,
    pub state_root: PathBuf,
    pub warning: Option<String>,
    pub read_only: bool,
    pub migrated: bool,
    metadata_root: PathBuf,
    legacy_root: PathBuf,
    default_parent: Option<PathBuf>,
    persistent: bool,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Location {
    version: u8,
    image_root: PathBuf,
}

fn safe_path(path: &Path) -> Result<(), String> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err("壁纸保存目录必须为系统提供的绝对路径".into());
    }
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err("壁纸保存目录不能包含符号链接".into())
            }
            Ok(_) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(format!("无法验证壁纸保存目录：{error}")),
        }
    }
    Ok(())
}

fn directory(path: &Path) -> Result<(), String> {
    safe_path(path)?;
    fs::create_dir_all(path).map_err(|e| format!("无法创建壁纸保存目录：{e}"))?;
    if !fs::symlink_metadata(path)
        .map_err(|e| e.to_string())?
        .is_dir()
    {
        return Err("壁纸保存位置不是文件夹".into());
    }
    Ok(())
}

fn comparable(path: PathBuf, case_insensitive: bool) -> PathBuf {
    if case_insensitive {
        PathBuf::from(path.to_string_lossy().to_lowercase())
    } else {
        path
    }
}

fn identity(path: &Path) -> Result<PathBuf, String> {
    safe_path(path)?;
    let mut existing = path;
    let mut suffix = Vec::new();
    while !existing.exists() {
        suffix.push(existing.file_name().ok_or("无法验证目录身份")?.to_owned());
        existing = existing.parent().ok_or("无法验证目录父级")?;
    }
    let mut resolved =
        fs::canonicalize(existing).map_err(|e| format!("无法解析目录真实位置：{e}"))?;
    for part in suffix.iter().rev() {
        resolved.push(part);
    }
    Ok(comparable(resolved, cfg!(windows)))
}

fn overlaps(a: &Path, b: &Path) -> Result<bool, String> {
    let a = identity(a)?;
    let b = identity(b)?;
    Ok(a.starts_with(&b) || b.starts_with(&a))
}

fn read_file(path: &Path, limit: u64) -> Result<Option<Vec<u8>>, String> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("无法读取壁纸存储文件：{error}")),
    };
    if !meta.is_file() || meta.len() > limit {
        return Err("壁纸存储文件无效或超过安全大小".into());
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err("壁纸存储文件超过安全大小".into());
    }
    Ok(Some(bytes))
}

// Only files created by this transaction are rolled back. Existing matching
// files, original libraries, and unrelated user files are never removed.
#[derive(Default)]
struct Copies {
    created: Vec<PathBuf>,
    committed: bool,
}

impl Copies {
    fn copy(&mut self, path: &Path, bytes: &[u8], limit: u64) -> Result<(), String> {
        if let Some(existing) = read_file(path, limit)? {
            if Sha256::digest(&existing) != Sha256::digest(bytes) {
                return Err("目标目录存在同名但内容不同的壁纸或状态，已保留原目录".into());
            }
            return Ok(());
        }
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|e| format!("无法复制壁纸文件：{e}"))?;
        self.created.push(path.to_owned());
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|e| format!("复制壁纸失败：{e}"))?;
        drop(file);
        let written = read_file(path, limit)?.ok_or("复制后的壁纸文件丢失")?;
        if Sha256::digest(&written) != Sha256::digest(bytes) {
            return Err("壁纸复制校验失败，已保留原目录".into());
        }
        Ok(())
    }

    fn images(&mut self, source: &Path, destination: &Path) -> Result<(), String> {
        for (path, _, _) in cache::entries(source)? {
            let id = path.file_name().ok_or("壁纸文件编号无效")?;
            let bytes =
                read_file(&path, policy::MAX_IMAGE_BYTES as u64)?.ok_or("原壁纸文件丢失")?;
            self.copy(
                &destination.join(id),
                &bytes,
                policy::MAX_IMAGE_BYTES as u64,
            )?;
        }
        Ok(())
    }
}

impl Drop for Copies {
    fn drop(&mut self) {
        if !self.committed {
            for path in self.created.iter().rev() {
                if let Err(error) = fs::remove_file(path) {
                    eprintln!("[Pake] Wallpaper migration cleanup: {error}");
                }
            }
        }
    }
}

impl Storage {
    pub fn new(
        legacy_root: PathBuf,
        metadata_root: PathBuf,
        picture_parent: Result<PathBuf, String>,
    ) -> Self {
        let (default_parent, warning) = match picture_parent {
            Ok(path) => (Some(path), None),
            Err(error) => (
                None,
                Some(format!(
                    "无法解析系统图片目录，暂时保留原保存位置；请更改位置重试：{error}"
                )),
            ),
        };
        Self {
            image_root: legacy_root.clone(),
            state_root: legacy_root.clone(),
            warning,
            read_only: false,
            migrated: false,
            metadata_root,
            legacy_root,
            default_parent,
            persistent: false,
        }
    }

    #[cfg(test)]
    pub fn legacy(root: PathBuf) -> Self {
        Self {
            image_root: root.clone(),
            state_root: root.clone(),
            metadata_root: root.clone(),
            legacy_root: root,
            default_parent: None,
            warning: None,
            read_only: false,
            migrated: false,
            persistent: false,
        }
    }

    pub fn initialize(mut self) -> Self {
        let result = self.initialize_inner();
        if let Err(error) = result {
            self.warning = Some(format!("保存目录未切换，原有图片已保留：{error}"));
            // A configuration that exists is the sole commit point; never
            // replace an untrusted selected location with a writable default.
            self.read_only = fs::symlink_metadata(self.metadata_root.join("storage.json")).is_ok();
            if self.read_only {
                self.state_root = self.metadata_root.clone();
            }
        }
        self
    }

    fn initialize_inner(&mut self) -> Result<(), String> {
        if self.metadata_root == self.legacy_root {
            return Ok(());
        }
        safe_path(&self.metadata_root)?;
        if let Some(bytes) = read_file(&self.metadata_root.join("storage.json"), 64 * 1024)? {
            let location: Location = serde_json::from_slice(&bytes)
                .map_err(|_| "壁纸目录配置损坏，请重新选择原保存位置")?;
            if location.version != 1 {
                return Err("壁纸目录配置版本不受支持".into());
            }
            self.image_root = location.image_root;
            self.state_root = self.metadata_root.clone();
            self.persistent = true;
            self.validate_destination(&self.image_root)?;
            self.require_source()?;
            self.warning = None;
            return Ok(());
        }
        if let Some(parent) = self.default_parent.clone() {
            let migrated = self.migrate(&parent)?;
            *self = migrated;
        }
        Ok(())
    }

    fn require_source(&self) -> Result<(), String> {
        self.validate_destination(&self.image_root)?;
        cache::validate_root(&self.image_root)
            .map_err(|_| "已选择的壁纸目录不可用，请连接原磁盘后重试；原目录配置已保留".into())
    }

    pub fn entries(&self) -> Result<Vec<(PathBuf, u64, std::time::SystemTime)>, String> {
        if self.persistent && self.read_only && self.require_source().is_err() {
            return Ok(Vec::new());
        }
        cache::entries(&self.image_root)
    }

    fn validate_destination(&self, target: &Path) -> Result<(), String> {
        safe_path(target)?;
        if target.file_name().and_then(|name| name.to_str()) != Some(LIBRARY_NAME)
            || overlaps(target, &self.metadata_root)?
            || overlaps(target, &self.legacy_root)?
        {
            return Err("不能选择内部设置、旧缓存或与其嵌套的目录".into());
        }
        if identity(target)? != identity(&self.image_root)? && overlaps(target, &self.image_root)? {
            return Err("新旧壁纸目录不能相互嵌套".into());
        }
        Ok(())
    }

    pub fn migrate(&self, parent: &Path) -> Result<Self, String> {
        if self.read_only && !self.persistent {
            return Err("无法确认已保存的原图库位置，请先恢复原目录配置；未更改保存位置".into());
        }
        safe_path(parent)?;
        if parent.parent().is_none() {
            return Err("不能选择磁盘根目录".into());
        }
        let target = parent.join(LIBRARY_NAME);
        self.validate_destination(&target)?;
        if self.persistent {
            self.require_source()?;
        }
        if identity(&target)? == identity(&self.image_root)? && self.persistent && !self.read_only {
            return Ok(self.clone());
        }
        directory(parent)?;
        let target = fs::canonicalize(parent)
            .map_err(|e| format!("无法解析所选文件夹：{e}"))?
            .join(LIBRARY_NAME);
        directory(&target)?;
        directory(&self.metadata_root)?;
        if !self.persistent {
            directory(&self.image_root)?;
        }
        let mut copies = Copies::default();
        copies.images(&self.image_root, &target)?;
        if self.state_root != self.metadata_root {
            if let Some(bytes) = read_file(&self.state_root.join("state.json"), MAX_STATE_BYTES)? {
                copies.copy(
                    &self.metadata_root.join("state.json"),
                    &bytes,
                    MAX_STATE_BYTES,
                )?;
            }
        }
        #[cfg(unix)]
        fs::File::open(&target)
            .and_then(|file| file.sync_all())
            .map_err(|e| format!("同步新壁纸目录失败：{e}"))?;
        let bytes = serde_json::to_vec(&Location {
            version: 1,
            image_root: target.clone(),
        })
        .map_err(|e| e.to_string())?;
        let config_path = self.metadata_root.join("storage.json");
        let warning = match cache::atomic_write(&config_path, &bytes) {
            Ok(()) => None,
            Err(error) => {
                // atomic_write can report a directory-sync failure after its
                // rename committed. Never roll back files now referenced by
                // the committed config; report the durability warning instead.
                if read_file(&config_path, 64 * 1024)?.as_deref() != Some(bytes.as_slice()) {
                    return Err(error);
                }
                Some(format!("保存位置已切换，但目录同步未确认：{error}"))
            }
        };
        copies.committed = true;
        let mut next = self.clone();
        next.image_root = target;
        next.state_root = self.metadata_root.clone();
        next.persistent = true;
        next.read_only = false;
        next.migrated = true;
        next.warning = warning;
        Ok(next)
    }
}

#[cfg(test)]
mod tests;
