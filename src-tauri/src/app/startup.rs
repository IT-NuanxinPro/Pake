use std::{borrow::Cow, collections::HashMap, sync::Mutex};
use tauri::{
    utils::assets::{AssetKey, AssetsIter, CspHash, EmbeddedAssets},
    Assets, Context, Manager, Runtime, Theme, Url, WebviewWindow,
};

pub const ASSET_NAME: &str = "wallpaper-startup.html";
const ASSET_KEY: &str = "/wallpaper-startup.html";
const HTML: &str = include_str!("startup.html");
const CONNECT_TIMEOUT_MS: u64 = 15_000;

struct StartupAssets<R: Runtime> {
    fallback: Box<dyn Assets<R>>,
}

impl<R: Runtime> Assets<R> for StartupAssets<R> {
    fn setup(&self, app: &tauri::App<R>) {
        self.fallback.setup(app);
    }

    fn get(&self, key: &AssetKey) -> Option<Cow<'_, [u8]>> {
        if key.as_ref() == ASSET_KEY {
            Some(Cow::Borrowed(HTML.as_bytes()))
        } else {
            self.fallback.get(key)
        }
    }

    fn iter(&self) -> Box<AssetsIter<'_>> {
        Box::new(
            self.fallback
                .iter()
                .filter(|(key, _)| key.as_ref() != ASSET_KEY)
                .chain(std::iter::once((
                    Cow::Borrowed(ASSET_KEY),
                    Cow::Borrowed(HTML.as_bytes()),
                ))),
        )
    }

    fn csp_hashes(&self, html_path: &AssetKey) -> Box<dyn Iterator<Item = CspHash<'_>> + '_> {
        self.fallback.csp_hashes(html_path)
    }
}

pub fn with_assets<R: Runtime>(mut context: Context<R>) -> Context<R> {
    let empty = EmbeddedAssets::new(Default::default(), &[], Default::default());
    let fallback = context.set_assets(Box::new(empty));
    context.set_assets(Box::new(StartupAssets { fallback }));
    context
}

fn official_url(url: &Url) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some("wallpaper.061129.xyz")
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
}

pub fn configured_target(url_type: &str, value: &str, home_window: bool) -> Option<Url> {
    if url_type != "web" || !home_window {
        return None;
    }
    Url::parse(value).ok().filter(official_url)
}

pub fn is_local_page(url: &Url) -> bool {
    url.path() == ASSET_KEY
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && ((url.scheme() == "tauri" && url.host_str() == Some("localhost"))
            || (url.scheme() == "http" && url.host_str() == Some("tauri.localhost")))
}

pub fn local_url() -> Result<Url, String> {
    let origin = if cfg!(target_os = "windows") {
        "http://tauri.localhost/"
    } else {
        "tauri://localhost/"
    };
    Url::parse(origin)
        .and_then(|url| url.join(ASSET_NAME))
        .map_err(|error| error.to_string())
}

pub fn initialization_script(target: &Url, dark_mode: bool) -> Result<String, serde_json::Error> {
    let surface = HTML
        .split_once("<!-- startup-surface -->")
        .and_then(|(_, rest)| rest.split_once("<!-- /startup-surface -->"))
        .map(|(surface, _)| surface)
        .unwrap_or(HTML);
    let config = serde_json::json!({ "target": target, "surface": surface, "darkMode": dark_mode });
    Ok(format!(
        "window.__WALLPAPER_STARTUP_CONFIG__ = {};\n{}",
        serde_json::to_string(&config)?,
        include_str!("../inject/startup.js")
    ))
}

pub fn background(dark: bool) -> tauri::window::Color {
    if dark {
        tauri::window::Color(15, 15, 26, 255)
    } else {
        tauri::window::Color(248, 249, 250, 255)
    }
}

pub fn sync_background(window: &WebviewWindow) {
    match window.theme() {
        Ok(theme) => {
            if let Err(error) = window.set_background_color(Some(background(theme == Theme::Dark)))
            {
                eprintln!("[Pake] Failed to synchronize startup background: {error}");
            }
        }
        Err(error) => eprintln!("[Pake] Failed to read startup theme: {error}"),
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Boot,
    Loading,
    Ready,
    TimedOut,
}

struct Session {
    target: Url,
    generation: u64,
    phase: Phase,
    reveal_claimed: bool,
}

impl Session {
    fn advance(&mut self, phase: Phase) -> u64 {
        self.generation += 1;
        self.phase = phase;
        self.generation
    }

    fn ready(&mut self, generation: u64) -> bool {
        if self.generation != generation || self.phase != Phase::Loading {
            return false;
        }
        self.phase = Phase::Ready;
        true
    }

    fn timeout(&mut self, generation: u64) -> bool {
        if self.generation != generation || self.phase != Phase::Loading {
            return false;
        }
        self.advance(Phase::TimedOut);
        true
    }
}

#[derive(Default)]
pub struct StartupState(Mutex<HashMap<String, Session>>);

pub fn register(window_app: &tauri::AppHandle, label: &str, target: Url) -> Result<(), String> {
    window_app
        .state::<StartupState>()
        .0
        .lock()
        .map_err(|_| "Startup state is unavailable")?
        .insert(
            label.to_owned(),
            Session {
                target,
                generation: 1,
                phase: Phase::Boot,
                reveal_claimed: false,
            },
        );
    Ok(())
}

pub fn enabled(window: &WebviewWindow) -> bool {
    window
        .app_handle()
        .state::<StartupState>()
        .0
        .lock()
        .map(|sessions| sessions.contains_key(window.label()))
        .unwrap_or(false)
}

pub fn claim_local_reveal(window: &WebviewWindow) -> bool {
    if !window.url().map(|url| is_local_page(&url)).unwrap_or(false) {
        return false;
    }
    window
        .app_handle()
        .state::<StartupState>()
        .0
        .lock()
        .ok()
        .and_then(|mut sessions| {
            sessions.get_mut(window.label()).map(|session| {
                let first = !session.reveal_claimed;
                session.reveal_claimed = true;
                first
            })
        })
        .unwrap_or(false)
}

pub fn cancel_reveal(window: &WebviewWindow) {
    if let Ok(mut sessions) = window.app_handle().state::<StartupState>().0.lock() {
        if let Some(session) = sessions.get_mut(window.label()) {
            session.reveal_claimed = true;
        }
    }
}

pub fn forget_window(app: &tauri::AppHandle, label: &str) {
    if let Ok(mut sessions) = app.state::<StartupState>().0.lock() {
        sessions.remove(label);
    }
}

pub fn start_local_page(window: &WebviewWindow) {
    if let Err(error) = window.eval("window.__WALLPAPER_STARTUP__?.start()") {
        eprintln!("[Pake] Failed to start the local launch page: {error}");
    }
}

fn schedule_timeout(window: WebviewWindow, generation: u64) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(CONNECT_TIMEOUT_MS)).await;
        let timed_out = window
            .app_handle()
            .state::<StartupState>()
            .0
            .lock()
            .ok()
            .and_then(|mut sessions| {
                sessions
                    .get_mut(window.label())
                    .map(|session| session.timeout(generation))
            })
            .unwrap_or(false);
        if !timed_out {
            return;
        }
        // Do not replace an authentication page after the user navigated away.
        let current = match window.url() {
            Ok(url) => url,
            Err(error) => {
                eprintln!("[Pake] Failed to inspect startup navigation: {error}");
                return;
            }
        };
        let browser_error = matches!(current.scheme(), "about" | "chrome-error");
        if !official_url(&current) && !is_local_page(&current) && !browser_error {
            return;
        }
        match local_url().and_then(|url| window.navigate(url).map_err(|error| error.to_string())) {
            Ok(()) => {}
            Err(error) => eprintln!("[Pake] Failed to restore the local launch page: {error}"),
        }
    });
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartupReply {
    generation: u64,
    timed_out: bool,
    accepted: bool,
}

#[tauri::command]
pub async fn wallpaper_startup(
    window: WebviewWindow,
    event: String,
    generation: Option<u64>,
) -> Result<StartupReply, String> {
    let source = window.url().map_err(|error| error.to_string())?;
    let local = is_local_page(&source);
    if !local && !official_url(&source) {
        return Err("Startup is restricted to the official application".into());
    }
    let mut navigate = None;
    let mut deadline = None;
    let reply = {
        let state = window.app_handle().state::<StartupState>();
        let mut sessions = state.0.lock().map_err(|_| "Startup state is unavailable")?;
        let session = sessions
            .get_mut(window.label())
            .ok_or("This window has no startup session")?;
        let mut accepted = true;
        match event.as_str() {
            "status" if local => {}
            "begin" if !local => {
                deadline = Some(session.advance(Phase::Loading));
            }
            "open" if local && generation == Some(session.generation) => {
                if session.phase == Phase::Boot {
                    deadline = Some(session.advance(Phase::Loading));
                    navigate = Some(session.target.clone());
                } else {
                    accepted = false;
                }
            }
            "retry" if generation == Some(session.generation) => {
                if local {
                    deadline = Some(session.advance(Phase::Loading));
                    navigate = Some(session.target.clone());
                } else {
                    session.advance(Phase::Boot);
                    navigate = Some(local_url()?);
                }
            }
            "ready" if !local => {
                accepted = generation.is_some_and(|value| session.ready(value));
            }
            "open" | "retry" => accepted = false,
            _ => return Err("Unsupported startup event".into()),
        }
        StartupReply {
            generation: session.generation,
            timed_out: session.phase == Phase::TimedOut,
            accepted,
        }
    };
    if let Some(url) = navigate {
        if local && official_url(&url) {
            // Replace the local launch entry so Back never returns to a
            // completed boot page. The URL comes only from native config.
            let value = serde_json::to_string(url.as_str()).map_err(|error| error.to_string())?;
            window
                .eval(format!("window.location.replace({value})"))
                .map_err(|error| error.to_string())?;
        } else {
            window.navigate(url).map_err(|error| error.to_string())?;
        }
    }
    if let Some(value) = deadline {
        schedule_timeout(window, value);
    }
    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ExistingAssets;

    impl Assets<tauri::Wry> for ExistingAssets {
        fn get(&self, key: &AssetKey) -> Option<Cow<'_, [u8]>> {
            (key.as_ref() == "/existing.txt").then_some(Cow::Borrowed(b"original"))
        }
        fn iter(&self) -> Box<AssetsIter<'_>> {
            Box::new(std::iter::once((
                Cow::Borrowed("/existing.txt"),
                Cow::Borrowed(b"original".as_slice()),
            )))
        }
        fn csp_hashes(&self, _: &AssetKey) -> Box<dyn Iterator<Item = CspHash<'_>> + '_> {
            Box::new(std::iter::once(CspHash::Script("existing-hash")))
        }
    }

    #[test]
    fn startup_assets_preserve_existing_assets_and_csp() {
        let assets = StartupAssets {
            fallback: Box::new(ExistingAssets),
        };
        assert_eq!(
            assets
                .get(&AssetKey::from("existing.txt"))
                .unwrap()
                .as_ref(),
            b"original"
        );
        assert!(assets
            .get(&AssetKey::from(ASSET_NAME))
            .unwrap()
            .starts_with(b"<!doctype html>"));
        assert!(assets.get(&AssetKey::from("arbitrary.html")).is_none());
        assert_eq!(assets.iter().count(), 2);
        assert_eq!(
            assets
                .csp_hashes(&AssetKey::from("existing.txt"))
                .next()
                .unwrap()
                .hash(),
            "existing-hash"
        );
    }

    #[test]
    fn only_official_configured_home_windows_launch_locally() {
        assert!(configured_target("web", "https://wallpaper.061129.xyz/desktop", true).is_some());
        for url in [
            "http://wallpaper.061129.xyz/",
            "https://wallpaper.061129.xyz:444/",
            "https://wallpaper.061129.xyz.evil.test/",
            "https://user@wallpaper.061129.xyz/",
            "https://weekly.tw93.fun/",
        ] {
            assert!(configured_target("web", url, true).is_none(), "{url}");
        }
        assert!(configured_target("local", "https://wallpaper.061129.xyz/", true).is_none());
        assert!(configured_target("web", "https://wallpaper.061129.xyz/", false).is_none());
        assert!(is_local_page(&local_url().unwrap()));
        assert!(!is_local_page(
            &Url::parse("https://evil.test/wallpaper-startup.html").unwrap()
        ));
    }

    #[test]
    fn startup_ipc_is_available_only_to_local_and_official_home_webviews() {
        use tauri::ipc::Origin;
        let mut context: Context<tauri::Wry> = tauri::generate_context!();
        let authority = context.runtime_authority_mut();
        for label in ["pake", "pake-1", "pake-42"] {
            assert!(authority
                .resolve_access("wallpaper_startup", label, label, &Origin::Local)
                .is_some());
            for (url, allowed) in [
                ("https://wallpaper.061129.xyz/desktop", true),
                ("http://wallpaper.061129.xyz/", false),
                ("https://wallpaper.061129.xyz:444/", false),
                ("https://wallpaper.061129.xyz.evil.test/", false),
                ("https://accounts.google.com/", false),
            ] {
                let source = Origin::Remote {
                    url: Url::parse(url).unwrap(),
                };
                assert_eq!(
                    authority
                        .resolve_access("wallpaper_startup", label, label, &source)
                        .is_some(),
                    allowed,
                    "{label} {url}"
                );
            }
        }
        assert!(authority
            .resolve_access(
                "wallpaper_startup",
                "unrelated",
                "unrelated",
                &Origin::Local
            )
            .is_none());
    }

    #[test]
    fn late_ready_and_timeout_cannot_change_a_new_navigation() {
        let mut session = Session {
            target: Url::parse("https://wallpaper.061129.xyz/").unwrap(),
            generation: 1,
            phase: Phase::Boot,
            reveal_claimed: false,
        };
        let old = session.advance(Phase::Loading);
        let current = session.advance(Phase::Loading);
        assert!(!session.ready(old));
        assert!(!session.timeout(old));
        assert!(session.ready(current));
        assert!(!session.timeout(current));
        assert!(!session.ready(current));
        let next = session.advance(Phase::Loading);
        assert!(session.timeout(next));
        assert!(!session.ready(next));
        assert!(session.phase == Phase::TimedOut);
    }
}
