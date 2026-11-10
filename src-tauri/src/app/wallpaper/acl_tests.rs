use tauri::{ipc::Origin, Url};

const COMMANDS: [&str; 5] = [
    "wallpaper_capabilities",
    "wallpaper_apply",
    "wallpaper_restore",
    "wallpaper_configure_rotation",
    "wallpaper_rotation_status",
];

#[test]
fn wallpaper_acl_checks_request_origin_before_async_dispatch() {
    let mut context: tauri::Context<tauri::Wry> = tauri::generate_context!();
    let authority = context.runtime_authority_mut();
    for command in COMMANDS {
        for label in ["pake", "pake-1", "pake-42"] {
            for url in [
                "https://wallpaper.061129.xyz/",
                "https://wallpaper.061129.xyz/wallpaper/token",
            ] {
                assert!(
                    authority
                        .resolve_access(
                            command,
                            label,
                            label,
                            &Origin::Remote {
                                url: Url::parse(url).unwrap()
                            }
                        )
                        .is_some(),
                    "trusted {command} {label}"
                );
            }
            for url in [
                "https://evil.test/",
                "https://wallpaper.061129.xyz.evil.test/",
                "http://wallpaper.061129.xyz/",
                "https://wallpaper.061129.xyz:444/",
                "https://sub.wallpaper.061129.xyz/",
            ] {
                // A later navigation cannot change the request-origin used by Tauri ACL.
                let current_window_url = Url::parse("https://wallpaper.061129.xyz/").unwrap();
                assert!(super::policy::validate_source(&current_window_url).is_ok());
                let queued_request_origin = Origin::Remote {
                    url: Url::parse(url).unwrap(),
                };
                assert!(
                    authority
                        .resolve_access(command, label, label, &queued_request_origin)
                        .is_none(),
                    "untrusted {command} {url}"
                );
            }
            assert!(authority
                .resolve_access(command, label, label, &Origin::Local)
                .is_none());
        }
        assert!(authority
            .resolve_access(
                command,
                "unrelated",
                "unrelated",
                &Origin::Remote {
                    url: Url::parse("https://wallpaper.061129.xyz/").unwrap()
                }
            )
            .is_none());
    }
}

#[test]
fn legacy_commands_remain_available_without_granting_wallpaper() {
    let mut context: tauri::Context<tauri::Wry> = tauri::generate_context!();
    let authority = context.runtime_authority_mut();
    let external = Origin::Remote {
        url: Url::parse("https://example.com/").unwrap(),
    };
    for command in [
        "download_file",
        "send_notification",
        "increment_dock_badge",
        "set_dock_badge",
        "set_dock_badge_label",
        "clear_dock_badge",
        "update_theme_mode",
        "set_zoom",
        "webview_navigate",
    ] {
        assert!(
            authority
                .resolve_access(command, "pake", "pake", &external)
                .is_some(),
            "{command}"
        );
    }
    assert!(authority
        .resolve_access("wallpaper_apply", "pake", "pake", &external)
        .is_none());
}
