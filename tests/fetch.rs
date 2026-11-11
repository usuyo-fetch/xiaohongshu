//! 对着本机的假小红书跑 canonicalize 和 fetch：解析、重定向 → 访客 cookie 重试 → visitor-required、
//! 错误映射、输出目录里有什么。

mod common;

use std::collections::BTreeSet;
use std::path::Path;

use common::{COOKIE, Fake, ID, JPEG, MP4, WEBP};
use tempfile::TempDir;
use usuyo_fetch_xiaohongshu::fetch::{self, Request};
use usuyo_fetch_xiaohongshu::{Error, Lang, link};
use usuyo_plugin::protocol::{
    Canonicalized, Completeness, ErrorCode, FetchOutput, Identity, ItemKind, MediaKind,
    NotFoundReason, OutputKind,
};
use usuyo_plugin_kit::http::Cookie;

struct Got {
    result: Result<FetchOutput, Error>,
    dir: TempDir,
}

fn run(fake: &Fake, input: &str, cookie: Option<&str>, want: Option<&[OutputKind]>) -> Got {
    let dir = tempfile::tempdir().unwrap();
    let host = url::Url::parse(&fake.web).unwrap();
    let cookie = cookie.map(|c| Cookie::new(c, [host.host_str().unwrap()]));
    let result = fetch::fetch(
        &fake.site(),
        Request {
            input,
            out: dir.path(),
            want: want.map(|w| w.iter().copied().collect()),
            visitor_cookie: cookie.as_ref(),
        },
    );
    Got { result, dir }
}

fn ok(got: &Got) -> &FetchOutput {
    match &got.result {
        Ok(out) => out,
        Err(e) => panic!("expected success, got {e:?}"),
    }
}

fn failure(got: &Got) -> usuyo_plugin::protocol::CallFailure {
    match &got.result {
        Err(e) => e.to_failure(Lang::Zh),
        Ok(out) => panic!("expected a failure, got {out:?}"),
    }
}

/// 输出目录里的全部文件，相对路径，排好序；顺带确认没有符号链接。
fn files(dir: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let kind = entry.file_type().unwrap();
            assert!(!kind.is_symlink(), "{:?}", entry.path());
            if kind.is_dir() {
                walk(root, &entry.path(), out);
            } else {
                let rel = entry
                    .path()
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                out.push(rel);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

fn read_json(dir: &Path, name: &str) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(dir.join(name)).unwrap()).unwrap()
}

#[test]
fn image_note_with_js_literal_quirks() {
    let fake = Fake::start();
    let got = run(&fake, &fake.note_url("ok"), None, None);
    let out = ok(&got);

    assert_eq!(out.identity, Some(link::identity(ID)));
    assert_eq!(out.kind, ItemKind::Post);
    assert_eq!(out.title.as_deref(), Some("手冲咖啡的三段式注水"));
    let text = out.text.as_deref().unwrap();
    // 字符串里的 undefined 和 new Set([]) 原样留着
    assert!(
        text.contains("别把 undefined 和 new Set([]) 当成配方"),
        "{text}"
    );
    let author = out.author.as_ref().unwrap();
    assert_eq!(author.name, "咖啡日常");
    assert_eq!(
        author.platform_id.as_deref(),
        Some("5b56be0014de415b2db830a6")
    );
    assert_eq!(
        author.url.as_deref(),
        Some("https://www.xiaohongshu.com/user/profile/5b56be0014de415b2db830a6")
    );
    assert_eq!(
        out.published_at.as_ref().unwrap().as_str(),
        "2026-10-01T08:00:00Z"
    );
    assert_eq!(out.platform_tags, ["手冲咖啡", "咖啡日常"]);

    // 第三张图的主机不在名单里，不去取，如实写进原因
    assert_eq!(out.images.len(), 2);
    assert_eq!(out.completeness, Completeness::Partial);
    assert_eq!(out.missing, [OutputKind::Video]);
    let reason = out.missing_reason.as_deref().unwrap();
    assert!(reason.contains("有 1 张图片没有下载下来"), "{reason}");
    assert!(reason.contains("这是图文笔记，没有视频"), "{reason}");
    assert!(out.media.is_empty());
    assert_eq!(out.evidence.status, Some(200));

    let dir = got.dir.path();
    assert_eq!(
        files(dir),
        [
            "images/001.webp",
            "images/002.jpg",
            "meta.json",
            "note.json"
        ]
    );
    assert_eq!(std::fs::read(dir.join("images/001.webp")).unwrap(), WEBP);
    assert_eq!(std::fs::read(dir.join("images/002.jpg")).unwrap(), JPEG);
    let meta = read_json(dir, "meta.json");
    assert_eq!(meta["counts"]["liked"], "1.2万");
    assert_eq!(meta["ipLocation"], "上海");
    assert_eq!(meta["failed"][0]["url"], "https://evil.example/c.jpg");
    assert_eq!(meta["visitorCookieUsed"], false);
    assert_eq!(read_json(dir, "note.json")["noteId"], ID);

    // 一次笔记页请求，图片只请求名单内的两张，图片请求带源作 Referer、不带 cookie
    assert_eq!(fake.page_requests().len(), 1);
    let images: Vec<_> = fake
        .seen()
        .into_iter()
        .filter(|s| s.target.starts_with("/img/"))
        .collect();
    assert_eq!(images.len(), 2);
    assert!(images.iter().all(|s| s.cookie.is_none()));
    assert!(
        images
            .iter()
            .all(|s| s.referer.as_deref() == Some(&format!("{}/", fake.web)[..]))
    );
    // 输出的 JSON 能按协议的类型读回来
    let json = serde_json::to_string(out).unwrap();
    assert_eq!(&serde_json::from_str::<FetchOutput>(&json).unwrap(), out);
}

#[test]
fn video_note_uses_page_streams_only() {
    let fake = Fake::start();
    let got = run(&fake, &fake.note_url("video"), None, None);
    let out = ok(&got);
    assert_eq!(out.kind, ItemKind::Video);
    assert_eq!(
        out.completeness,
        Completeness::Full,
        "{:?}",
        out.missing_reason
    );
    assert_eq!(out.author.as_ref().unwrap().name, "咖啡日常");
    assert_eq!(out.media.len(), 1);
    let video = &out.media[0];
    assert_eq!(
        (video.path.as_str(), video.kind),
        ("media/video.mp4", MediaKind::Video)
    );
    assert_eq!(video.mime.as_deref(), Some("video/mp4"));
    assert_eq!(video.duration_sec, Some(181.0));
    let dir = got.dir.path();
    assert_eq!(
        files(dir),
        [
            "images/001.jpg",
            "media/video.mp4",
            "meta.json",
            "note.json"
        ]
    );
    assert_eq!(std::fs::read(dir.join("media/video.mp4")).unwrap(), MP4);

    // h264 里最高的那一路：主地址 500，换备用地址；不拼 originVideoKey，不碰 h265
    let media: Vec<String> = fake
        .seen()
        .into_iter()
        .filter(|s| s.target.starts_with("/media/"))
        .map(|s| s.target)
        .collect();
    assert_eq!(media, ["/media/broken.mp4", "/media/v.mp4"]);
}

#[test]
fn live_photos_are_saved_as_video() {
    let fake = Fake::start();
    let got = run(&fake, &fake.note_url("live"), None, None);
    let out = ok(&got);
    assert_eq!(
        out.completeness,
        Completeness::Full,
        "{:?}",
        out.missing_reason
    );
    assert_eq!(out.media.len(), 1);
    assert_eq!(out.media[0].path.as_str(), "media/live-002.mp4");
    assert_eq!(
        files(got.dir.path()),
        [
            "images/001.jpg",
            "images/002.webp",
            "media/live-002.mp4",
            "meta.json",
            "note.json"
        ]
    );
}

#[test]
fn video_note_without_streams_is_partial() {
    let fake = Fake::start();
    let got = run(&fake, &fake.note_url("nostream"), None, None);
    let out = ok(&got);
    assert_eq!(out.completeness, Completeness::Partial);
    assert_eq!(out.missing, [OutputKind::Video]);
    assert_eq!(out.missing_reason.as_deref(), Some("页面没有给出视频地址"));
    assert_eq!(out.images.len(), 1);
    assert!(fake.seen().iter().all(|s| !s.target.starts_with("/media/")));
}

#[test]
fn want_limits_what_is_downloaded() {
    let fake = Fake::start();
    let got = run(
        &fake,
        &fake.note_url("video"),
        None,
        Some(&[OutputKind::Text]),
    );
    let out = ok(&got);
    assert_eq!(out.completeness, Completeness::Full);
    assert!(out.images.is_empty() && out.media.is_empty() && out.title.is_none());
    assert_eq!(files(got.dir.path()), ["note.json"]);
    assert_eq!(fake.seen().len(), 1, "{:?}", fake.seen());
}

#[test]
fn same_note_redirect_is_followed() {
    let fake = Fake::start();
    let input = format!("{}/discovery/item/{ID}?xsec_token=hop", fake.web);
    let got = run(&fake, &input, None, Some(&[OutputKind::Text]));
    assert!(ok(&got).text.is_some());
    assert_eq!(fake.page_requests().len(), 2);
}

#[test]
fn home_redirect_retries_once_with_the_visitor_cookie() {
    let fake = Fake::start();
    let got = run(
        &fake,
        &fake.note_url("needcookie"),
        Some(COOKIE),
        Some(&[OutputKind::Text, OutputKind::Meta]),
    );
    let out = ok(&got);
    assert!(out.text.is_some());
    let pages = fake.page_requests();
    assert_eq!(pages.len(), 2);
    assert_eq!(pages[0].cookie, None);
    assert_eq!(pages[1].cookie.as_deref(), Some(COOKIE));
    // 重定向的去处（首页）没有被请求
    assert!(pages.iter().all(|p| p.target != "/"));
    assert_eq!(
        read_json(got.dir.path(), "meta.json")["visitorCookieUsed"],
        true
    );
    // cookie 不出现在输出和输出目录里
    assert!(
        !serde_json::to_string(out)
            .unwrap()
            .contains("visitor-session-secret")
    );
    for f in files(got.dir.path()) {
        let bytes = std::fs::read(got.dir.path().join(f)).unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("visitor-session-secret"));
    }
}

#[test]
fn empty_note_content_also_takes_the_cookie_path() {
    let fake = Fake::start();
    let got = run(
        &fake,
        &fake.note_url("absent"),
        Some(COOKIE),
        Some(&[OutputKind::Text]),
    );
    assert!(ok(&got).text.is_some());
    assert_eq!(fake.page_requests().len(), 2);

    let fake = Fake::start();
    let f = failure(&run(&fake, &fake.note_url("absent"), None, None));
    assert_eq!(f.code, ErrorCode::VisitorRequired);
    assert_eq!(fake.page_requests().len(), 1);
}

#[test]
fn without_a_cookie_home_means_visitor_required() {
    let fake = Fake::start();
    let f = failure(&run(&fake, &fake.note_url("needcookie"), None, None));
    assert_eq!(f.code, ErrorCode::VisitorRequired);
    assert!(f.hint.as_deref().unwrap().contains("访客身份"), "{f:?}");
    assert_eq!(fake.page_requests().len(), 1);
}

#[test]
fn a_stale_cookie_is_tried_once_then_reported() {
    for token in ["home", "login"] {
        let fake = Fake::start();
        let f = failure(&run(&fake, &fake.note_url(token), Some(COOKIE), None));
        assert_eq!(f.code, ErrorCode::VisitorRequired, "{token}");
        assert!(f.hint.as_deref().unwrap().contains("过期"), "{f:?}");
        let pages = fake.page_requests();
        assert_eq!(pages.len(), 2, "{token}: {pages:?}");
        assert!(
            pages
                .iter()
                .all(|p| !p.target.starts_with("/login") && p.target != "/explore")
        );
    }
}

#[test]
fn error_mapping() {
    let cases: [(&str, ErrorCode, Option<NotFoundReason>); 11] = [
        (
            "expired",
            ErrorCode::NotFound,
            Some(NotFoundReason::LinkExpired),
        ),
        ("sec", ErrorCode::Blocked, None),
        ("captcha", ErrorCode::Blocked, None),
        ("risk", ErrorCode::Blocked, None),
        ("verify", ErrorCode::Blocked, None),
        ("gateway", ErrorCode::Blocked, None),
        ("gone", ErrorCode::NotFound, None),
        ("limited", ErrorCode::RateLimited, None),
        ("changed", ErrorCode::UpstreamChanged, None),
        ("noscript", ErrorCode::UpstreamChanged, None),
        ("nonexistent-token", ErrorCode::NotFound, None),
    ];
    for (token, code, reason) in cases {
        let fake = Fake::start();
        // 带着 cookie 也不该重试：这些都不是「被送回首页」
        let f = failure(&run(&fake, &fake.note_url(token), Some(COOKIE), None));
        assert_eq!((f.code, f.reason), (code, reason), "{token}: {f:?}");
        assert_eq!(fake.page_requests().len(), 1, "{token}");
        if token == "limited" {
            assert_eq!(f.retry_after, Some(600));
        }
        if code == ErrorCode::Blocked || code == ErrorCode::UpstreamChanged {
            assert!(f.hint.is_some(), "{token}");
        }
    }
}

#[test]
fn links_without_xsec_token_are_not_requested() {
    let fake = Fake::start();
    let f = failure(&run(
        &fake,
        &format!("{}/explore/{ID}", fake.web),
        Some(COOKIE),
        None,
    ));
    assert_eq!(
        (f.code, f.reason),
        (ErrorCode::NotFound, Some(NotFoundReason::LinkExpired))
    );
    assert!(fake.seen().is_empty());
}

#[test]
fn rejects_what_it_cannot_take() {
    let fake = Fake::start();
    let f = failure(&run(&fake, "https://example.com/explore/x", None, None));
    assert_eq!(f.code, ErrorCode::UnsupportedInput);
    let f = failure(&run(
        &fake,
        &fake.note_url("ok"),
        None,
        Some(&[OutputKind::Html]),
    ));
    assert_eq!(f.code, ErrorCode::InvalidArgs);
    // 真站点的链接对着本机配置的 Site 不认
    let real = format!("https://www.xiaohongshu.com/explore/{ID}?xsec_token=ok");
    assert_eq!(
        failure(&run(&fake, &real, None, None)).code,
        ErrorCode::UnsupportedInput
    );
    assert!(fake.seen().is_empty());
}

#[test]
fn short_links_expand_without_touching_the_note_host() {
    let fake = Fake::start();
    let site = fake.site();
    for path in ["/abc", "/hop"] {
        let got = link::canonicalize(&site, &format!("{}{path}", fake.short)).unwrap();
        let Canonicalized::Match {
            identity,
            resolved_url: Some(resolved),
        } = got
        else {
            panic!("{got:?}");
        };
        assert_eq!(identity, link::identity(ID));
        assert!(
            matches!(&identity, Identity::Platform { url: Some(u), .. } if u == &format!("https://www.xiaohongshu.com/explore/{ID}"))
        );
        // 展开后的全部参数都留着，xsec_token 在里面
        assert_eq!(
            resolved,
            format!(
                "{}/discovery/item/{ID}?app_platform=ios&xsec_token=ok&xsec_source=app_share&shareRedId=xyz",
                fake.web
            )
        );
    }
    assert!(
        fake.seen()
            .iter()
            .all(|s| s.server == "short" && s.cookie.is_none())
    );

    assert_eq!(
        link::canonicalize(&site, &format!("{}/profile", fake.short)).unwrap(),
        Canonicalized::NoMatch
    );
    assert_eq!(
        link::canonicalize(&site, &format!("{}/elsewhere", fake.short)).unwrap(),
        Canonicalized::NoMatch
    );
    let code = |path: &str| {
        link::canonicalize(&site, &format!("{}{path}", fake.short))
            .unwrap_err()
            .to_failure(Lang::Zh)
            .code
    };
    assert_eq!(code("/gone"), ErrorCode::NotFound);
    assert_eq!(code("/captcha"), ErrorCode::Blocked);
    assert_eq!(code("/loop"), ErrorCode::UpstreamChanged);
    assert!(fake.page_requests().is_empty());
}

#[test]
fn fetch_takes_a_short_link_and_sends_no_cookie_to_it() {
    let fake = Fake::start();
    let got = run(
        &fake,
        &format!("{}/abc", fake.short),
        Some(COOKIE),
        Some(&[OutputKind::Text]),
    );
    assert!(ok(&got).text.is_some());
    let seen = fake.seen();
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert!(seen.iter().all(|s| s.cookie.is_none()));
    // 分享链接里指向分享人的参数不发给笔记页
    let page = &fake.page_requests()[0];
    assert_eq!(
        page.target,
        format!("/discovery/item/{ID}?xsec_token=ok&xsec_source=app_share")
    );
    let want: BTreeSet<_> = ["short", "web"].into();
    assert_eq!(seen.iter().map(|s| s.server).collect::<BTreeSet<_>>(), want);
}

#[test]
fn english_hints_follow_the_locale() {
    let fake = Fake::start();
    let got = run(&fake, &fake.note_url("needcookie"), None, None);
    let hint = got.result.unwrap_err().to_failure(Lang::En).hint.unwrap();
    assert!(hint.contains("visitor identity"), "{hint}");
}
