mod cache;
mod platform;
mod policy;
mod service;

use serde::{Deserialize, Serialize};
use service::Service;
use std::sync::Arc;
use tauri::{AppHandle, Manager, WebviewWindow};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WallpaperItem {
    pub asset_key: String,
    pub url: String,
    pub title: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RotationConfig {
    pub enabled: bool,
    pub interval_minutes: u64,
    pub items: Vec<WallpaperItem>,
}

impl Default for RotationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_minutes: 60,
            items: Vec::new(),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    supported: bool,
    platform: &'static str,
    reason: Option<String>,
    version: u8,
    app_version: String,
    supported_formats: Vec<&'static str>,
    supports_bing: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RotationStatus {
    supported: bool,
    enabled: bool,
    interval_minutes: u64,
    current: Option<WallpaperItem>,
    previous: Option<WallpaperItem>,
    last_error: Option<String>,
    cache_count: usize,
}

pub(super) async fn blocking<T: Send + 'static>(
    operation: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(operation)
        .await
        .map_err(|e| format!("原生壁纸任务中断：{e}"))?
}

fn authorized_service(window: &WebviewWindow) -> Result<Arc<Service>, String> {
    let url = window.url().map_err(|_| "无法验证窗口来源")?;
    policy::validate_source(&url)?;
    window
        .app_handle()
        .try_state::<Arc<Service>>()
        .map(|state| state.inner().clone())
        .ok_or("当前应用未启用壁纸站原生能力".into())
}

#[tauri::command]
pub async fn wallpaper_capabilities(window: WebviewWindow) -> Result<Capabilities, String> {
    let service = authorized_service(&window)?;
    let reason = service.reason().await?;
    Ok(Capabilities {
        supported: reason.is_none(),
        platform: std::env::consts::OS,
        reason,
        version: 1,
        app_version: window.app_handle().package_info().version.to_string(),
        supported_formats: vec!["jpg", "jpeg", "png"],
        supports_bing: false,
    })
}

#[tauri::command]
pub async fn wallpaper_apply(
    window: WebviewWindow,
    item: WallpaperItem,
) -> Result<RotationStatus, String> {
    let service = authorized_service(&window)?;
    service.apply(item).await
}

#[tauri::command]
pub async fn wallpaper_restore(window: WebviewWindow) -> Result<RotationStatus, String> {
    authorized_service(&window)?.restore().await
}

#[tauri::command]
pub async fn wallpaper_configure_rotation(
    window: WebviewWindow,
    config: RotationConfig,
) -> Result<RotationStatus, String> {
    authorized_service(&window)?.configure(config).await
}

#[tauri::command]
pub async fn wallpaper_rotation_status(window: WebviewWindow) -> Result<RotationStatus, String> {
    authorized_service(&window)?.status().await
}

pub fn start(app: &AppHandle, config: &super::config::PakeConfig) -> Result<(), String> {
    // The runtime app config, not the generic package name, owns this feature.
    let is_gallery = config
        .windows
        .first()
        .and_then(|window| tauri::Url::parse(&window.url).ok())
        .is_some_and(|url| policy::validate_source(&url).is_ok());
    if !is_gallery {
        return Ok(());
    }
    let root = app
        .path()
        .app_cache_dir()
        .map_err(|e| e.to_string())?
        .join("wallpaper-v1");
    let service = Arc::new(Service::new(root));
    app.manage(service.clone());
    tauri::async_runtime::spawn(async move {
        loop {
            if let Err(error) = service.tick().await {
                eprintln!("[Pake] Wallpaper rotation: {error}");
            }
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        }
    });
    Ok(())
}

#[cfg(test)]
mod acl_tests;

#[cfg(test)]
mod capability_contract_tests {
    use super::Capabilities;

    #[test]
    fn browser_capabilities_use_camel_case_fields() {
        let value = serde_json::to_value(Capabilities {
            supported: true,
            platform: "windows",
            reason: None,
            version: 1,
            app_version: "5.2.0".into(),
            supported_formats: vec!["jpg", "jpeg", "png"],
            supports_bing: false,
        })
        .unwrap();
        assert_eq!(value["appVersion"], "5.2.0");
        assert_eq!(
            value["supportedFormats"],
            serde_json::json!(["jpg", "jpeg", "png"])
        );
        assert_eq!(value["supportsBing"], false);
        assert!(value.get("app_version").is_none());
    }
}

#[cfg(any(target_os = "linux", test))]
mod gsettings;
