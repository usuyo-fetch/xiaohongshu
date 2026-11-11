//! 插件能连哪些主机。所有请求都先过 [`Site::may_request`]，名单之外的一律不发。
//!
//! 正式运行只用 [`Site::xiaohongshu`]；测试用 [`Site::loopback`] 把三类主机都指到本机，
//! 它只接受回环地址，所以测试不可能连到真的小红书。

use std::net::IpAddr;

use url::Url;
use usuyo_plugin_kit::http::Policy;

/// 笔记页、短链、图片视频三类主机。
#[derive(Debug, Clone)]
pub struct Site {
    /// 笔记页所在的源（scheme + host + port）。
    web: Url,
    /// 短链的源。
    short: Vec<Url>,
    /// 图片、视频的主机：`*.` 开头的按后缀匹配子域，其余按整个主机名匹配。
    media: Vec<String>,
    /// 页面给的 `http://` 图片视频地址改走 `https://`。
    media_https: bool,
    /// 地址策略：正式运行只连公网地址。
    policy: Policy,
}

/// 规范链接和作者链接总写真站点，测试里也一样：它们只用于去重和显示。
pub const CANONICAL_ORIGIN: &str = "https://www.xiaohongshu.com";

impl Site {
    /// 小红书本身。和清单 `permissions.network.hosts` 保持一致。
    pub fn xiaohongshu() -> Self {
        Self {
            web: Url::parse(CANONICAL_ORIGIN).expect("constant URL"),
            short: ["https://xhslink.com", "https://xhslink.cn"]
                .map(|u| Url::parse(u).expect("constant URL"))
                .to_vec(),
            media: vec!["*.xhscdn.com".into()],
            media_https: true,
            policy: Policy::from_host(),
        }
    }

    /// 测试用：笔记页和短链各是一个本机地址，图片视频只能在回环地址上。
    /// `policy` 由测试给，让本机测试服务算公网。
    pub fn loopback(web: &str, short: &str, policy: Policy) -> Result<Self, String> {
        let parse = |text: &str| -> Result<Url, String> {
            let url = Url::parse(text).map_err(|e| format!("`{text}`: {e}"))?;
            match url.host_str().and_then(|h| h.parse::<IpAddr>().ok()) {
                Some(ip) if ip.is_loopback() => Ok(url),
                _ => Err(format!("`{text}` is not a loopback address")),
            }
        };
        Ok(Self {
            web: parse(web)?,
            short: vec![parse(short)?],
            media: vec!["127.0.0.1".into()],
            media_https: false,
            policy,
        })
    }

    pub(crate) fn policy(&self) -> Policy {
        self.policy
    }

    /// 发请求的客户端用的主机名单：`*.` 开头的写成 `.` 开头的后缀。
    pub(crate) fn web_hosts(&self) -> Vec<String> {
        self.web.host_str().map(str::to_owned).into_iter().collect()
    }

    pub(crate) fn short_hosts(&self) -> Vec<String> {
        self.short
            .iter()
            .filter_map(|u| u.host_str().map(str::to_owned))
            .collect()
    }

    pub(crate) fn media_hosts(&self) -> Vec<String> {
        self.media
            .iter()
            .map(|m| m.strip_prefix('*').unwrap_or(m).to_owned())
            .collect()
    }

    pub(crate) fn web_origin(&self) -> &Url {
        &self.web
    }

    pub(crate) fn is_web(&self, url: &Url) -> bool {
        same_host(&self.web, url)
    }

    /// 输入的短链对应的那个源。
    pub(crate) fn short_origin(&self, url: &Url) -> Option<&Url> {
        self.short.iter().find(|s| same_host(s, url))
    }

    pub(crate) fn is_media(&self, url: &Url) -> bool {
        let Some(host) = url.host_str() else {
            return false;
        };
        matches!(url.scheme(), "http" | "https")
            && self.media.iter().any(|m| match m.strip_prefix("*.") {
                Some(suffix) => host
                    .strip_suffix(suffix)
                    .is_some_and(|rest| rest.len() > 1 && rest.ends_with('.')),
                None => host == m,
            })
    }

    /// 页面给的图片视频地址：只认名单里的主机，正式运行时升到 https。
    pub(crate) fn media_url(&self, raw: &str) -> Option<Url> {
        let mut url = Url::parse(raw.trim()).ok()?;
        if self.media_https && url.scheme() == "http" {
            url.set_scheme("https").ok()?;
        }
        self.is_media(&url).then_some(url)
    }

    /// 唯一的出站闸门。
    pub(crate) fn may_request(&self, url: &Url) -> bool {
        matches!(url.scheme(), "http" | "https")
            && (self.is_web(url) || self.short_origin(url).is_some() || self.is_media(url))
    }
}

/// 同一个主机名，端口一致（默认端口记作没写）。scheme 不比：`http://` 的输入改走源的 scheme。
fn same_host(origin: &Url, url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.host_str() == origin.host_str()
        && url.port() == origin.port()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_hosts() {
        let site = Site::xiaohongshu();
        let u = |s: &str| Url::parse(s).unwrap();
        assert!(site.is_web(&u("https://www.xiaohongshu.com/explore/x")));
        assert!(site.is_web(&u("http://www.xiaohongshu.com/explore/x")));
        assert!(!site.is_web(&u("https://xiaohongshu.com/explore/x")));
        assert!(site.short_origin(&u("http://xhslink.com/a/b")).is_some());
        assert!(site.is_media(&u("https://sns-webpic-qc.xhscdn.com/a")));
        assert!(!site.is_media(&u("https://xhscdn.com/a")));
        assert!(!site.is_media(&u("https://evilxhscdn.com/a")));
        assert!(!site.may_request(&u("https://example.com/")));
        assert_eq!(
            site.media_url("http://sns-video-bd.xhscdn.com/v.mp4")
                .unwrap()
                .scheme(),
            "https"
        );
        assert!(site.media_url("https://example.com/a.jpg").is_none());
    }

    #[test]
    fn loopback_refuses_real_hosts() {
        let p = Policy::strict();
        assert!(Site::loopback("https://www.xiaohongshu.com", "http://127.0.0.1:2", p).is_err());
        assert!(Site::loopback("http://127.0.0.1:1", "https://xhslink.com", p).is_err());
        let site = Site::loopback("http://127.0.0.1:1", "http://127.0.0.1:2", p).unwrap();
        assert!(!site.may_request(&Url::parse("https://www.xiaohongshu.com/explore/x").unwrap()));
        assert!(!site.may_request(&Url::parse("https://sns-webpic-qc.xhscdn.com/a").unwrap()));
    }
}
