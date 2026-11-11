//! 认链接（协议 §3.5）：四种长链直接认，短链只跟重定向、不带 cookie，
//! 一看到长链就停，不去请求笔记页。

use std::time::Duration;

use url::Url;
use usuyo_plugin::protocol::{Canonicalized, Identity, PlatformId};

use crate::error::Error;
use crate::http::{self, Http, Purpose};
use crate::site::{CANONICAL_ORIGIN, Site};

pub const PLATFORM: &str = "xiaohongshu";
const SHORT_HOPS: usize = 5;
const SHORT_TIMEOUT: Duration = Duration::from_secs(8);

/// 小红书笔记 id：24 位十六进制，统一成小写。
pub fn note_id(text: &str) -> Option<String> {
    (text.len() == 24 && text.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| text.to_ascii_lowercase())
}

/// 长链的四种路径：`explore/<id>`、`search_result/<id>`、`discovery/item/<id>`、
/// `user/profile/<作者>/<id>`。
pub(crate) fn note_id_in_path(url: &Url) -> Option<String> {
    let segments: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).collect();
    let id = match segments.as_slice() {
        ["explore" | "search_result", id] => id,
        ["discovery", "item", id] => id,
        ["user", "profile", _, id] => id,
        _ => return None,
    };
    note_id(id)
}

pub fn canonical_url(id: &str) -> String {
    format!("{CANONICAL_ORIGIN}/explore/{id}")
}

pub fn identity(id: &str) -> Identity {
    Identity::Platform {
        platform: PlatformId::try_from(PLATFORM).expect("constant platform id"),
        content_id: id.into(),
        url: Some(canonical_url(id)),
    }
}

/// 笔记页上的一条长链，`url` 保留原来的全部参数（作 `resolvedUrl`）；请求时用 [`NoteLink::page_url`]。
#[derive(Debug, Clone)]
pub(crate) struct NoteLink {
    pub id: String,
    pub url: Url,
}

/// 请求笔记页时只带这两个参数（ParseHub 清洗链接后也只留它们）。手机分享的链接还带着
/// `shareRedId` 一类指向分享人的参数，不发出去。
const PAGE_PARAMS: [&str; 2] = ["xsec_token", "xsec_source"];

impl NoteLink {
    /// 要请求的地址：笔记页的源（scheme 也用源的），原来的路径，只留 [`PAGE_PARAMS`]。
    pub fn page_url(&self, site: &Site) -> Url {
        page_url(site, &self.url)
    }

    pub fn xsec_token(&self) -> Option<String> {
        self.url
            .query_pairs()
            .find(|(k, _)| k == "xsec_token")
            .map(|(_, v)| v.into_owned())
            .filter(|v| !v.trim().is_empty())
    }
}

/// 笔记页主机上的一个地址，换到源的 scheme 上，只留 [`PAGE_PARAMS`]。
pub(crate) fn page_url(site: &Site, url: &Url) -> Url {
    let mut target = site.web_origin().clone();
    target.set_path(url.path());
    let kept: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(k, _)| PAGE_PARAMS.contains(&k.as_ref()))
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    if !kept.is_empty() {
        target.query_pairs_mut().extend_pairs(kept);
    }
    target
}

/// 短链主机上的一个地址，换到那个源的 scheme 上。
fn short_url(origin: &Url, url: &Url) -> Url {
    let mut target = origin.clone();
    target.set_path(url.path());
    target.set_query(url.query());
    target
}

pub(crate) enum Link {
    Note(NoteLink),
    Short(Url),
}

pub(crate) fn classify(site: &Site, input: &str) -> Option<Link> {
    let url = Url::parse(input.trim()).ok()?;
    if site.is_web(&url) {
        let id = note_id_in_path(&url)?;
        let mut target = site.web_origin().clone();
        target.set_path(url.path());
        target.set_query(url.query());
        return Some(Link::Note(NoteLink { id, url: target }));
    }
    let origin = site.short_origin(&url)?;
    url.path_segments()?.find(|s| !s.is_empty())?;
    Some(Link::Short(short_url(origin, &url)))
}

/// 展开短链：只请求短链主机，不带 cookie；`Location` 一指向长链就停。
/// 返回 `None` 表示它指向的不是笔记（例如分享的是主页）。
pub(crate) fn expand(http: &Http<'_>, short: &Url) -> Result<Option<NoteLink>, Error> {
    let mut at = short.clone();
    for _ in 0..SHORT_HOPS {
        let response = http.get(&at, Purpose::Short, SHORT_TIMEOUT)?;
        let status = response.status().as_u16();
        let Some(next) = http::location(&response, &at)? else {
            return Err(match status {
                404 | 410 => Error::NotFound(format!("short link answered HTTP {status}")),
                429 => Error::RateLimited {
                    message: "short link answered HTTP 429".into(),
                    retry_after: http::retry_after(&response),
                },
                500..=599 => Error::Network(format!("short link answered HTTP {status}")),
                _ => Error::UpstreamChanged(format!(
                    "short link answered HTTP {status} without a redirect"
                )),
            });
        };
        if let Some(origin) = http.site().short_origin(&next) {
            at = short_url(origin, &next);
            continue;
        }
        if !http.site().is_web(&next) {
            return Ok(None);
        }
        if let Some(id) = note_id_in_path(&next) {
            return Ok(Some(NoteLink { id, url: next }));
        }
        let path = next.path();
        if path.starts_with("/website-login/captcha") {
            return Err(Error::Blocked(format!("short link led to {path}")));
        }
        if path.starts_with("/404") {
            return Err(Error::NotFound(format!("short link led to {path}")));
        }
        return Ok(None);
    }
    Err(Error::UpstreamChanged(format!(
        "short link redirected more than {SHORT_HOPS} times"
    )))
}

pub fn canonicalize(site: &Site, input: &str) -> Result<Canonicalized, Error> {
    match classify(site, input) {
        None => Ok(Canonicalized::NoMatch),
        Some(Link::Note(note)) => Ok(Canonicalized::Match {
            identity: identity(&note.id),
            resolved_url: None,
        }),
        Some(Link::Short(short)) => {
            let http = Http::new(site, Duration::from_secs(9));
            Ok(match expand(&http, &short)? {
                Some(note) => Canonicalized::Match {
                    identity: identity(&note.id),
                    // 原样保留展开后的全部参数，`xsec_token` 在里面
                    resolved_url: Some(note.url.into()),
                },
                None => Canonicalized::NoMatch,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "671234567890abcdef012345";

    fn id_of(input: &str) -> Option<String> {
        match classify(&Site::xiaohongshu(), input)? {
            Link::Note(n) => Some(n.id),
            Link::Short(_) => None,
        }
    }

    #[test]
    fn long_link_shapes() {
        let table = [
            (
                format!(
                    "https://www.xiaohongshu.com/explore/{ID}?xsec_token=AB%3D&xsec_source=pc_feed"
                ),
                true,
            ),
            (
                format!("https://www.xiaohongshu.com/search_result/{ID}?xsec_token=x"),
                true,
            ),
            (
                format!(
                    "https://www.xiaohongshu.com/discovery/item/{ID}?app_platform=ios&xsec_token=x"
                ),
                true,
            ),
            (
                format!(
                    "https://www.xiaohongshu.com/user/profile/5b56be0014de415b2db830a6/{ID}?xsec_token=x"
                ),
                true,
            ),
            (format!("http://www.xiaohongshu.com/explore/{ID}/"), true),
            (
                format!("https://www.xiaohongshu.com/explore/{}", ID.to_uppercase()),
                true,
            ),
            ("https://www.xiaohongshu.com/explore".into(), false),
            (
                "https://www.xiaohongshu.com/user/profile/5b56be0014de415b2db830a6".into(),
                false,
            ),
            (
                format!("https://www.xiaohongshu.com/explore/{}", &ID[..23]),
                false,
            ),
            (
                "https://www.xiaohongshu.com/explore/zz1234567890abcdef012345".into(),
                false,
            ),
            (format!("https://xiaohongshu.com/explore/{ID}"), false),
            (format!("https://example.com/explore/{ID}"), false),
            ("not a url".into(), false),
        ];
        for (input, matched) in table {
            assert_eq!(id_of(&input).as_deref() == Some(ID), matched, "{input}");
        }
    }

    #[test]
    fn long_links_keep_every_parameter_and_go_over_https() {
        let Some(Link::Note(n)) = classify(
            &Site::xiaohongshu(),
            &format!(
                "http://www.xiaohongshu.com/discovery/item/{ID}?xsec_token=AB%3D&xsec_source=app_share"
            ),
        ) else {
            panic!()
        };
        assert_eq!(
            n.url.as_str(),
            format!(
                "https://www.xiaohongshu.com/discovery/item/{ID}?xsec_token=AB%3D&xsec_source=app_share"
            )
        );
        assert_eq!(n.xsec_token().as_deref(), Some("AB="));
    }

    #[test]
    fn page_requests_carry_only_the_token_parameters() {
        let site = Site::xiaohongshu();
        let Some(Link::Note(n)) = classify(
            &site,
            &format!(
                "http://www.xiaohongshu.com/discovery/item/{ID}?app_platform=ios&xsec_token=AB%3D&shareRedId=me&xsec_source=app_share&author_share=1"
            ),
        ) else {
            panic!()
        };
        assert_eq!(
            n.page_url(&site).as_str(),
            format!(
                "https://www.xiaohongshu.com/discovery/item/{ID}?xsec_token=AB%3D&xsec_source=app_share"
            )
        );
    }

    #[test]
    fn short_link_shapes() {
        let site = Site::xiaohongshu();
        for input in [
            "https://xhslink.com/WNd9gI",
            "http://xhslink.com/a/jqL6B32eU0F7",
            "https://xhslink.cn/m/EXCcWbUXnl",
            "https://xhslink.com/o/3y3uwYYeyHn",
        ] {
            assert!(
                matches!(classify(&site, input), Some(Link::Short(_))),
                "{input}"
            );
        }
        assert!(classify(&site, "https://xhslink.com/").is_none());
        assert!(classify(&site, "https://xhslink.org/abc").is_none());
    }

    #[test]
    fn canonicalizes_long_links_without_network() {
        let site = Site::xiaohongshu();
        let got = canonicalize(
            &site,
            &format!("https://www.xiaohongshu.com/user/profile/u/{ID}?xsec_token=t"),
        )
        .unwrap();
        assert_eq!(
            got,
            Canonicalized::Match {
                identity: identity(ID),
                resolved_url: None
            }
        );
        assert_eq!(
            canonicalize(&site, "https://example.com/a").unwrap(),
            Canonicalized::NoMatch
        );
    }
}
