use super::{cache, policy, RotationStatus};
use image::{ImageFormat, ImageReader, Limits};
use serde::Serialize;
use std::{
    ffi::OsString,
    fs,
    io::{BufReader, Cursor},
    path::{Path, PathBuf},
    process::Command,
};

pub const MAX_THUMBNAIL_BYTES: usize = 256 * 1024;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheEntry {
    pub cache_id: String,
    pub title: String,
    pub title_known: bool,
    pub bytes: u64,
    pub modified_at: Option<u64>,
    pub last_applied_at: Option<u64>,
    pub is_current: bool,
    pub is_previous: bool,
    pub in_rotation: bool,
    pub possibly_active: bool,
    pub can_apply: bool,
    pub delete_blocked_reason: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheSnapshot {
    pub storage_path: String,
    pub total_bytes: u64,
    pub total_count: usize,
    pub read_only: bool,
    pub warning: Option<String>,
    pub status: RotationStatus,
    pub entries: Vec<CacheEntry>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClearUnusedResult {
    pub removed_count: usize,
    pub freed_bytes: u64,
    pub library: CacheSnapshot,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageChoice {
    pub cancelled: bool,
    pub library: CacheSnapshot,
}

pub fn file_path(root: &Path, cache_id: &str) -> Result<PathBuf, String> {
    cache::validate_id(cache_id)?;
    cache::validate_root(root)?;
    let path = root.join(cache_id);
    if !fs::symlink_metadata(&path)
        .map_err(|_| "本地壁纸文件不存在")?
        .is_file()
    {
        return Err("本地壁纸不是普通文件或为符号链接".into());
    }
    Ok(path)
}

pub fn image_path(root: &Path, cache_id: &str) -> Result<PathBuf, String> {
    let path = file_path(root, cache_id)?;
    let metadata = fs::metadata(&path).map_err(|e| e.to_string())?;
    if metadata.len() == 0 || metadata.len() > policy::MAX_IMAGE_BYTES as u64 {
        return Err("本地壁纸为空或超过 32 MiB".into());
    }
    let bytes = fs::read(&path).map_err(|e| format!("读取本地壁纸失败：{e}"))?;
    let extension = cache_id.rsplit('.').next().ok_or("本地壁纸格式无效")?;
    let mime = if extension == "png" {
        "image/png"
    } else {
        "image/jpeg"
    };
    policy::validate_image(mime, extension, &bytes)?;
    Ok(path)
}

pub fn thumbnail(root: &Path, cache_id: &str) -> Result<Vec<u8>, String> {
    let path = image_path(root, cache_id)?;
    let format = if cache_id.ends_with(".png") {
        ImageFormat::Png
    } else {
        ImageFormat::Jpeg
    };
    let mut reader = ImageReader::with_format(
        BufReader::new(fs::File::open(path).map_err(|e| e.to_string())?),
        format,
    );
    let mut limits = Limits::default();
    limits.max_image_width = Some(16384);
    limits.max_image_height = Some(16384);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    let image = reader
        .decode()
        .map_err(|_| "本地图片损坏或超过安全解码限制")?;
    let mut width = 360;
    let mut height = 240;
    loop {
        let resized = image.thumbnail(width, height).to_rgb8();
        let mut bytes = Cursor::new(Vec::new());
        resized
            .write_to(&mut bytes, ImageFormat::Png)
            .map_err(|_| "无法生成本地壁纸预览")?;
        let bytes = bytes.into_inner();
        if bytes.len() <= MAX_THUMBNAIL_BYTES {
            return Ok(bytes);
        }
        if width <= 90 {
            return Err("本地壁纸预览超过大小限制".into());
        }
        width /= 2;
        height /= 2;
    }
}

fn reveal_command(
    root: &Path,
    cache_id: Option<&str>,
) -> Result<(&'static str, Vec<OsString>), String> {
    cache::validate_root(root)?;
    let path = cache_id.map(|id| file_path(root, id)).transpose()?;
    #[cfg(target_os = "macos")]
    {
        let args = match path {
            Some(path) => vec![OsString::from("-R"), path.into_os_string()],
            None => vec![root.as_os_str().to_owned()],
        };
        Ok(("/usr/bin/open", args))
    }
    #[cfg(target_os = "windows")]
    {
        let args = match path {
            Some(path) => {
                let mut arg = OsString::from("/select,");
                arg.push(path);
                vec![arg]
            }
            None => vec![root.as_os_str().to_owned()],
        };
        Ok(("explorer.exe", args))
    }
    #[cfg(target_os = "linux")]
    {
        let _path = path;
        Ok(("/usr/bin/xdg-open", vec![root.as_os_str().to_owned()]))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        let _path = path;
        Err("当前平台暂不支持打开本地目录".into())
    }
}

pub fn reveal(root: &Path, cache_id: Option<&str>) -> Result<(), String> {
    let (program, args) = reveal_command(root, cache_id)?;
    let status = Command::new(program)
        .args(args)
        .status()
        .map_err(|e| format!("无法打开本地壁纸目录：{e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("系统未能打开本地壁纸目录".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn root() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "wallpaper-library-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn only_complete_cache_ids_can_resolve_regular_files() {
        let root = root();
        let id = format!("{}.png", "a".repeat(64));
        fs::write(root.join(&id), b"not image").unwrap();
        assert!(file_path(&root, &id).is_ok());
        assert!(image_path(&root, &id).is_err());
        for id in [
            "../state.json",
            "/tmp/a.png",
            "a.png",
            "state.json",
            "..",
            "",
        ] {
            assert!(file_path(&root, id).is_err());
        }
        #[cfg(unix)]
        {
            let linked = format!("{}.png", "b".repeat(64));
            std::os::unix::fs::symlink(root.join(&id), root.join(&linked)).unwrap();
            assert!(file_path(&root, &linked).is_err());
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn png_and_jpeg_thumbnails_are_bounded_binary_pngs() {
        let root = root();
        for (extension, format) in [("png", ImageFormat::Png), ("jpg", ImageFormat::Jpeg)] {
            let id = format!("{}.{extension}", "a".repeat(64));
            let source = image::RgbImage::from_fn(1000, 700, |x, y| {
                image::Rgb([(x % 256) as u8, (y % 256) as u8, ((x * y) % 256) as u8])
            });
            source.save_with_format(root.join(&id), format).unwrap();
            let bytes = thumbnail(&root, &id).unwrap();
            assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
            assert!(bytes.len() <= 256 * 1024);
            let result = image::load_from_memory(&bytes).unwrap();
            assert!(result.width() <= 360 && result.height() <= 240);
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn malformed_and_excessive_dimension_images_are_rejected() {
        let root = root();
        let id = format!("{}.png", "a".repeat(64));
        fs::write(root.join(&id), b"\x89PNG\r\n\x1a\ntruncated").unwrap();
        assert!(thumbnail(&root, &id).is_err());
        image::RgbImage::new(16385, 1).save(root.join(&id)).unwrap();
        assert!(thumbnail(&root, &id).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reveal_uses_fixed_program_and_literal_arguments_without_launching() {
        let root = root();
        let id = format!("{}.png", "a".repeat(64));
        fs::write(root.join(&id), b"file").unwrap();
        let (program, args) = reveal_command(&root, Some(&id)).unwrap();
        assert!(!matches!(program, "sh" | "bash" | "cmd" | "powershell"));
        assert!(args
            .iter()
            .any(|arg| arg.to_string_lossy().contains(root.to_str().unwrap())));
        assert!(reveal_command(&root, Some("../state.json")).is_err());
        let upper = format!("{}.png", "A".repeat(64));
        assert!(reveal_command(&root, Some(&upper)).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
