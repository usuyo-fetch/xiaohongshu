//! `xhs-downloader` 方法：全部离线。本机起一个假的 XHS-Downloader（形状照 `tests/fixtures/xhs-downloader/NOTES.md`），
//! 不运行真的 XHS-Downloader，不连小红书。库函数的测试把笔记页、短链、图片视频都指到本机；
//! 起插件进程的测试清空环境，代理指向一个关着的端口，万一哪条路径连了别处，只会失败。

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use usuyo_fetch_xiaohongshu::xhsdownloader::{self, Request, Service};
use usuyo_fetch_xiaohongshu::{Error, Lang, Site};
use usuyo_plugin::protocol::{Access, Account, Login, OutputKind, PluginKind, PluginManifest};
use usuyo_plugin_kit::http::Policy;

const ID: &str = "671234567890abcdef012345";
const TOKEN: &str = "payload.signature-TOKEN-0123";
const JPEG: &[u8] = b"\xff\xd8\xff\xe0fake-jpeg";
const MP4: &[u8] = b"\x00\x00\x00\x18ftypmp42fake-video-bytes";
const BIN: &str = env!("CARGO_BIN_EXE_usuyo-fetch-xiaohongshu");
const DEAD_PROXY: &str = "http://127.0.0.1:9";

fn fixture(name: &str, base: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/xhs-downloader")
        .join(name);
    std::fs::read_to_string(path)
        .unwrap()
        .replace("{BASE}", base)
        .replace("{ID}", ID)
}

#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: String,
}

struct Reply {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
    /// 回答之前先等多久。
    delay: Duration,
}

fn json_reply(body: impl Into<Vec<u8>>) -> Reply {
    Reply {
        status: 200,
        content_type: "application/json",
        body: body.into(),
        delay: Duration::ZERO,
    }
}

fn status_reply(status: u16, body: &str) -> Reply {
    Reply {
        status,
        ..json_reply(body)
    }
}

/// 本机的假 XHS-Downloader，也兼当图片视频的主机：`/img/*`、`/media/*`。
struct FakeService {
    base: String,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl FakeService {
    fn start(route: impl Fn(&Seen, &str) -> Reply + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let route = Arc::new(route);
        let (log, served) = (seen.clone(), base.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let (route, log, base) = (route.clone(), log.clone(), served.clone());
                std::thread::spawn(move || {
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        return;
                    }
                    let mut parts = line.split_whitespace();
                    let method = parts.next().unwrap_or_default().to_owned();
                    let path = parts.next().unwrap_or("/").to_owned();
                    let mut headers = BTreeMap::new();
                    loop {
                        let mut header = String::new();
                        if reader.read_line(&mut header).unwrap_or(0) == 0 || header == "\r\n" {
                            break;
                        }
                        if let Some((k, v)) = header.split_once(':') {
                            headers.insert(k.to_ascii_lowercase(), v.trim().to_owned());
                        }
                    }
                    let length: usize = headers
                        .get("content-length")
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(0);
                    let mut body = vec![0u8; length];
                    reader.read_exact(&mut body).ok();
                    let seen = Seen {
                        method,
                        path,
                        headers,
                        body: String::from_utf8_lossy(&body).into_owned(),
                    };
                    log.lock().unwrap().push(seen.clone());
                    let reply = route(&seen, &base);
                    std::thread::sleep(reply.delay);
                    let head = format!(
                        "HTTP/1.1 {} X\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        reply.status,
                        reply.content_type,
                        reply.body.len()
                    );
                    stream.write_all(head.as_bytes()).ok();
                    stream.write_all(&reply.body).ok();
                });
            }
        });
        Self { base, seen }
    }

    /// 答 `file` 这份样例，图片视频照常给。
    fn answering(file: &'static str) -> Self {
        Self::start(move |seen, base| media_or(seen, || json_reply(fixture(file, base))))
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    fn detail_requests(&self) -> Vec<Seen> {
        self.seen()
            .into_iter()
            .filter(|s| s.path == "/xhs/detail")
            .collect()
    }

    fn media_requests(&self) -> Vec<Seen> {
        self.seen()
            .into_iter()
            .filter(|s| s.path.starts_with("/img/") || s.path.starts_with("/media/"))
            .collect()
    }

    fn service(&self, token: Option<&str>) -> Service {
        Service::new(&self.base, token).unwrap()
    }

    fn site(&self) -> Site {
        site(&self.base)
    }

    fn link(&self) -> String {
        format!(
            "{}/explore/{ID}?xsec_token=tok&xsec_source=pc_collect",
            self.base
        )
    }
}

fn media_or(seen: &Seen, otherwise: impl FnOnce() -> Reply) -> Reply {
    if seen.path.starts_with("/img/") {
        Reply {
            content_type: "image/jpeg",
            ..json_reply(JPEG)
        }
    } else if seen.path.starts_with("/media/") {
        Reply {
            content_type: "video/mp4",
            ..json_reply(MP4)
        }
    } else {
        otherwise()
    }
}

fn site(base: &str) -> Site {
    // 短链主机指向一个没人监听的端口：插件不该去请求它
    Site::loopback(base, "http://127.0.0.1:1", local_policy()).unwrap()
}

fn local_policy() -> Policy {
    Policy {
        lookup: |_, _| Err(std::io::Error::other("no DNS in tests")),
        public: |ip| ip == IpAddr::from([127, 0, 0, 1]),
        cookies_over_http: true,
        ..Policy::strict()
    }
}

fn run(
    fake: &FakeService,
    token: Option<&str>,
    input: &str,
    out: &Path,
    want: Option<&[OutputKind]>,
) -> Result<usuyo_plugin::protocol::FetchOutput, Error> {
    xhsdownloader::fetch(
        &fake.site(),
        &fake.service(token),
        Request {
            input,
            out,
            want: want.map(|w| w.iter().copied().collect()),
        },
    )
}

fn failure_of(err: &Error) -> Value {
    serde_json::to_value(err.to_failure(Lang::Zh)).unwrap()
}

#[test]
fn video_note_through_the_service() {
    let fake = FakeService::answering("video.json");
    let out = tempfile::tempdir().unwrap();
    let got = run(&fake, Some(TOKEN), &fake.link(), out.path(), None).unwrap();

    // 请求：一次 POST，令牌只在 Authorization 里，请求体固定不下载、不查记录，不带 cookie 和代理
    let requests = fake.detail_requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request.method, "POST");
    assert_eq!(
        request.headers.get("authorization").map(String::as_str),
        Some(format!("Bearer {TOKEN}").as_str())
    );
    assert!(!request.headers.contains_key("cookie"));
    assert!(!request.body.contains(TOKEN));
    let body: Value = serde_json::from_str(&request.body).unwrap();
    assert_eq!(
        body,
        json!({
            "url": format!("{}/explore/{ID}?xsec_token=tok&xsec_source=pc_collect", fake.base),
            "download": false,
            "check_record": false,
        })
    );

    // 身份和 note-page 的一样
    let value = serde_json::to_value(&got).unwrap();
    assert_eq!(value["identity"]["platform"], "xiaohongshu");
    assert_eq!(value["identity"]["contentId"], ID);
    assert_eq!(
        value["identity"]["url"],
        format!("https://www.xiaohongshu.com/explore/{ID}")
    );
    assert_eq!(value["kind"], "video");
    assert_eq!(value["title"], "三分钟磨豆机对比");
    assert_eq!(value["text"], "三台磨豆机，同一支豆子。");
    assert_eq!(value["author"]["name"], "咖啡日常");
    assert_eq!(value["author"]["platformId"], "5b56be0014de415b2db830a6");
    assert_eq!(
        value["author"]["url"],
        "https://www.xiaohongshu.com/user/profile/5b56be0014de415b2db830a6"
    );
    assert_eq!(value["publishedAt"], "2026-10-01T12:00:00Z");
    assert_eq!(value["platformTags"], json!(["磨豆机", "咖啡"]));
    assert_eq!(value["completeness"], "full");
    assert_eq!(value["missing"], json!([]));
    assert_eq!(value["evidence"]["access"], "public-web");
    assert!(value["evidence"].get("status").is_none());
    assert_eq!(value["images"], json!(["images/001.jpg"]));
    assert_eq!(value["media"][0]["path"], "media/video.mp4");
    assert_eq!(value["media"][0]["kind"], "video");

    assert_eq!(
        std::fs::read(out.path().join("media/video.mp4")).unwrap(),
        MP4
    );
    assert_eq!(
        std::fs::read(out.path().join("images/001.jpg")).unwrap(),
        JPEG
    );
    let meta: Value =
        serde_json::from_slice(&std::fs::read(out.path().join("meta.json")).unwrap()).unwrap();
    assert_eq!(meta["noteId"], ID);
    assert_eq!(meta["counts"]["liked"], "980");
    // 服务的原样回答存在 note.json，令牌不在任何输出文件里
    let raw = std::fs::read_to_string(out.path().join("note.json")).unwrap();
    assert!(raw.contains("作品ID"));
    for name in ["note.json", "meta.json"] {
        let text = std::fs::read_to_string(out.path().join(name)).unwrap();
        assert!(!text.contains(TOKEN), "{name}");
    }
    assert!(!serde_json::to_string(&got).unwrap().contains(TOKEN));
}

#[test]
fn image_note_with_a_live_part_and_absent_counts() {
    let fake = FakeService::answering("images.json");
    let out = tempfile::tempdir().unwrap();
    let got = run(&fake, Some(TOKEN), &fake.link(), out.path(), None).unwrap();
    let value = serde_json::to_value(&got).unwrap();
    assert_eq!(value["kind"], "post");
    assert!(value.get("title").is_none() || value["title"].is_null());
    assert_eq!(value["text"], "今天的三张照片");
    assert_eq!(
        value["images"],
        json!(["images/001.jpg", "images/002.jpg", "images/003.jpg"])
    );
    assert_eq!(value["media"].as_array().unwrap().len(), 1);
    assert_eq!(value["media"][0]["path"], "media/live-002.mp4");
    // 时间戳带小数秒：按毫秒取整
    assert_eq!(value["publishedAt"], "2026-10-01T12:00:00.5Z");
    assert_eq!(value["completeness"], "full");
    let meta: Value =
        serde_json::from_slice(&std::fs::read(out.path().join("meta.json")).unwrap()).unwrap();
    // 服务把没有的互动数记成 "-1"，这里按没有算
    assert!(meta["counts"]["collected"].is_null());
    assert_eq!(meta["counts"]["liked"], "1.2万");
}

#[test]
fn only_what_was_wanted_is_downloaded() {
    let fake = FakeService::answering("video.json");
    let out = tempfile::tempdir().unwrap();
    let got = run(
        &fake,
        Some(TOKEN),
        &fake.link(),
        out.path(),
        Some(&[OutputKind::Text, OutputKind::Meta]),
    )
    .unwrap();
    assert!(fake.media_requests().is_empty());
    let value = serde_json::to_value(&got).unwrap();
    assert_eq!(value["text"], "三台磨豆机，同一支豆子。");
    assert_eq!(value["images"], json!([]));
    assert_eq!(value["completeness"], "full");
}

#[test]
fn a_picture_that_cannot_be_downloaded_is_reported_missing() {
    let fake = FakeService::start(|seen, base| {
        if seen.path == "/img/2.jpg" {
            return status_reply(404, "");
        }
        media_or(seen, || json_reply(fixture("images.json", base)))
    });
    let out = tempfile::tempdir().unwrap();
    let got = run(&fake, Some(TOKEN), &fake.link(), out.path(), None).unwrap();
    let value = serde_json::to_value(&got).unwrap();
    assert_eq!(value["completeness"], "partial");
    assert_eq!(value["missing"], json!(["images"]));
    assert!(value["missingReason"].as_str().unwrap().contains("1 张"));
    assert_eq!(value["images"].as_array().unwrap().len(), 2);
}

#[test]
fn a_short_link_goes_to_the_service_unchanged() {
    let fake = FakeService::answering("video.json");
    let out = tempfile::tempdir().unwrap();
    // 短链主机在测试里是 127.0.0.1:1，没人监听：插件自己去请求就会失败
    let got = run(
        &fake,
        Some(TOKEN),
        "http://127.0.0.1:1/abc?x=1",
        out.path(),
        None,
    )
    .unwrap();
    let body: Value = serde_json::from_str(&fake.detail_requests()[0].body).unwrap();
    assert_eq!(body["url"], "http://127.0.0.1:1/abc?x=1");
    // 身份取自服务回答的作品 ID
    assert_eq!(
        serde_json::to_value(&got).unwrap()["identity"]["contentId"],
        ID
    );
}

#[test]
fn locale_of_the_service_does_not_matter() {
    // 服务界面语言是 en_US 时，作品类型的值是 video、image、album
    let fake = FakeService::start(|seen, base| {
        media_or(seen, || {
            json_reply(fixture("video.json", base).replace("\"视频\"", "\"video\""))
        })
    });
    let out = tempfile::tempdir().unwrap();
    let got = run(&fake, Some(TOKEN), &fake.link(), out.path(), None).unwrap();
    assert_eq!(serde_json::to_value(&got).unwrap()["kind"], "video");
}

#[test]
fn no_token_means_no_request() {
    let fake = FakeService::answering("video.json");
    let out = tempfile::tempdir().unwrap();
    let err = run(&fake, None, &fake.link(), out.path(), None).unwrap_err();
    let failure = failure_of(&err);
    assert_eq!(failure["code"], "dependency-missing");
    assert_eq!(failure["needs"], json!(["setting:xhs-token"]));
    assert!(fake.seen().is_empty());
}

#[test]
fn a_link_without_xsec_token_is_not_sent() {
    let fake = FakeService::answering("video.json");
    let out = tempfile::tempdir().unwrap();
    let link = format!("{}/explore/{ID}", fake.base);
    let err = run(&fake, Some(TOKEN), &link, out.path(), None).unwrap_err();
    assert!(matches!(err, Error::LinkExpired(_)));
    let err = run(
        &fake,
        Some(TOKEN),
        "https://example.com/a",
        out.path(),
        None,
    )
    .unwrap_err();
    assert!(matches!(err, Error::Unsupported(_)));
    assert!(fake.seen().is_empty());
}

#[test]
fn the_service_is_not_running() {
    let port = {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let service = Service::new(&format!("http://127.0.0.1:{port}"), Some(TOKEN)).unwrap();
    let fake = FakeService::answering("video.json");
    let out = tempfile::tempdir().unwrap();
    let err = xhsdownloader::fetch(
        &fake.site(),
        &service,
        Request {
            input: &fake.link(),
            out: out.path(),
            want: None,
        },
    )
    .unwrap_err();
    let failure = failure_of(&err);
    assert_eq!(failure["code"], "dependency-missing");
    assert_eq!(failure["needs"], json!(["service:endpoint"]));
    let hint = failure["hint"].as_str().unwrap();
    assert!(
        hint.contains("没有在运行") && hint.contains("uv run main.py api"),
        "{hint}"
    );
    assert!(!failure.to_string().contains(TOKEN));
}

#[test]
fn the_service_refuses_the_token() {
    let fake = FakeService::start(|_, _| status_reply(403, r#"{"detail":"Forbidden"}"#));
    let out = tempfile::tempdir().unwrap();
    let err = run(&fake, Some(TOKEN), &fake.link(), out.path(), None).unwrap_err();
    let failure = failure_of(&err);
    assert_eq!(failure["code"], "dependency-missing");
    assert_eq!(failure["needs"], json!(["setting:xhs-token"]));
    assert!(!failure.to_string().contains(TOKEN));
    assert!(!format!("{err:?}").contains(TOKEN));
}

#[test]
fn error_answers_of_the_service() {
    let out = tempfile::tempdir().unwrap();
    let case = |file: &'static str| {
        let fake = FakeService::answering(file);
        let err = run(&fake, Some(TOKEN), &fake.link(), out.path(), None).unwrap_err();
        (failure_of(&err), fake)
    };
    // 服务没取到（data 为 null）：取不到，带着服务的原话进失败日志
    let (failure, _) = case("no-data.json");
    assert_eq!(failure["code"], "not-found");
    assert!(
        failure["detail"]
            .as_str()
            .unwrap()
            .contains("获取小红书作品数据失败")
    );
    // 它按下载记录跳过了：请求里写了 check_record: false，不该发生
    let (failure, _) = case("skipped.json");
    assert_eq!(failure["code"], "upstream-changed");
    // 认不出的作品类型
    let (failure, _) = case("unknown-type.json");
    assert_eq!(failure["code"], "upstream-changed");
    assert!(failure["message"].as_str().unwrap().contains("note type"));

    // 其余的回答形状
    for (status, body, code) in [
        (200, "not json at all", "upstream-changed"),
        (200, r#"{"data":{}}"#, "upstream-changed"),
        (200, r#"{"message":"x","data":[1]}"#, "upstream-changed"),
        (
            200,
            r#"{"message":"x","data":{"作品ID":"short","作品类型":"视频"}}"#,
            "upstream-changed",
        ),
        (
            200,
            r#"{"message":"x","data":{"作品类型":"视频"}}"#,
            "upstream-changed",
        ),
        (
            422,
            r#"{"detail":[{"msg":"field required"}]}"#,
            "upstream-changed",
        ),
        (500, "Internal Server Error", "upstream-changed"),
        (404, "Not Found", "upstream-changed"),
    ] {
        let fake = FakeService::start(move |_, _| status_reply(status, body));
        let err = run(&fake, Some(TOKEN), &fake.link(), out.path(), None).unwrap_err();
        let failure = failure_of(&err);
        assert_eq!(failure["code"], code, "{status} {body}");
        // 回答的开头进 detail，给修插件的人看
        assert!(failure["detail"].is_string(), "{status} {body}: {failure}");
    }
}

#[test]
fn a_note_other_than_the_link_is_refused() {
    let fake = FakeService::start(|seen, base| {
        media_or(seen, || {
            json_reply(fixture("video.json", base).replace(ID, "aaaaaaaaaaaaaaaaaaaaaaaa"))
        })
    });
    let out = tempfile::tempdir().unwrap();
    let err = run(&fake, Some(TOKEN), &fake.link(), out.path(), None).unwrap_err();
    assert_eq!(failure_of(&err)["code"], "upstream-changed");
    assert!(fake.media_requests().is_empty());
}

#[test]
fn a_service_that_never_answers_times_out() {
    let fake = FakeService::start(|_, _| Reply {
        delay: Duration::from_secs(3),
        ..json_reply("{}")
    });
    let out = tempfile::tempdir().unwrap();
    let service = fake
        .service(Some(TOKEN))
        .with_timeout(Duration::from_millis(400));
    let started = std::time::Instant::now();
    let err = xhsdownloader::fetch(
        &fake.site(),
        &service,
        Request {
            input: &fake.link(),
            out: out.path(),
            want: None,
        },
    )
    .unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(failure_of(&err)["code"], "network");
}

#[test]
fn a_huge_answer_is_not_read_to_the_end() {
    let fake = FakeService::start(|_, _| {
        let mut body = r#"{"message":"x","data":{"作品描述":""#.as_bytes().to_vec();
        body.resize(9 << 20, b'a');
        json_reply(body)
    });
    let out = tempfile::tempdir().unwrap();
    let err = run(&fake, Some(TOKEN), &fake.link(), out.path(), None).unwrap_err();
    assert_eq!(failure_of(&err)["code"], "network");
}

#[test]
fn only_local_addresses_are_accepted() {
    for bad in [
        "http://192.168.1.5:5556",
        "http://example.com:5556",
        "https://127.0.0.1:5556",
        "http://user:pw@127.0.0.1:5556",
        "http://127.0.0.1:5556/api",
        "http://127.0.0.1:5556/?a=1",
        "not a url",
        "http://0.0.0.0:5556",
    ] {
        let err = Service::new(bad, Some(TOKEN)).unwrap_err();
        let failure = failure_of(&err);
        assert_eq!(failure["needs"], json!(["setting:endpoint"]), "{bad}");
        assert!(!failure.to_string().contains("pw@"), "{bad}");
    }
    for good in [
        "http://127.0.0.1:5556",
        "http://127.0.0.1:5556/",
        "http://localhost:5556",
        "http://[::1]:5556",
    ] {
        assert!(Service::new(good, Some(TOKEN)).is_ok(), "{good}");
    }
    // 令牌里有空白或控制字符：报错，不写出它
    let err = Service::new("http://127.0.0.1:5556", Some("a b\r\nX-Evil: 1")).unwrap_err();
    assert!(!format!("{err:?}{}", failure_of(&err)).contains("X-Evil"));
    // 空的令牌等于没填
    let service = Service::new("http://127.0.0.1:5556", Some("  ")).unwrap();
    assert!(!format!("{service:?}").contains("redacted"));
    let service = Service::new("http://127.0.0.1:5556", Some(TOKEN)).unwrap();
    assert!(!format!("{service:?}").contains(TOKEN));
}

// ---- 起插件进程：设置和密钥经宿主给的文件传入

fn plugin(args: &[&str], settings: Option<&Value>, secrets: Option<&Value>, dir: &Path) -> Output {
    let mut cmd = Command::new(BIN);
    cmd.env_clear()
        .env("USUYO_PROTOCOL", "1")
        .env("USUYO_LOCALE", "zh-CN")
        .env("HTTP_PROXY", DEAD_PROXY)
        .env("HTTPS_PROXY", DEAD_PROXY)
        .env("ALL_PROXY", DEAD_PROXY)
        .args(args);
    if let Some(s) = settings {
        let path = dir.join("settings.json");
        std::fs::write(&path, s.to_string()).unwrap();
        cmd.env("USUYO_SETTINGS", path);
    }
    if let Some(s) = secrets {
        let path = dir.join("secrets.json");
        std::fs::write(&path, s.to_string()).unwrap();
        cmd.env("USUYO_SECRETS", path);
    }
    cmd.output().unwrap()
}

fn envelope(out: &Output) -> Value {
    let stdout = String::from_utf8(out.stdout.clone()).unwrap();
    assert_eq!(
        stdout.lines().count(),
        1,
        "stdout is one JSON value: {stdout}"
    );
    serde_json::from_str(&stdout).unwrap()
}

fn production_link() -> String {
    format!("https://www.xiaohongshu.com/explore/{ID}?xsec_token=tok")
}

#[test]
fn the_process_reads_settings_and_secrets_and_gives_text_and_meta() {
    let fake = FakeService::answering("video.json");
    let dir = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let output = plugin(
        &[
            "fetch",
            &production_link(),
            "--out",
            out.path().to_str().unwrap(),
            "--method",
            "xhs-downloader",
            "--want",
            "text,meta",
        ],
        Some(&json!({"endpoint": fake.base})),
        Some(&json!({"xhs-token": TOKEN})),
        dir.path(),
    );
    assert!(output.status.success(), "{output:?}");
    let value = envelope(&output);
    assert_eq!(value["ok"], true);
    assert_eq!(value["identity"]["contentId"], ID);
    assert_eq!(value["text"], "三台磨豆机，同一支豆子。");
    let requests = fake.detail_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].headers.get("authorization").map(String::as_str),
        Some(format!("Bearer {TOKEN}").as_str())
    );
    // 令牌不在 stdout、stderr 里
    assert!(!String::from_utf8_lossy(&output.stdout).contains(TOKEN));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(TOKEN));
}

#[test]
fn the_process_without_a_token_or_service_answers_dependency_missing() {
    let dir = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let args = [
        "fetch",
        &production_link(),
        "--out",
        out.path().to_str().unwrap(),
        "--method",
        "xhs-downloader",
    ];
    // 没有任何设置：没有令牌，连都不连
    let value = envelope(&plugin(&args, None, None, dir.path()));
    assert_eq!(value["error"]["code"], "dependency-missing");
    assert_eq!(value["error"]["needs"], json!(["setting:xhs-token"]));

    let closed = {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", listener.local_addr().unwrap())
    };
    let output = plugin(
        &args,
        Some(&json!({"endpoint": closed})),
        Some(&json!({"xhs-token": TOKEN})),
        dir.path(),
    );
    assert_eq!(output.status.code(), Some(1));
    let value = envelope(&output);
    assert_eq!(value["error"]["code"], "dependency-missing");
    assert_eq!(value["error"]["needs"], json!(["service:endpoint"]));
    assert!(
        value["error"]["hint"]
            .as_str()
            .unwrap()
            .contains("没有在运行")
    );

    // 设置写坏了：不是本机地址
    let value = envelope(&plugin(
        &args,
        Some(&json!({"endpoint": "http://example.com:5556"})),
        Some(&json!({"xhs-token": TOKEN})),
        dir.path(),
    ));
    assert_eq!(value["error"]["needs"], json!(["setting:endpoint"]));
    // 设置文件坏了
    std::fs::write(dir.path().join("secrets.json"), "{oops").unwrap();
    let mut cmd = Command::new(BIN);
    cmd.env_clear()
        .env("USUYO_SECRETS", dir.path().join("secrets.json"))
        .args(args);
    let output = cmd.output().unwrap();
    let value = envelope(&output);
    assert_eq!(value["error"]["code"], "internal");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("oops"));
}

#[test]
fn health_names_the_method_it_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let doc = |title: &str| {
        json!({"openapi": "3.1.0", "info": {"title": title, "version": "2.8"}}).to_string()
    };

    // 服务在运行、令牌填了
    let fake = FakeService::start({
        let ok = doc("XHS-Downloader");
        move |seen, _| {
            if seen.path == "/openapi.json" {
                json_reply(ok.clone())
            } else {
                status_reply(404, "")
            }
        }
    });
    let value = envelope(&plugin(
        &["health"],
        Some(&json!({"endpoint": fake.base})),
        Some(&json!({"xhs-token": TOKEN})),
        dir.path(),
    ));
    assert_eq!(value["ok"], true);
    assert_eq!(value["ready"], true);
    assert_eq!(
        value["upstream"],
        json!([{"name": "XHS-Downloader", "version": "2.8"}])
    );
    assert!(
        value["checks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["ok"] == true)
    );
    assert!(!value.to_string().contains(TOKEN));

    // 服务没在运行：只挡 xhs-downloader，一句中文说怎么启动
    let closed = {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", listener.local_addr().unwrap())
    };
    let value = envelope(&plugin(
        &["health"],
        Some(&json!({"endpoint": closed})),
        Some(&json!({"xhs-token": TOKEN})),
        dir.path(),
    ));
    assert_eq!(
        value["ready"], true,
        "a blocked method does not block the plugin"
    );
    let check = value["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["ok"] == false)
        .unwrap();
    assert_eq!(check["methods"], json!(["xhs-downloader"]));
    let message = check["message"].as_str().unwrap();
    assert!(
        message.contains("没有在运行") && message.contains("uv run main.py api"),
        "{message}"
    );

    // 没填令牌
    let value = envelope(&plugin(
        &["health"],
        Some(&json!({"endpoint": fake.base})),
        None,
        dir.path(),
    ));
    let bad: Vec<&Value> = value["checks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["ok"] == false)
        .collect();
    assert_eq!(bad.len(), 1);
    assert!(bad[0]["message"].as_str().unwrap().contains("没填访问令牌"));
    assert_eq!(bad[0]["methods"], json!(["xhs-downloader"]));

    // 地址上是别的程序
    let other = FakeService::start(move |_, _| json_reply(doc("Something else")));
    let value = envelope(&plugin(
        &["health"],
        Some(&json!({"endpoint": other.base})),
        Some(&json!({"xhs-token": TOKEN})),
        dir.path(),
    ));
    let bad = value["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["ok"] == false)
        .unwrap();
    assert!(
        bad["message"]
            .as_str()
            .unwrap()
            .contains("不像 XHS-Downloader")
    );

    // 设置里不是本机地址
    let value = envelope(&plugin(
        &["health"],
        Some(&json!({"endpoint": "http://example.com:5556"})),
        None,
        dir.path(),
    ));
    assert_eq!(value["checks"][0]["ok"], false);
    assert_eq!(value["checks"][0]["methods"], json!(["xhs-downloader"]));
}

#[test]
fn manifest_declares_the_method_the_way_the_code_calls_it() {
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
    let m = parsed.value;
    let PluginKind::Fetch(spec) = &m.kind else {
        panic!("not a fetch plugin");
    };
    let method = spec.method(xhsdownloader::METHOD).unwrap();
    assert!(method.built && method.sites.is_empty() && method.visitors.is_empty());
    assert_eq!(
        (method.access, method.login, method.account),
        (Access::PublicWeb, Login::None, Account::None)
    );
    assert!(method.on_by_default());
    assert!(method.risk.is_some());
    let fetch = method.capabilities.fetch.as_ref().unwrap();
    assert!(
        fetch.outputs.contains(&OutputKind::Video) && fetch.outputs.contains(&OutputKind::Text)
    );
    // 设置的名字和代码读的一致，令牌是密钥设置
    let setting = |key: &str| m.header.settings.iter().find(|s| s.key == key).unwrap();
    assert_eq!(
        setting("endpoint").default,
        Some(Value::String(xhsdownloader::DEFAULT_ENDPOINT.into()))
    );
    assert!(setting("xhs-token").secret && !setting("endpoint").secret);
    // 能连的主机：设置里的服务地址，和默认图片格式给的图片主机
    let hosts = &m.header.permissions.network.hosts;
    for host in ["${setting.endpoint}", "ci.xiaohongshu.com", "*.xhscdn.com"] {
        assert!(hosts.iter().any(|h| h == host), "{host}");
    }
}
