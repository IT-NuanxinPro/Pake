use std::path::Path;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::process::{Command, Stdio};

#[cfg(any(target_os = "linux", test))]
fn linux_schema(desktop: &str) -> Option<&'static str> {
    let desktop = desktop.to_ascii_lowercase();
    if desktop.split(':').any(|v| v == "gnome" || v == "ubuntu") {
        Some("org.gnome.desktop.background")
    } else if desktop
        .split(':')
        .any(|v| v == "cinnamon" || v == "x-cinnamon")
    {
        Some("org.cinnamon.desktop.background")
    } else {
        None
    }
}

#[derive(Debug)]
pub struct ApplyFailure {
    pub message: String,
    pub may_have_changed: bool,
}

impl From<String> for ApplyFailure {
    fn from(message: String) -> Self {
        Self {
            message,
            may_have_changed: false,
        }
    }
}

#[derive(Clone, Debug)]
pub enum Backend {
    #[cfg(test)]
    Test,
    #[cfg(target_os = "windows")]
    Windows,
    #[cfg(target_os = "macos")]
    Mac,
    #[cfg(target_os = "linux")]
    GSettings {
        schema: String,
        dark: bool,
    },
    Unsupported(String),
}

impl Backend {
    pub fn detect() -> Self {
        #[cfg(target_os = "windows")]
        {
            Self::Windows
        }
        #[cfg(target_os = "macos")]
        {
            if Path::new("/usr/bin/osascript").is_file() {
                Self::Mac
            } else {
                Self::Unsupported("未检测到 macOS 壁纸接口".into())
            }
        }
        #[cfg(target_os = "linux")]
        {
            let desktop = std::env::var("XDG_CURRENT_DESKTOP")
                .unwrap_or_default()
                .to_lowercase();
            let Some(schema) = linux_schema(&desktop) else {
                return Self::Unsupported("当前 Linux 桌面暂不支持；仅支持 GNOME/Cinnamon".into());
            };
            match run_command(Command::new("gsettings").args(["list-keys", schema])) {
                Ok(keys) if keys.lines().any(|k| k == "picture-uri") => {
                    let dark = keys.lines().any(|k| k == "picture-uri-dark");
                    for key in if dark {
                        vec!["picture-uri", "picture-uri-dark"]
                    } else {
                        vec!["picture-uri"]
                    } {
                        match run_command(Command::new("gsettings").args(["writable", schema, key]))
                        {
                            Ok(value) if value.trim() == "true" => {}
                            _ => return Self::Unsupported("桌面壁纸设置不可写".into()),
                        }
                    }
                    Self::GSettings {
                        schema: schema.into(),
                        dark,
                    }
                }
                _ => Self::Unsupported("未检测到可用的 gsettings 壁纸接口".into()),
            }
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
        {
            Self::Unsupported("当前平台暂不支持原生壁纸".into())
        }
    }

    pub fn reason(&self) -> Option<String> {
        match self {
            Self::Unsupported(reason) => Some(reason.clone()),
            _ => None,
        }
    }

    pub fn apply(&self, path: &Path) -> Result<(), ApplyFailure> {
        match self {
            #[cfg(test)]
            Self::Test => {
                if path.is_file() {
                    Ok(())
                } else {
                    Err("test image missing".to_string().into())
                }
            }
            #[cfg(target_os = "windows")]
            Self::Windows => {
                use std::os::windows::ffi::OsStrExt;
                use windows_sys::Win32::UI::WindowsAndMessaging::{
                    SystemParametersInfoW, SPIF_SENDCHANGE, SPIF_UPDATEINIFILE,
                    SPI_SETDESKWALLPAPER,
                };
                let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
                let result = unsafe {
                    SystemParametersInfoW(
                        SPI_SETDESKWALLPAPER,
                        0,
                        wide.as_ptr() as *mut _,
                        SPIF_UPDATEINIFILE | SPIF_SENDCHANGE,
                    )
                };
                if result == 0 {
                    Err(format!("Windows 设置壁纸失败：{}", std::io::Error::last_os_error()).into())
                } else {
                    Ok(())
                }
            }
            #[cfg(target_os = "macos")]
            Self::Mac => {
                // The path is an argv value, never interpolated into AppleScript.
                run_command(Command::new("/usr/bin/osascript").args(["-e", "on run argv\ntell application \"System Events\"\nset picture of every desktop to POSIX file (item 1 of argv)\nend tell\nend run", "--"]).arg(path)).map(|_| ()).map_err(Into::into)
            }
            #[cfg(target_os = "linux")]
            Self::GSettings { schema, dark } => {
                let uri = tauri::Url::from_file_path(path)
                    .map_err(|_| ApplyFailure::from("缓存路径无效".to_string()))?;
                super::gsettings::apply(schema, *dark, uri.as_str(), |args| {
                    run_command(Command::new("gsettings").args(args))
                })
            }
            Self::Unsupported(reason) => Err(reason.clone().into()),
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn run_command(command: &mut Command) -> Result<String, String> {
    use std::{
        io::Read,
        time::{Duration, Instant},
    };
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("无法运行系统壁纸接口：{e}"))?;
    let start = Instant::now();
    loop {
        match child
            .try_wait()
            .map_err(|e| format!("无法读取系统操作结果：{e}"))?
        {
            Some(status) => {
                if !status.success() {
                    return Err(format!(
                        "系统壁纸接口执行失败（{status}），请检查系统自动化权限或桌面设置"
                    ));
                }
                let mut output = String::new();
                if let Some(stdout) = child.stdout.take() {
                    stdout
                        .take(8192)
                        .read_to_string(&mut output)
                        .map_err(|e| e.to_string())?;
                }
                return Ok(output);
            }
            None if start.elapsed() > Duration::from_secs(20) => {
                child
                    .kill()
                    .map_err(|e| format!("系统壁纸接口超时且终止失败：{e}"))?;
                child.wait().map_err(|e| e.to_string())?;
                return Err("系统壁纸接口操作超时".into());
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::linux_schema;

    #[test]
    fn linux_requires_a_supported_desktop_token() {
        assert_eq!(
            linux_schema("ubuntu:GNOME"),
            Some("org.gnome.desktop.background")
        );
        assert_eq!(
            linux_schema("X-Cinnamon"),
            Some("org.cinnamon.desktop.background")
        );
        for desktop in ["", "KDE", "sway", "not-gnome", "GNOME_FAKE"] {
            assert!(linux_schema(desktop).is_none());
        }
    }
}
