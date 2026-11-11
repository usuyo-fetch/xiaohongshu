//! 按宿主的方式起插件进程：清空环境，代理指向一个关着的本机端口，
//! 万一哪条路径真的发了请求，只会失败，不会连到小红书。这里的调用都不该联网。

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use usuyo_plugin::protocol::{Canonicalized, PluginManifest};

const ID: &str = "671234567890abcdef012345";
const DEAD_PROXY: &str = "http://127.0.0.1:9";

fn plugin(args: &[&str], secrets: Option<&Path>) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_usuyo-fetch-xiaohongshu"));
    cmd.env_clear()
        .env("USUYO_PROTOCOL", "1")
        .env("USUYO_LOCALE", "zh-CN")
        .env("HTTP_PROXY", DEAD_PROXY)
        .env("HTTPS_PROXY", DEAD_PROXY)
        .env("ALL_PROXY", DEAD_PROXY)
        .args(args);
    if let Some(s) = secrets {
        cmd.env("USUYO_SECRETS", s);
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
        &["fetch", &input, "--out", dir.path().to_str().unwrap()],
        None,
    );
    assert_eq!(out.status.code(), Some(1));
    let value = envelope(&out);
    assert_eq!(value["error"]["code"], "not-found");
    assert_eq!(value["error"]["reason"], "link-expired");
    assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none());
}

#[test]
fn a_malformed_visitor_cookie_is_reported_before_any_request() {
    let dir = tempfile::tempdir().unwrap();
    let secrets = dir.path().join("secrets.json");
    std::fs::write(&secrets, r#"{"visitor-cookie": "a1=x\r\nHost: evil"}"#).unwrap();
    let out_dir = dir.path().join("out");
    std::fs::create_dir(&out_dir).unwrap();
    let input = format!("https://www.xiaohongshu.com/explore/{ID}?xsec_token=x");
    let out = plugin(
        &["fetch", &input, "--out", out_dir.to_str().unwrap()],
        Some(&secrets),
    );
    let value = envelope(&out);
    assert_eq!(value["error"]["code"], "dependency-missing");
    assert_eq!(value["error"]["needs"][0], "setting:visitor-cookie");
}

#[test]
fn health_reads_only_local_settings() {
    let m = manifest();
    let dir = tempfile::tempdir().unwrap();
    let out = plugin(&["health"], None);
    let value = envelope(&out);
    assert_eq!(value["ok"], true);
    assert_eq!(value["ready"], true);
    assert_eq!(value["protocol"], 1);
    assert_eq!(value["version"], m.header.version.to_string());
    assert_eq!(value["checks"][0]["name"], "visitor-cookie");

    let secrets = dir.path().join("secrets.json");
    std::fs::write(
        &secrets,
        r#"{"visitor-cookie": "a1=x; web_session=secret-value"}"#,
    )
    .unwrap();
    let out = plugin(&["health", "--probe"], Some(&secrets));
    let text = String::from_utf8(out.stdout.clone()).unwrap();
    assert!(!text.contains("secret-value"));
    let value = envelope(&out);
    assert_eq!(value["ready"], true);
    assert_eq!(value["checks"][0]["message"], "填了访客 cookie");
    assert_eq!(value["checks"][1]["name"], "probe");

    std::fs::write(&secrets, r#"{"visitor-cookie": "no-equals-sign"}"#).unwrap();
    assert_eq!(
        envelope(&plugin(&["health"], Some(&secrets)))["ready"],
        false
    );
}

#[test]
fn usage_errors_exit_2() {
    for args in [
        &["fetch"][..],
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
