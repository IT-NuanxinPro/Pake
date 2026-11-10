use super::{RotationConfig, WallpaperItem};
use std::collections::HashSet;
use tauri::Url;

pub const MAX_IMAGE_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_CACHE_BYTES: u64 = 256 * 1024 * 1024;

pub fn validate_source(url: &Url) -> Result<(), String> {
    if url.scheme() == "https"
        && url.host_str() == Some("wallpaper.061129.xyz")
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
    {
        Ok(())
    } else {
        Err("壁纸能力仅允许官方壁纸站调用".into())
    }
}

fn decode_segment(value: &str) -> Result<String, String> {
    let mut bytes = Vec::new();
    let input = value.as_bytes();
    let mut i = 0;
    while i < input.len() {
        if input[i] == b'%' {
            let hex = input.get(i + 1..i + 3).ok_or("路径编码无效")?;
            let hex = std::str::from_utf8(hex).map_err(|_| "路径编码无效")?;
            bytes.push(u8::from_str_radix(hex, 16).map_err(|_| "路径编码无效")?);
            i += 3;
        } else {
            bytes.push(input[i]);
            i += 1;
        }
    }
    String::from_utf8(bytes).map_err(|_| "路径必须为 UTF-8".into())
}

fn safe_segment(segment: &str) -> Result<(), String> {
    let mut value = segment.to_string();
    for _ in 0..8 {
        if value.is_empty()
            || value == "."
            || value == ".."
            || value
                .chars()
                .any(|c| c.is_control() || matches!(c, '/' | '\\' | ':'))
        {
            return Err("图片路径不安全".into());
        }
        if !value.contains('%') {
            return Ok(());
        }
        value = decode_segment(&value)?;
    }
    Err("路径编码层级过多".into())
}

fn valid_tag(value: &str) -> bool {
    value
        .strip_prefix('v')
        .map(|v| {
            let parts: Vec<_> = v.split('.').collect();
            parts.len() == 3
                && parts.iter().all(|p| {
                    !p.is_empty() && p.len() <= 10 && p.bytes().all(|c| c.is_ascii_digit())
                })
        })
        .unwrap_or(false)
}

pub fn validate_item(item: &WallpaperItem) -> Result<String, String> {
    if item.url.len() > 8192 || item.asset_key.len() > 8192 || item.title.len() > 512 {
        return Err("图片信息过长".into());
    }
    // Validate the original path too: URL parsers normalize dot segments.
    if item.url.contains('\\') || item.url.chars().any(char::is_control) {
        return Err("图片地址无效".into());
    }
    let url = Url::parse(&item.url).map_err(|_| "图片地址无效")?;
    if url.scheme() != "https"
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("仅支持可信 HTTPS 图片地址，无参数或身份凭据".into());
    }
    let original_path = item
        .url
        .strip_prefix("https://")
        .and_then(|v| v.find('/').map(|i| &v[i..]))
        .ok_or("图片地址缺少路径")?;
    for segment in original_path.trim_start_matches('/').split('/') {
        safe_segment(segment)?;
    }
    let tagged = match url.host_str() {
        Some("wallpaper.061129.xyz") => url.path().strip_prefix("/images/"),
        Some("cdn.jsdelivr.net") => url.path().strip_prefix("/gh/IT-NuanxinPro/nuanXinProPic@"),
        Some("raw.githubusercontent.com") => {
            url.path().strip_prefix("/IT-NuanxinPro/nuanXinProPic/")
        }
        _ => None,
    }
    .ok_or("仅支持壁纸仓库图片；暂不支持 Bing")?;
    let (tag, path) = tagged.split_once('/').ok_or("图片路径无效")?;
    if !valid_tag(tag) {
        return Err("图片版本标签无效".into());
    }
    let parts: Vec<_> = path.split('/').collect();
    if parts.len() < 3
        || parts[0] != "wallpaper"
        || !matches!(parts[1], "desktop" | "mobile" | "avatar")
    {
        return Err("图片不属于静态壁纸目录".into());
    }
    if item.asset_key != format!("{}:path:/{}", parts[1], path) {
        return Err("资源身份与图片路径不一致".into());
    }
    let extension = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    if !matches!(extension.as_str(), "jpg" | "jpeg" | "png") {
        return Err("原生壁纸目前仅支持 JPG/JPEG/PNG，请先将 WebP 转换为 PNG".into());
    }
    Ok(extension)
}

pub fn validate_config(mut config: RotationConfig) -> Result<RotationConfig, String> {
    if !config.enabled {
        return Ok(config);
    }
    if ![15, 30, 60, 360, 720, 1440].contains(&config.interval_minutes) {
        return Err("轮换间隔必须为 15、30、60、360、720 或 1440 分钟".into());
    }
    if config.items.is_empty() || config.items.len() > 50 {
        return Err("轮换列表须包含 1 至 50 张图片".into());
    }
    let mut seen = HashSet::new();
    for item in &config.items {
        validate_item(item)?;
    }
    config
        .items
        .retain(|item| seen.insert(item.asset_key.clone()));
    Ok(config)
}

pub fn validate_image(content_type: &str, extension: &str, bytes: &[u8]) -> Result<(), String> {
    if bytes.is_empty() || bytes.len() > MAX_IMAGE_BYTES {
        return Err("图片为空或超过 32 MiB".into());
    }
    let mime = content_type.split(';').next().unwrap_or("").trim();
    let valid = match extension {
        "jpg" | "jpeg" => mime == "image/jpeg" && bytes.starts_with(&[0xff, 0xd8, 0xff]),
        "png" => mime == "image/png" && bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err("响应类型或图片签名与扩展名不符".into())
    }
}

#[cfg(test)]
#[path = "policy_tests.rs"]
mod tests;
