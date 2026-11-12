//! 按宿主的方式起插件进程：清空环境，代理指向一个关着的本机端口，
//! 万一哪条路径真的发了请求，只会失败，不会连到小红书。这里的调用都不该联网。

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use usuyo_plugin::protocol::{Canonicalized, PluginManifest};

const ID: &str = "671234567890abcdef012345";
const DEAD_PROXY: &str = "http://127.0.0.1:9";

fn plugin(args: &[&str], visitor: Option<&Path>) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_usuyo-fetch-xiaohongshu"));
    cmd.env_clear()
        .env("USUYO_PROTOCOL", "1")
        .env("USUYO_LOCALE", "zh-CN")
        .env("HTTP_PROXY", DEAD_PROXY)
        .env("HTTPS_PROXY", DEAD_PROXY)
        .env("ALL_PROXY", DEAD_PROXY)
        .args(args);
    if let Some(v) = visitor {
        cmd.env("USUYO_VISITOR", v);
    }
    cmd.output().unwrap()
}

fn envelope(out: &Output) -> serde_json::Value {
    let stdout = String::from_utf8(out.stdout.clone()).unwrap();
    assert_eq!(
        stdout.lines().count(),
        1,
        "stdout is one JSON value: {stdout}"
    );
    serde_json::from_str(&stdout).unwrap()
}

fn manifest() -> PluginManifest {
    let text = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("usuyo-plugin.yaml"),
    )
    .unwrap();
    let parsed = PluginManifest::from_yaml(&text).unwrap();
    assert!(
        parsed.unknown_fields.is_empty(),
        "{:?}",
        parsed.unknown_fields
    );
    parsed.value
}

#[test]
fn canonicalize_long_links() {
    for input in [
        format!("https://www.xiaohongshu.com/explore/{ID}?xsec_token=AB%3D"),
        format!("https://www.xiaohongshu.com/search_result/{ID}?xsec_token=x"),
        format!("https://www.xiaohongshu.com/discovery/item/{ID}?xsec_token=x"),
        format!(
            "https://www.xiaohongshu.com/user/profile/5b56be0014de415b2db830a6/{ID}?xsec_token=x"
        ),
    ] {
        let out = plugin(&["canonicalize", &input], None);
        assert!(out.status.success(), "{input}: {out:?}");
        let mut value = envelope(&out);
        assert_eq!(value["ok"], true);
        assert_eq!(value["identity"]["platform"], "xiaohongshu");
        assert_eq!(value["identity"]["contentId"], ID);
        assert_eq!(
            value["identity"]["url"],
            format!("https://www.xiaohongshu.com/explore/{ID}")
        );
        value.as_object_mut().unwrap().remove("ok");
        let parsed: Canonicalized = serde_json::from_value(value).unwrap();
        assert!(matches!(
            parsed,
            Canonicalized::Match {
                resolved_url: None,
                ..
            }
        ));
    }
    let out = plugin(&["canonicalize", "https://example.com/a"], None);
    assert_eq!(
        envelope(&out),
        serde_json::json!({"ok": true, "match": false})
    );
}

#[test]
fn fetch_without_xsec_token_answers_without_network() {
    let dir = tempfile::tempdir().unwrap();
    let input = format!("https://www.xiaohongshu.com/explore/{ID}");
    let out = plugin(
        &[
            "fetch",
            &input,
            "--out",
            dir.path().to_str().unwrap(),
            "--method",
            "note-page",
        ],
        None,
    );
    assert_eq!(out.status.code(), Some(1));
    let value = envelope(&out);
    assert_eq!(value["error"]["code"], "not-found");
    assert_eq!(value["error"]["reason"], "link-expired");
    assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none());
}

/// 宿主交来的访客身份文件（`USUYO_VISITOR`，和 `USUYO_CREDENTIALS` 一个形状）。
fn visitor_file(dir: &Path, value: &str) -> PathBuf {
    let path = dir.join("visitor.json");
    let file = serde_json::json!({
        "sites": [{"site": "www.xiaohongshu.com", "source": "visitor-window"}],
        "storageState": {"cookies": [
            {"name": "a1", "value": value, "domain": ".xiaohongshu.com", "path": "/"}]}
    });
    std::fs::write(&path, file.to_string()).unwrap();
    path
}

#[test]
fn a_malformed_visitor_identity_is_reported_before_any_request() {
    let dir = tempfile::tempdir().unwrap();
    let visitor = visitor_file(dir.path(), "visitor-secret-1\r\nHost: evil");
    let out_dir = dir.path().join("out");
    std::fs::create_dir(&out_dir).unwrap();
    let input = format!("https://www.xiaohongshu.com/explore/{ID}?xsec_token=x");
    let out = plugin(
        &[
            "fetch",
            &input,
            "--out",
            out_dir.to_str().unwrap(),
            "--method",
            "note-page",
        ],
        Some(&visitor),
    );
    let value = envelope(&out);
    assert_eq!(value["error"]["code"], "internal");
    let text = format!("{value} {}", String::from_utf8_lossy(&out.stderr));
    assert!(!text.contains("visitor-secret-1"), "{text}");
    assert!(std::fs::read_dir(&out_dir).unwrap().next().is_none());

    // 形状对的访客身份读得进来，没有 xsec_token 的链接照样不联网就答
    let visitor = visitor_file(dir.path(), "visitor-secret-1");
    let input = format!("https://www.xiaohongshu.com/explore/{ID}");
    let out = plugin(
        &[
            "fetch",
            &input,
            "--out",
            out_dir.to_str().unwrap(),
            "--method",
            "note-page",
        ],
        Some(&visitor),
    );
    assert_eq!(envelope(&out)["error"]["code"], "not-found");
}

#[test]
fn health_answers_without_requests() {
    let m = manifest();
    let out = plugin(&["health"], None);
    let value = envelope(&out);
    assert_eq!(value["ok"], true);
    assert_eq!(value["ready"], true);
    assert_eq!(value["protocol"], 1);
    assert_eq!(value["version"], m.header.version.to_string());
    assert_eq!(value["checks"], serde_json::json!([]));

    let value = envelope(&plugin(&["health", "--probe"], None));
    assert_eq!(value["ready"], true);
    assert_eq!(value["checks"][0]["name"], "probe");
}

#[test]
fn calls_for_other_methods_are_invalid_args() {
    let dir = tempfile::tempdir().unwrap();
    let input = format!("https://www.xiaohongshu.com/explore/{ID}");
    for method in ["userscript", "xhs-downloader", "nope"] {
        let out = plugin(
            &[
                "fetch",
                &input,
                "--out",
                dir.path().to_str().unwrap(),
                "--method",
                method,
            ],
            None,
        );
        assert_eq!(out.status.code(), Some(1), "{method}");
        assert_eq!(envelope(&out)["error"]["code"], "invalid-args", "{method}");
    }
    let out = plugin(&["sources", "--method", "note-page"], None);
    assert_eq!(envelope(&out)["error"]["code"], "invalid-args");
}

#[test]
fn usage_errors_exit_2() {
    for args in [
        &["fetch"][..],
        &[
            "fetch",
            "https://www.xiaohongshu.com/explore/1",
            "--out",
            "/o",
        ][..],
        &["list", "x"][..],
        &["health", "--x"][..],
        &[][..],
    ] {
        let out = plugin(args, None);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(out.stdout.is_empty());
    }
}

#[test]
fn manifest_loads_in_the_host_and_routes_every_link_shape() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("plugins/usuyo-fetch/xiaohongshu/0.1.0");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("usuyo-plugin.yaml"),
        dir.join("usuyo-plugin.yaml"),
    )
    .unwrap();
    std::fs::copy(
        env!("CARGO_BIN_EXE_usuyo-fetch-xiaohongshu"),
        dir.join("usuyo-fetch-xiaohongshu"),
    )
    .unwrap();
    let found = usuyo_plugin::discover(&tmp.path().join("plugins")).unwrap();
    assert!(found.rejected.is_empty(), "{:?}", found.rejected);
    assert_eq!(found.fetch.len(), 1);
    // 访客身份写在 `visitors` 里、只给笔记页那种方法，不是账号站点，也没有手填的设置
    let spec = &found.fetch[0].spec;
    assert!(spec.sites.is_empty());
    assert_eq!(spec.visitors[0].site, "www.xiaohongshu.com");
    // routes.md 第 3 节的每一行；做了的是笔记页和浏览器脚本
    let ids: Vec<&str> = spec.methods.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "note-page",
            "userscript",
            "iphone",
            "android",
            "xhs-downloader",
            "rsshub"
        ]
    );
    let built: Vec<&str> = spec.built().map(|m| m.id.as_str()).collect();
    assert_eq!(built, ["note-page", "userscript"]);
    let note = spec.method("note-page").unwrap();
    assert_eq!(note.visitors, ["www.xiaohongshu.com"]);
    assert!(note.sites.is_empty() && note.capabilities.fetch.is_some());
    assert!(found.fetch[0].header.settings.is_empty());
    let host = usuyo_plugin::PluginHost::new(
        usuyo_plugin::HostConfig {
            work_dir: tmp.path().join("work"),
            cache_dir: tmp.path().join("cache"),
            locale: "zh-CN".into(),
            limits: usuyo_plugin::CallLimits::default(),
        },
        found.fetch,
    );
    for link in [
        format!("https://www.xiaohongshu.com/explore/{ID}?xsec_token=x"),
        format!("https://www.xiaohongshu.com/search_result/{ID}?xsec_token=x"),
        format!("https://www.xiaohongshu.com/discovery/item/{ID}?xsec_token=x"),
        format!("https://www.xiaohongshu.com/user/profile/u/{ID}?xsec_token=x"),
        "https://xhslink.com/a/jqL6B32eU0F7".into(),
        "https://xhslink.cn/m/EXCcWbUXnl".into(),
    ] {
        let url = url::Url::parse(&link).unwrap();
        assert_eq!(host.candidates(&url).len(), 1, "{link}");
    }
    assert!(
        host.candidates(&url::Url::parse("https://example.com/").unwrap())
            .is_empty()
    );
}
