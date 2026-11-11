//! 本机的假小红书：一个笔记页服务、一个短链服务，按 `xsec_token` 的值演出各种情况，
//! 记下每个请求（路径、带没带 cookie），测试据此核对请求次数。不碰真网络。
//! 只有 `tests/fetch.rs` 用它。

use std::io::{BufRead, BufReader, Write};
use std::net::{IpAddr, TcpListener};
use std::sync::{Arc, Mutex};

use usuyo_fetch_xiaohongshu::Site;
use usuyo_plugin_kit::http::Policy;

pub const ID: &str = "671234567890abcdef012345";
pub const COOKIE: &str = "a1=visitor-a1; webId=visitor-web; web_session=visitor-session-secret";
pub const WEBP: &[u8] = b"RIFF\x10\x00\x00\x00WEBPVP8 fake";
pub const JPEG: &[u8] = b"\xff\xd8\xff\xe0fake-jpeg";
pub const MP4: &[u8] = b"\x00\x00\x00\x18ftypmp42fake-video-bytes";

#[derive(Debug, Clone)]
pub struct Seen {
    pub server: &'static str,
    pub target: String,
    pub cookie: Option<String>,
    pub referer: Option<String>,
}

/// 测试用的地址策略：只有 127.0.0.1 算公网，不查 DNS。
pub fn local_policy() -> Policy {
    Policy {
        lookup: |_, _| {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no DNS in tests",
            ))
        },
        public: |ip| ip == IpAddr::from([127, 0, 0, 1]),
        cookies_over_http: true,
    }
}

pub struct Fake {
    pub web: String,
    pub short: String,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Fake {
    pub fn start() -> Self {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let web_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let short_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let web = format!("http://{}", web_listener.local_addr().unwrap());
        let short = format!("http://{}", short_listener.local_addr().unwrap());
        spawn(web_listener, "web", web.clone(), seen.clone(), web_route);
        spawn(
            short_listener,
            "short",
            web.clone(),
            seen.clone(),
            short_route,
        );
        Self { web, short, seen }
    }

    pub fn site(&self) -> Site {
        Site::loopback(&self.web, &self.short, local_policy()).unwrap()
    }

    pub fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    /// 对笔记页（不含图片视频）的请求。
    pub fn page_requests(&self) -> Vec<Seen> {
        self.seen()
            .into_iter()
            .filter(|s| {
                s.server == "web"
                    && !s.target.starts_with("/img/")
                    && !s.target.starts_with("/media/")
            })
            .collect()
    }

    pub fn note_url(&self, token: &str) -> String {
        format!(
            "{}/explore/{ID}?xsec_token={token}&xsec_source=pc_collect",
            self.web
        )
    }
}

pub struct Reply {
    status: &'static str,
    content_type: &'static str,
    body: Vec<u8>,
    headers: Vec<String>,
}

fn reply(status: &'static str, content_type: &'static str, body: impl Into<Vec<u8>>) -> Reply {
    Reply {
        status,
        content_type,
        body: body.into(),
        headers: Vec::new(),
    }
}

fn redirect(status: &'static str, to: String) -> Reply {
    Reply {
        headers: vec![format!("Location: {to}")],
        ..reply(status, "text/html", Vec::new())
    }
}

fn fixture(name: &str, web: &str) -> Vec<u8> {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(path)
        .unwrap()
        .replace("{BASE}", web)
        .replace("{ID}", ID)
        .into_bytes()
}

fn token(target: &str) -> String {
    let query = target.split_once('?').map(|(_, q)| q).unwrap_or_default();
    query
        .split('&')
        .find_map(|p| p.strip_prefix("xsec_token="))
        .unwrap_or_default()
        .to_owned()
}

fn web_route(target: &str, cookie: Option<&str>, web: &str) -> Reply {
    let with_cookie = cookie.is_some_and(|c| c.contains("web_session=visitor-session-secret"));
    let path = target.split('?').next().unwrap_or_default();
    match path {
        "/img/a.webp" => return reply("200 OK", "image/webp", WEBP),
        "/img/b.jpg" | "/img/cover.jpg" => return reply("200 OK", "image/jpeg", JPEG),
        "/media/v.mp4" | "/media/low.mp4" => return reply("200 OK", "video/mp4", MP4),
        "/media/broken.mp4" => return reply("500 Internal Server Error", "text/plain", "boom"),
        _ => {}
    }
    let note_paths = [format!("/explore/{ID}"), format!("/discovery/item/{ID}")];
    if !note_paths.iter().any(|p| p == path) {
        return reply("404 Not Found", "text/html", "<html>404</html>");
    }
    let page = |name: &str| reply("200 OK", "text/html; charset=utf-8", fixture(name, web));
    match token(target).as_str() {
        "ok" => page("image-note.html"),
        "video" => page("video-note.html"),
        "live" => page("live-note.html"),
        "nostream" => page("video-nostream.html"),
        "hop" => redirect("302 Found", format!("/explore/{ID}?xsec_token=ok")),
        "needcookie" if with_cookie => page("image-note.html"),
        "needcookie" => redirect("302 Found", "/".into()),
        "absent" if with_cookie => page("image-note.html"),
        "absent" => page("absent-note.html"),
        "home" => redirect("302 Found", format!("{web}/explore")),
        "login" => redirect(
            "302 Found",
            "/login?redirectPath=%2Fexplore&exSource=".into(),
        ),
        "expired" => redirect(
            "302 Found",
            "/404/sec_ab12cd?redirectPath=%2Fexplore&error_code=300031&error_msg=%E5%BD%93%E5%89%8D%E7%AC%94%E8%AE%B0%E6%9A%82%E6%97%B6%E6%97%A0%E6%B3%95%E6%B5%8F%E8%A7%88".into(),
        ),
        "sec" => redirect("302 Found", "/404/sec_ab12cd".into()),
        "captcha" => redirect(
            "302 Found",
            "/website-login/captcha?redirectPath=%2Fexplore&verifyType=101".into(),
        ),
        "gone" => redirect("302 Found", "/404?source=note".into()),
        "risk" => reply(
            "200 OK",
            "application/json",
            r#"{"code":300013,"success":false,"msg":"访问频次异常，请勿频繁操作或重启试试"}"#,
        ),
        "verify" => page("verify.html"),
        "changed" => page("changed.html"),
        "noscript" => reply("200 OK", "text/html", "<html><body>空壳</body></html>"),
        "limited" => Reply {
            headers: vec!["Retry-After: 600".into()],
            ..reply("429 Too Many Requests", "text/html", "")
        },
        "gateway" => reply("461 Unknown", "application/json", r#"{"code":461}"#),
        _ => reply("404 Not Found", "text/html", "<html>404</html>"),
    }
}

fn short_route(target: &str, _cookie: Option<&str>, web: &str) -> Reply {
    match target {
        "/abc" => redirect(
            "307 Temporary Redirect",
            format!(
                "{web}/discovery/item/{ID}?app_platform=ios&xsec_token=ok&xsec_source=app_share&shareRedId=xyz"
            ),
        ),
        "/hop" => redirect("302 Found", "/abc".into()),
        "/profile" => redirect(
            "302 Found",
            format!("{web}/user/profile/5b56be0014de415b2db830a6?xsec_token=x"),
        ),
        "/captcha" => redirect("302 Found", format!("{web}/website-login/captcha")),
        "/loop" => redirect("302 Found", "/loop".into()),
        "/elsewhere" => redirect("302 Found", "https://example.org/".into()),
        _ => reply("404 Not Found", "text/html", "nope"),
    }
}

type Route = fn(&str, Option<&str>, &str) -> Reply;

fn spawn(
    listener: TcpListener,
    server: &'static str,
    web: String,
    seen: Arc<Mutex<Vec<Seen>>>,
    route: Route,
) {
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let seen = seen.clone();
            let web = web.clone();
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    return;
                }
                let target = line.split_whitespace().nth(1).unwrap_or("/").to_owned();
                let mut cookie = None;
                let mut referer = None;
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).unwrap_or(0) == 0 || header == "\r\n" {
                        break;
                    }
                    if let Some((name, value)) = header.split_once(':') {
                        match name.to_ascii_lowercase().as_str() {
                            "cookie" => cookie = Some(value.trim().to_owned()),
                            "referer" => referer = Some(value.trim().to_owned()),
                            _ => {}
                        }
                    }
                }
                seen.lock().unwrap().push(Seen {
                    server,
                    target: target.clone(),
                    cookie: cookie.clone(),
                    referer,
                });
                let r = route(&target, cookie.as_deref(), &web);
                let mut head = format!(
                    "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n",
                    r.status,
                    r.content_type,
                    r.body.len()
                );
                for h in &r.headers {
                    head.push_str(h);
                    head.push_str("\r\n");
                }
                head.push_str("\r\n");
                stream.write_all(head.as_bytes()).ok();
                stream.write_all(&r.body).ok();
            });
        }
    });
}
