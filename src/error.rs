//! 这个插件会失败的几种方式，和它们对应的协议错误码（协议 §3.3）。

use usuyo_plugin_kit::protocol::{CallFailure, ErrorCode, NotFoundReason};

use crate::state::StateError;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not a Xiaohongshu note link: {0}")]
    Unsupported(String),
    #[error("invalid arguments: {0}")]
    InvalidArgs(String),
    /// 链接里没有 `xsec_token`，或页面说它不能用了（`error_code=300031`）。
    #[error("xsec_token is missing or no longer accepted: {0}")]
    LinkExpired(String),
    #[error("note not found: {0}")]
    NotFound(String),
    /// 被送去首页或登录页；`cookie_tried` 说这次带没带访客 cookie。
    #[error("sent to {at} (visitor cookie used: {cookie_tried})")]
    VisitorRequired { at: String, cookie_tried: bool },
    #[error("verification page or risk-control answer: {0}")]
    Blocked(String),
    #[error("rate limited: {message}")]
    RateLimited {
        message: String,
        retry_after: Option<u64>,
    },
    #[error("page changed: {0}")]
    UpstreamChanged(String),
    #[error(transparent)]
    State(#[from] StateError),
    #[error("network: {0}")]
    Network(String),
    /// 宿主交来的访客身份文件读不了或形状不对（宿主写的，不是你填的）。
    #[error("{0}")]
    Visitor(String),
    #[error("{0}")]
    Internal(String),
    #[error("write output: {0}")]
    Io(#[from] std::io::Error),
}

/// `hint` 按 `USUYO_LOCALE` 写：`zh` 开头写中文，其余写英文。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Zh,
    En,
}

impl Lang {
    pub fn from_locale(locale: Option<&str>) -> Self {
        match locale {
            Some(l) if !l.to_ascii_lowercase().starts_with("zh") => Lang::En,
            _ => Lang::Zh,
        }
    }

    fn pick(self, zh: &str, en: &str) -> String {
        match self {
            Lang::Zh => zh.into(),
            Lang::En => en.into(),
        }
    }
}

impl Error {
    pub fn to_failure(&self, lang: Lang) -> CallFailure {
        let message = self.to_string();
        let with = |code: ErrorCode, hint: Option<String>| CallFailure {
            hint,
            ..CallFailure::new(code, message.clone())
        };
        match self {
            Error::Unsupported(_) => with(ErrorCode::UnsupportedInput, None),
            Error::InvalidArgs(_) => with(ErrorCode::InvalidArgs, None),
            Error::LinkExpired(_) => CallFailure {
                reason: Some(NotFoundReason::LinkExpired),
                ..with(
                    ErrorCode::NotFound,
                    Some(lang.pick(
                        "链接里的 xsec_token 没有了或已失效，请从小红书重新分享，或在收藏页重新交一次清单",
                        "The link's xsec_token is missing or expired; share the note again, or send the list again from your saved page",
                    )),
                )
            },
            Error::NotFound(_) => with(ErrorCode::NotFound, None),
            Error::VisitorRequired { cookie_tried, .. } => with(
                ErrorCode::VisitorRequired,
                Some(if *cookie_tried {
                    lang.pick(
                        "带上访客身份也被送回了首页，访客身份可能过期了：在「获取插件」页的小红书下面换一份",
                        "Even with the visitor identity the page sent us home; it may have expired: take a fresh one on the fetch plugins page",
                    )
                } else {
                    lang.pick(
                        "这篇笔记不带访客身份取不到：在「获取插件」页的小红书下面取一份访客身份（不是你的账号）",
                        "This note needs a visitor identity: take one on the fetch plugins page (it is not your account)",
                    )
                }),
            ),
            Error::Blocked(_) => with(
                ErrorCode::Blocked,
                Some(lang.pick(
                    "小红书出现了验证页或风控提示，已暂停这个插件；先在你的浏览器里正常看一会儿，再点继续",
                    "Xiaohongshu showed a verification or risk-control page; this plugin is paused until you press continue",
                )),
            ),
            Error::RateLimited { retry_after, .. } => CallFailure {
                retry_after: *retry_after,
                ..with(ErrorCode::RateLimited, None)
            },
            Error::UpstreamChanged(_) | Error::State(_) => with(
                ErrorCode::UpstreamChanged,
                Some(lang.pick(
                    "小红书的页面改了，这个插件要更新",
                    "The Xiaohongshu page changed; this plugin needs an update",
                )),
            ),
            Error::Network(_) => with(ErrorCode::Network, None),
            Error::Visitor(_) | Error::Internal(_) | Error::Io(_) => {
                with(ErrorCode::Internal, None)
            }
        }
    }
}
