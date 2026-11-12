//! 这个插件会失败的几种方式，和它们对应的协议错误码（协议 §3.3）。

use usuyo_plugin_kit::protocol::{CallFailure, ErrorCode, Need, NotFoundReason};

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
    /// XHS-Downloader 的回答和插件认得的不一样（形状、状态码）。
    #[error("XHS-Downloader answered something unexpected: {0}")]
    ServiceChanged(String),
    #[error(transparent)]
    State(#[from] StateError),
    #[error("network: {0}")]
    Network(String),
    /// 宿主交来的访客身份文件读不了或形状不对（宿主写的，不是你填的）。
    #[error("{0}")]
    Visitor(String),
    #[error("{0}")]
    Internal(String),
    /// 本机的 XHS-Downloader 用不上：设置不合格、没在运行、令牌不对。`need` 是协议 §3.3 的 `needs` 项。
    #[error("{message}")]
    Missing { need: &'static str, message: String },
    #[error("write output: {0}")]
    Io(#[from] std::io::Error),
    /// 页面回了，但不是能用的回答：带着页面的开头，宿主记进失败日志。
    #[error("{error}")]
    Answered { error: Box<Error>, answer: String },
}

/// `Error::Missing` 的三种 `need`。
pub const NEED_ENDPOINT: &str = "setting:endpoint";
pub const NEED_TOKEN: &str = "setting:xhs-token";
pub const NEED_SERVICE: &str = "service:endpoint";

/// 服务没在运行时给用户的一句话，`health` 的检查也用它。
pub const SERVICE_DOWN_ZH: &str = "XHS-Downloader 没有在运行：请先在它的文件夹里运行 uv run main.py api 启动它的 API 模式（它的设置里 cookie 请留空），再重试";
const SERVICE_DOWN_EN: &str = "XHS-Downloader is not running: start its API mode first with `uv run main.py api` in its folder (leave the cookie in its settings empty), then retry";

/// 留给失败日志的页面开头最多多少字节。
const ANSWER_BYTES: usize = 4096;

impl Error {
    /// 带上页面的开头（`text` 是整页，只留开头）。
    pub fn answered(self, text: &str) -> Error {
        let mut end = text.len().min(ANSWER_BYTES);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        Error::Answered {
            error: Box::new(self),
            answer: text[..end].to_owned(),
        }
    }
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
            Error::Answered { error, answer } => error.to_failure(lang).with_detail(answer),
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
            Error::ServiceChanged(_) => with(
                ErrorCode::UpstreamChanged,
                Some(lang.pick(
                    "XHS-Downloader 的回答和这个插件认得的不一样：请把它更新到最新版，或等插件更新",
                    "XHS-Downloader answered something this plugin does not know: update it to the latest version, or wait for a plugin update",
                )),
            ),
            Error::Network(_) => with(ErrorCode::Network, None),
            Error::Missing { need, .. } => {
                let mut failure = with(
                    ErrorCode::DependencyMissing,
                    Some(match *need {
                        NEED_ENDPOINT => lang.pick(
                            "插件设置里的 XHS-Downloader 服务地址要写成本机地址，例如 http://127.0.0.1:5556",
                            "The XHS-Downloader address in the plugin settings must be a local address such as http://127.0.0.1:5556",
                        ),
                        NEED_TOKEN => lang.pick(
                            "XHS-Downloader 要访问令牌：把它启动时在终端打印的令牌（或 uv run main.py token 的输出）填进插件设置的「XHS-Downloader 访问令牌」；已经填了的话，是令牌不对",
                            "XHS-Downloader needs its access token: put the token it prints when it starts (or the output of `uv run main.py token`) into the plugin setting; if it is already there, it is wrong",
                        ),
                        _ => lang.pick(SERVICE_DOWN_ZH, SERVICE_DOWN_EN),
                    }),
                );
                failure.needs = vec![
                    Need::try_from((*need).to_owned()).expect("constant need"),
                ];
                failure
            }
            Error::Visitor(_) | Error::Internal(_) | Error::Io(_) => {
                with(ErrorCode::Internal, None)
            }
        }
    }
}
