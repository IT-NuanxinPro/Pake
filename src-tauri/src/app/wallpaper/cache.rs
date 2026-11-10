use super::{policy, WallpaperItem};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};
use tauri_plugin_http::reqwest::{redirect::Policy, Client};

pub fn filename(item: &WallpaperItem) -> Result<String, String> {
    let extension = policy::validate_item(item)?;
    let mut hash = Sha256::new();
    hash.update(item.asset_key.as_bytes());
    hash.update([0]);
    hash.update(item.url.as_bytes());
    Ok(format!("{:x}.{extension}", hash.finalize()))
}

pub fn managed_name(name: &str) -> bool {
    let Some((hash, ext)) = name.split_once('.') else {
        return false;
    };
    hash.len() == 64
        && hash
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        && matches!(ext, "jpg" | "jpeg" | "png")
}

pub fn validate_id(cache_id: &str) -> Result<(), String> {
    if managed_name(cache_id) {
        Ok(())
    } else {
        Err("本地壁纸编号无效".into())
    }
}

pub fn ensure_root(root: &Path) -> Result<(), String> {
    fs::create_dir_all(root).map_err(|e| format!("无法创建壁纸缓存：{e}"))?;
    validate_root(root)
}

pub fn validate_root(root: &Path) -> Result<(), String> {
    if !fs::symlink_metadata(root)
        .map_err(|e| e.to_string())?
        .is_dir()
    {
        return Err("壁纸缓存目录无效或为符号链接".into());
    }
    Ok(())
}

#[cfg(test)]
pub fn clear_interrupted_writes(root: &Path) -> Result<(), String> {
    for entry in fs::read_dir(root).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if let Some(stem) = name.strip_suffix(".pending") {
            if (stem == "state"
                || (stem.len() == 64 && stem.bytes().all(|c| c.is_ascii_hexdigit())))
                && fs::symlink_metadata(entry.path())
                    .map_err(|e| e.to_string())?
                    .is_file()
            {
                fs::remove_file(entry.path())
                    .map_err(|e| format!("清理中断的缓存写入失败：{e}"))?;
            }
        }
    }
    Ok(())
}

fn validate_response(
    status: u16,
    length: Option<u64>,
    content_type: &str,
    extension: &str,
) -> Result<(), String> {
    if !(200..300).contains(&status) {
        return Err("图片下载失败或返回重定向".into());
    }
    if length.is_some_and(|length| length > policy::MAX_IMAGE_BYTES as u64) {
        return Err("图片超过 32 MiB".into());
    }
    let expected = if extension == "png" {
        "image/png"
    } else {
        "image/jpeg"
    };
    if content_type.split(';').next().unwrap_or("").trim() != expected {
        return Err("图片响应类型不受支持".into());
    }
    Ok(())
}

pub fn entries(root: &Path) -> Result<Vec<(PathBuf, u64, SystemTime)>, String> {
    validate_root(root)?;
    let mut entries = Vec::new();
    for entry in fs::read_dir(root).map_err(|e| format!("无法读取壁纸缓存：{e}"))? {
        let entry = entry.map_err(|e| e.to_string())?;
        if !managed_name(&entry.file_name().to_string_lossy()) {
            continue;
        }
        let meta = fs::symlink_metadata(entry.path()).map_err(|e| e.to_string())?;
        if meta.is_file() {
            entries.push((
                entry.path(),
                meta.len(),
                meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            ));
        }
    }
    Ok(entries)
}

#[cfg(test)]
pub fn reserve(root: &Path, incoming: u64, protected: &[String]) -> Result<(), String> {
    let mut files = entries(root)?;
    let mut size: u64 = files.iter().map(|entry| entry.1).sum();
    files.sort_by_key(|entry| entry.2);
    for (path, bytes, _) in files {
        if size.saturating_add(incoming) <= policy::MAX_CACHE_BYTES {
            return Ok(());
        }
        if protected
            .iter()
            .any(|name| path.file_name().is_some_and(|value| value == name.as_str()))
        {
            continue;
        }
        fs::remove_file(&path).map_err(|e| format!("清理缓存失败：{e}"))?;
        size = size.saturating_sub(bytes);
    }
    if size.saturating_add(incoming) > policy::MAX_CACHE_BYTES {
        Err("缓存已达 256 MiB 上限，当前与上一张图片受到保护".into())
    } else {
        Ok(())
    }
}

pub fn atomic_write(path: &Path, data: &[u8]) -> Result<(), String> {
    let temporary = path.with_extension("pending");
    // Operations are serialized; a previous interrupted write is safe to discard.
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .or_else(|error| {
            if error.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(error);
            }
            fs::remove_file(&temporary)?;
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
        })
        .map_err(|e| format!("无法创建缓存临时文件：{e}"))?;
    file.write_all(data)
        .and_then(|_| file.sync_all())
        .map_err(|e| format!("写入缓存失败：{e}"))?;
    drop(file);
    #[cfg(not(target_os = "windows"))]
    fs::rename(&temporary, path).map_err(|e| format!("原子写入缓存失败：{e}"))?;
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{
            MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
        };
        let from: Vec<u16> = temporary.as_os_str().encode_wide().chain(Some(0)).collect();
        let to: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        if unsafe {
            MoveFileExW(
                from.as_ptr(),
                to.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        } == 0
        {
            return Err(format!(
                "原子写入缓存失败：{}",
                std::io::Error::last_os_error()
            ));
        }
    }
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        fs::File::open(parent)
            .and_then(|file| file.sync_all())
            .map_err(|e| format!("同步缓存目录失败：{e}"))?;
    }
    Ok(())
}

pub fn cached_path(root: &Path, item: &WallpaperItem) -> Result<Option<PathBuf>, String> {
    let path = root.join(filename(item)?);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(value) => value,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    if !metadata.is_file() || metadata.len() > policy::MAX_IMAGE_BYTES as u64 {
        return Err("本地壁纸缓存无效".into());
    }
    let bytes = fs::read(&path).map_err(|e| e.to_string())?;
    let extension = policy::validate_item(item)?;
    let mime = if extension == "png" {
        "image/png"
    } else {
        "image/jpeg"
    };
    policy::validate_image(mime, &extension, &bytes)?;
    Ok(Some(path))
}

pub async fn prepare(root: PathBuf, item: WallpaperItem) -> Result<PathBuf, String> {
    let extension = policy::validate_item(&item)?;
    let check_root = root.clone();
    let check_item = item.clone();
    if let Some(path) = super::blocking(move || cached_path(&check_root, &check_item)).await? {
        return Ok(path);
    }
    let client = Client::builder()
        .redirect(Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(45))
        .build()
        .map_err(|e| format!("无法初始化图片下载：{e}"))?;
    let download = async {
        let mut response = client
            .get(&item.url)
            .send()
            .await
            .map_err(|_| "图片下载连接失败")?;
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();
        validate_response(
            response.status().as_u16(),
            response.content_length(),
            &content_type,
            &extension,
        )?;
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| "读取图片响应失败")? {
            if bytes.len().saturating_add(chunk.len()) > policy::MAX_IMAGE_BYTES {
                return Err("图片超过 32 MiB".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        policy::validate_image(&content_type, &extension, &bytes)?;
        Ok::<Vec<u8>, String>(bytes)
    };
    let bytes = tokio::time::timeout(Duration::from_secs(45), download)
        .await
        .map_err(|_| "图片下载超时")??;
    super::blocking(move || {
        let name = filename(&item)?;
        let path = root.join(name);
        atomic_write(&path, &bytes)?;
        Ok(path)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    fn root() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "wallpaper-cache-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }
    #[test]
    fn redirects_and_oversized_metadata_are_rejected() {
        for status in [301, 302, 303, 307, 308, 404, 500] {
            assert!(validate_response(status, None, "image/jpeg", "jpg").is_err());
        }
        assert!(validate_response(
            200,
            Some(policy::MAX_IMAGE_BYTES as u64 + 1),
            "image/jpeg",
            "jpg"
        )
        .is_err());
        assert!(validate_response(200, None, "text/html", "jpg").is_err());
        assert!(validate_response(200, None, "image/jpeg", "jpg").is_ok());
    }
    #[test]
    fn noncanonical_uppercase_ids_are_rejected_and_never_listed_or_cleared() {
        let root = root();
        let upper = format!("{}.png", "A".repeat(64));
        fs::write(root.join(&upper), b"preserve noncanonical file").unwrap();
        assert!(validate_id(&upper).is_err());
        assert!(entries(&root).unwrap().is_empty());
        reserve(&root, 0, &[]).unwrap();
        assert!(root.join(&upper).exists());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn cleanup_discards_only_interrupted_managed_writes() {
        let root = root();
        let pending = format!("{}.pending", "a".repeat(64));
        fs::write(root.join(&pending), b"partial").unwrap();
        fs::write(root.join("state.pending"), b"partial").unwrap();
        fs::write(root.join("unrelated.pending"), b"preserve").unwrap();
        clear_interrupted_writes(&root).unwrap();
        assert!(!root.join(pending).exists());
        assert!(!root.join("state.pending").exists());
        assert!(root.join("unrelated.pending").exists());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn atomic_state_replaces_complete_document() {
        let root = root();
        let path = root.join("state.json");
        atomic_write(&path, b"old").unwrap();
        atomic_write(&path, b"new document").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new document");
        assert!(!path.with_extension("pending").exists());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn eviction_preserves_current_and_previous_and_bounds_total() {
        let root = root();
        let a = format!("{}.jpg", "a".repeat(64));
        let b = format!("{}.png", "b".repeat(64));
        let c = format!("{}.jpg", "c".repeat(64));
        for name in [&a, &b, &c] {
            fs::File::create(root.join(name))
                .unwrap()
                .set_len(100 * 1024 * 1024)
                .unwrap();
        }
        reserve(&root, 32 * 1024 * 1024, &[a.clone(), b.clone()]).unwrap();
        assert!(root.join(a).exists());
        assert!(root.join(b).exists());
        assert!(!root.join(c).exists());
        assert!(
            entries(&root).unwrap().iter().map(|e| e.1).sum::<u64>() + 32 * 1024 * 1024
                <= policy::MAX_CACHE_BYTES
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn cache_identity_includes_full_url_and_path() {
        let a = WallpaperItem {
            asset_key: "desktop:path:/wallpaper/desktop/a.jpg".into(),
            url: "https://wallpaper.061129.xyz/images/v1.2.3/wallpaper/desktop/a.jpg".into(),
            title: "a".into(),
        };
        let mut b = a.clone();
        b.url = b.url.replace("v1.2.3", "v1.2.4");
        assert_ne!(filename(&a).unwrap(), filename(&b).unwrap());
    }
}
