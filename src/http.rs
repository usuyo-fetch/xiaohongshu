//! 发请求的唯一入口：主机先过 [`Site::may_request`]，再经 `usuyo_plugin_kit::http`
//! 按用途只连那一类主机、只连公网地址。重定向一律不自动跟，
//! cookie 只发给笔记页的主机，每个请求都有超时，整次调用有总时限。

use std::time::{Duration, Instant};

use url::Url;
use usuyo_plugin_kit::http::{Client, Cookie, Hosts, Response};

use crate::error::Error;
use crate::site::Site;

/// 普通桌面 Chrome 的 UA 和 Accept（ParseHub 记过：UA 和 Accept 对不上会被送去验证页）。
const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/142.0.0.0 Safari/537.36";
const ACCEPT_HTML: &str = "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.7";
const ACCEPT_IMAGE: &str = "image/avif,image/webp,image/apng,image/*,*/*;q=0.8";
const ACCEPT_MEDIA: &str = "*/*";

/// 访客 cookie：浏览器请求头里 `Cookie:` 后面的那一行。不打印、不写盘。
#[derive(Clone)]
pub struct VisitorCookie(String);

impl VisitorCookie {
    pub fn parse(raw: &str) -> Result<Self, Error> {
        let value = raw.trim();
        let value = value
            .strip_prefix("Cookie:")
            .or_else(|| value.strip_prefix("cookie:"))
            .unwrap_or(value)
            .trim();
        if value.is_empty() || value.len() > 8192 {
            return Err(Error::BadCookie("empty or longer than 8 KiB".into()));
        }
        if !value.bytes().all(|b| (0x20..0x7f).contains(&b)) {
            return Err(Error::BadCookie(
                "only printable ASCII on one line is allowed".into(),
            ));
        }
        let pairs_ok = value
            .split(';')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .all(|p| p.split_once('=').is_some_and(|(k, _)| !k.trim().is_empty()));
        if !pairs_ok {
            return Err(Error::BadCookie("expected `name=value; name=value`".into()));
        }
        Ok(Self(value.to_owned()))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for VisitorCookie {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("VisitorCookie(<redacted>)")
    }
}

pub(crate) struct Http<'s> {
    site: &'s Site,
    /// 笔记页、短链、图片视频各一个客户端，各自只连那一类主机。
    web: Client,
    short: Client,
    media: Client,
    deadline: Instant,
}

impl<'s> Http<'s> {
    /// `budget` 是整次调用留给网络的总时长，要比宿主的总超时短，留出收尾的时间。
    pub(crate) fn new(site: &'s Site, budget: Duration) -> Self {
        let client = |hosts: Vec<String>| {
            Client::new(USER_AGENT, Hosts::only(hosts)).with_policy(site.policy())
        };
        Self {
            site,
            web: client(site.web_hosts()),
            short: client(site.short_hosts()),
            media: client(site.media_hosts()),
            deadline: Instant::now() + budget,
        }
    }

    pub(crate) fn site(&self) -> &Site {
        self.site
    }

    pub(crate) fn time_left(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }

    /// 一次 GET，不跟重定向。`timeout` 是这个请求从发出到读完的上限，再受总时限约束。
    /// 每种用途只许发往对应的主机：cookie 只跟笔记页走，Referer 只跟图片视频走。
    pub(crate) fn get(
        &self,
        url: &Url,
        purpose: Purpose<'_>,
        timeout: Duration,
    ) -> Result<Response, Error> {
        let allowed = match purpose {
            Purpose::Page { .. } => self.site.is_web(url),
            Purpose::Short => self.site.short_origin(url).is_some(),
            Purpose::Image | Purpose::Video => self.site.is_media(url),
        };
        if !allowed || !self.site.may_request(url) {
            return Err(Error::Internal(format!(
                "refusing to contact `{}` for {purpose:?}: not on the plugin's host list",
                url.host_str().unwrap_or_default()
            )));
        }
        let left = self.time_left();
        if left < Duration::from_secs(1) {
            return Err(Error::Network("the call's time budget is used up".into()));
        }
        let accept = match purpose {
            Purpose::Page { .. } | Purpose::Short => ACCEPT_HTML,
            Purpose::Image => ACCEPT_IMAGE,
            Purpose::Video => ACCEPT_MEDIA,
        };
        let client = match purpose {
            Purpose::Page { .. } => &self.web,
            Purpose::Short => &self.short,
            Purpose::Image | Purpose::Video => &self.media,
        };
        let mut request = client
            .get(url)
            .header("Accept", accept)
            .header("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.6")
            .timeout(timeout.min(left));
        match purpose {
            // 访客 cookie 只给笔记页的主机
            Purpose::Page { cookie: Some(c) } => {
                request = request.cookie(&Cookie::new(c.as_str(), self.site.web_hosts()));
            }
            // 浏览器从笔记页取图时只带源，不带路径和 xsec_token
            Purpose::Image | Purpose::Video => {
                let referer = self.site.web_origin().as_str().to_owned();
                request = request.header("Referer", &referer);
            }
            Purpose::Page { cookie: None } | Purpose::Short => {}
        }
        request.send().map_err(|e| Error::Network(e.to_string()))
    }
}

/// 一个请求是做什么的。
#[derive(Debug, Clone, Copy)]
pub(crate) enum Purpose<'c> {
    /// 笔记页；只有这里能带访客 cookie。
    Page {
        cookie: Option<&'c VisitorCookie>,
    },
    /// 展开短链，不带任何 cookie。
    Short,
    Image,
    Video,
}

/// 响应头里的值。
pub(crate) fn header<'r>(response: &'r Response, name: &str) -> Option<&'r str> {
    response.header(name)
}

/// 重定向的去处，按请求的地址解析相对路径。
pub(crate) fn location(response: &Response, from: &Url) -> Result<Option<Url>, Error> {
    if !(300..400).contains(&response.status()) {
        return Ok(None);
    }
    let raw = header(response, "location").ok_or_else(|| {
        Error::UpstreamChanged(format!("HTTP {} without Location", response.status()))
    })?;
    from.join(raw)
        .map(Some)
        .map_err(|e| Error::UpstreamChanged(format!("bad Location `{raw}`: {e}")))
}

/// 读整个响应体，超过 `limit` 字节就算失败。
pub(crate) fn read_capped(response: &mut Response, limit: u64) -> Result<Vec<u8>, Error> {
    response.read_to_vec(limit).map_err(|e| match e {
        usuyo_plugin_kit::http::Error::TooLarge(_) => {
            Error::UpstreamChanged(format!("response is larger than {limit} bytes"))
        }
        other => Error::Network(other.to_string()),
    })
}

/// `Retry-After` 只认秒数。
pub(crate) fn retry_after(response: &Response) -> Option<u64> {
    header(response, "retry-after").and_then(|v| v.trim().parse().ok())
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::net::{IpAddr, TcpListener};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use usuyo_plugin_kit::http::Policy;

    use super::*;

    #[test]
    fn cookie_lines() {
        assert!(VisitorCookie::parse("a1=x; webId=y; web_session=z").is_ok());
        assert!(VisitorCookie::parse("Cookie: a1=x").is_ok());
        for bad in ["", "   ", "a1=x\r\nHost: evil", "novalue", "=x", "a1=中文"] {
            assert!(VisitorCookie::parse(bad).is_err(), "{bad:?}");
        }
        let c = VisitorCookie::parse("a1=secret").unwrap();
        assert!(!format!("{c:?}").contains("secret"));
    }

    /// 本机服务：`/to/<地址>` 回 302 到那个地址；记下连进来几次。
    fn serve() -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let hits = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&hits);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                count.fetch_add(1, Ordering::SeqCst);
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let path = line.split_whitespace().nth(1).unwrap_or("/").to_owned();
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).unwrap_or(0) == 0 || header == "\r\n" {
                        break;
                    }
                }
                let location = path.strip_prefix("/to/").unwrap_or_default();
                let head = format!(
                    "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                stream.write_all(head.as_bytes()).ok();
            }
        });
        (base, hits)
    }

    fn local_policy() -> Policy {
        Policy {
            lookup: |_, _| {
                Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "no DNS in tests",
                ))
            },
            public: |ip| ip == IpAddr::from([127, 0, 0, 1]),
            cookies_over_http: true,
            ..Policy::strict()
        }
    }

    /// 图片主机跳去名单外的主机、跳去回环地址：3xx 原样交回，跳转目标一次也不连。
    #[test]
    fn redirect_targets_off_the_list_are_never_contacted() {
        let (web, _) = serve();
        let (elsewhere, hits) = serve();
        let port = elsewhere.rsplit(':').next().unwrap();
        let site = Site::loopback(&web, &web, local_policy()).unwrap();
        let http = Http::new(&site, Duration::from_secs(10));
        let cookie = VisitorCookie::parse("a1=secret").unwrap();
        for target in [
            format!("http://localhost:{port}/x.jpg"),
            format!("http://127.0.0.2:{port}/x.jpg"),
        ] {
            let from = Url::parse(&format!("{web}/to/{target}")).unwrap();
            for purpose in [
                Purpose::Image,
                Purpose::Page {
                    cookie: Some(&cookie),
                },
            ] {
                let response = http.get(&from, purpose, Duration::from_secs(5)).unwrap();
                assert_eq!(response.status(), 302);
                let next = location(&response, &from).unwrap().unwrap();
                assert_eq!(next.as_str(), target);
                assert!(matches!(
                    http.get(&next, purpose, Duration::from_secs(5)),
                    Err(Error::Internal(_))
                ));
            }
        }
        assert_eq!(hits.load(Ordering::SeqCst), 0);

        // 正式的地址策略下，回环上的笔记页连之前就被拒
        let strict = Site::loopback(&web, &web, Policy::strict()).unwrap();
        let http = Http::new(&strict, Duration::from_secs(10));
        let page = Url::parse(&format!("{web}/explore/x")).unwrap();
        let err = http
            .get(
                &page,
                Purpose::Page { cookie: None },
                Duration::from_secs(5),
            )
            .err()
            .unwrap();
        assert!(err.to_string().contains("not a public address"), "{err}");
    }
}
