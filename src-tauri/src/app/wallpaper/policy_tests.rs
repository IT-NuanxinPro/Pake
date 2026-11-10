use super::*;

fn item(path: &str) -> WallpaperItem {
    WallpaperItem {
        asset_key: format!("desktop:path:/wallpaper/desktop/{path}"),
        url: format!("https://wallpaper.061129.xyz/images/v1.2.3/wallpaper/desktop/{path}"),
        title: "Test".into(),
    }
}

#[test]
fn source_requires_exact_https_origin() {
    for source in [
        "https://wallpaper.061129.xyz/",
        "https://wallpaper.061129.xyz:443/wallpaper/a",
    ] {
        assert!(validate_source(&Url::parse(source).unwrap()).is_ok());
    }
    for source in [
        "http://wallpaper.061129.xyz/",
        "https://wallpaper.061129.xyz:444/",
        "https://wallpaper.061129.xyz.evil.test/",
        "https://user@wallpaper.061129.xyz/",
        "file:///tmp/a",
    ] {
        assert!(
            validate_source(&Url::parse(source).unwrap()).is_err(),
            "{source}"
        );
    }
}

#[test]
fn repository_paths_bind_identity() {
    assert!(validate_item(&item("nature/%E5%B1%B1.jpg")).is_ok());
    for prefix in [
        "https://cdn.jsdelivr.net/gh/IT-NuanxinPro/nuanXinProPic@",
        "https://raw.githubusercontent.com/IT-NuanxinPro/nuanXinProPic/",
    ] {
        let mut value = item("a.png");
        value.url = format!("{prefix}v2.0.0/wallpaper/desktop/a.png");
        assert!(validate_item(&value).is_ok());
    }
    let mut value = item("a.jpg");
    value.asset_key = "desktop:path:/wallpaper/desktop/b.jpg".into();
    assert!(validate_item(&value).is_err());
}

#[test]
fn encoded_filename_delimiters_are_not_url_query_or_fragment() {
    for filename in [
        "%237743.jpg",
        "question%3F.jpg",
        "question%3f.jpg",
        "mixed%23%3F.png",
    ] {
        let value = item(filename);
        assert!(validate_item(&value).is_ok(), "{filename}");
        for suffix in ["?download=1", "#preview", "?", "#"] {
            let mut with_suffix = value.clone();
            with_suffix.url.push_str(suffix);
            assert!(validate_item(&with_suffix).is_err(), "{filename}{suffix}");
        }
    }
    for filename in [
        "#7743.jpg",
        "question?.jpg",
        "%23%252fsecret.jpg",
        "%3F%255csecret.jpg",
        "%23%253asecret.jpg",
    ] {
        assert!(validate_item(&item(filename)).is_err(), "{filename}");
    }
}

#[test]
fn unsafe_urls_and_nested_encodings_are_rejected() {
    for path in [
        "../a.jpg",
        "%2e%2e/a.jpg",
        "%252e%252e/a.jpg",
        "a%2fb.jpg",
        "a%255cb.jpg",
        "a%00.jpg",
        "a%253ab.jpg",
        "a.svg",
        "a.gif",
        "a.jpg?x=1",
        "a.jpg#x",
        "a.webp",
        "a//b.jpg",
        "a\\b.jpg",
    ] {
        assert!(validate_item(&item(path)).is_err(), "{path}");
    }
    for url in [
        "https://evil.test/a.jpg",
        "https://wallpaper.061129.xyz/images/main/wallpaper/desktop/a.jpg",
        "https://user@wallpaper.061129.xyz/images/v1.2.3/wallpaper/desktop/a.jpg",
        "https://cn.bing.com/th?id=OHR.Test_UHD.jpg",
    ] {
        let mut value = item("a.jpg");
        value.url = url.into();
        assert!(validate_item(&value).is_err());
    }
}

#[test]
fn queues_are_bounded_deduplicated_and_disable_accepts_empty() {
    let config = RotationConfig {
        enabled: true,
        interval_minutes: 30,
        items: vec![item("a.jpg"), item("a.jpg")],
    };
    assert_eq!(validate_config(config).unwrap().items.len(), 1);
    assert!(validate_config(RotationConfig {
        enabled: false,
        interval_minutes: 0,
        items: vec![]
    })
    .is_ok());
    for interval in [0, 1, 14, 16, 61, u64::MAX] {
        assert!(validate_config(RotationConfig {
            enabled: true,
            interval_minutes: interval,
            items: vec![item("a.jpg")]
        })
        .is_err());
    }
    assert!(validate_config(RotationConfig {
        enabled: true,
        interval_minutes: 15,
        items: vec![item("a.jpg"); 51]
    })
    .is_err());
    assert!(validate_config(RotationConfig {
        enabled: true,
        interval_minutes: 15,
        items: vec![]
    })
    .is_err());
}

#[test]
fn body_requires_matching_raster_signature_and_limit() {
    assert!(validate_image("image/jpeg", "jpg", &[0xff, 0xd8, 0xff, 0xe0]).is_ok());
    assert!(validate_image("image/png", "png", b"\x89PNG\r\n\x1a\n").is_ok());
    assert!(validate_image("image/jpeg", "jpg", b"<html>").is_err());
    assert!(validate_image("text/html", "jpg", &[0xff, 0xd8, 0xff]).is_err());
    assert!(validate_image("image/png", "jpg", b"\x89PNG\r\n\x1a\n").is_err());
    assert!(validate_image("image/jpeg", "jpg", &vec![0; MAX_IMAGE_BYTES + 1]).is_err());
}
