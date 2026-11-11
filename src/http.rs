//! 发请求的唯一入口：主机先过 [`Site::may_request`]，重定向一律不自动跟，
//! cookie 只发给笔记页的主机，每个请求都有超时，整次调用有总时限。

use std::time::{Duration, Instant};

use ureq::Body;
use ureq::http::Response;
use url::Url;

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
    agent: ureq::Agent,
    deadline: Instant,
}

impl<'s> Http<'s> {
    /// `budget` 是整次调用留给网络的总时长，要比宿主的总超时短，留出收尾的时间。
    pub(crate) fn new(site: &'s Site, budget: Duration) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(10)))
            .max_redirects(0)
            .http_status_as_error(false)
            .user_agent(USER_AGENT)
            .build()
            .into();
        Self {
            site,
            agent,
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
    ) -> Result<Response<Body>, Error> {
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
        let mut request = self
            .agent
            .get(url.as_str())
            .header("Accept", accept)
            .header("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.6");
        match purpose {
            Purpose::Page { cookie: Some(c) } => request = request.header("Cookie", c.as_str()),
            // 浏览器从笔记页取图时只带源，不带路径和 xsec_token
            Purpose::Image | Purpose::Video => {
                request = request.header("Referer", self.site.web_origin().as_str());
            }
            Purpose::Page { cookie: None } | Purpose::Short => {}
        }
        request
            .config()
            .timeout_global(Some(timeout.min(left)))
            .build()
            .call()
            .map_err(|e| Error::Network(e.to_string()))
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
pub(crate) fn header<'r>(response: &'r Response<Body>, name: &str) -> Option<&'r str> {
    response.headers().get(name).and_then(|v| v.to_str().ok())
}

/// 重定向的去处，按请求的地址解析相对路径。
pub(crate) fn location(response: &Response<Body>, from: &Url) -> Result<Option<Url>, Error> {
    if !response.status().is_redirection() {
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
pub(crate) fn read_capped(response: &mut Response<Body>, limit: u64) -> Result<Vec<u8>, Error> {
    response
        .body_mut()
        .with_config()
        .limit(limit)
        .read_to_vec()
        .map_err(|e| match e {
            ureq::Error::BodyExceedsLimit(_) => {
                Error::UpstreamChanged(format!("response is larger than {limit} bytes"))
            }
            other => Error::Network(other.to_string()),
        })
}

/// `Retry-After` 只认秒数。
pub(crate) fn retry_after(response: &Response<Body>) -> Option<u64> {
    header(response, "retry-after").and_then(|v| v.trim().parse().ok())
}

#[cfg(test)]
mod tests {
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
}
