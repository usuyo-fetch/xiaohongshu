//! 方法 `xhs-downloader`：问你自己在本机运行的 XHS-Downloader（GPL-3.0，API 模式）要一篇笔记，
//! 插件不带它、不下载它、不装它。
//!
//! 接口形状来自它 2.8 版的源码和 README（2026-11-12 用 `gh api` 读的，没有运行过它，没有对小红书发过请求）：
//! `POST <服务地址>/xhs/detail`，请求头 `Authorization: Bearer <令牌>`，请求体
//! `{"url", "download", "check_record"}`；回答 `{"message", "params", "data"}`，`data` 里是中文字段名
//! （`作品ID`、`作品类型`、`作品标题`、`作品描述`、`作品标签`、`时间戳`、`作者ID`、`作者昵称`、四个互动数、
//! `下载地址`、`动图地址`、`封面地址`）；`GET /openapi.json` 是它的接口文档（FastAPI）。
//!
//! 插件对服务只做这几件事：
//! - 请求体固定 `download: false`（不让它下载）和 `check_record: false`（它默认是 `true`，会跳过下载过的作品）；
//!   不带 `cookie`、`proxy` 字段：服务没收到 `cookie` 时用它自己配置文件里的那份（`request.py` 里
//!   `if cookie:` 才换成请求里的），所以 README 和提示都写「它的设置里 cookie 请留空」。
//! - 令牌只放在 `Authorization` 头里，不进日志、错误、`detail`、输出目录。
//! - 服务地址只能是本机地址（回环），请求不经宿主的代理，只连这一个地址。
//!
//! 拿到的地址（图片、视频）由 [`crate::fetch::write_note`] 下载，和 `note-page` 一样只去 `*.xhscdn.com` 和
//! `ci.xiaohongshu.com`（服务默认的图片格式 JPEG 给的是这台主机上的地址）。

use std::collections::BTreeSet;
use std::fmt;
use std::io::Read;
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::time::Duration;

use serde_json::Value;
use url::{Host, Url};
use usuyo_plugin_kit::http::{Client, Hosts, Policy};
use usuyo_plugin_kit::protocol::{FetchHealth, FetchOutput, OutputKind, Rfc3339, UpstreamVersion};

use crate::error::{Error, NEED_ENDPOINT, NEED_SERVICE, NEED_TOKEN, SERVICE_DOWN_ZH};
use crate::fetch::{self, BUDGET, OUTPUTS, Page};
use crate::link::{self, Link};
use crate::site::Site;
use crate::state::{Author, Counts, Image, Note, NoteKind, Stream};

/// 清单里的方法 id。
pub const METHOD: &str = "xhs-downloader";
/// 清单里设置项 `endpoint` 的默认值，XHS-Downloader 的 API 模式默认监听 5556。
pub const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:5556";
const ENDPOINT_KEY: &str = "endpoint";
const TOKEN_KEY: &str = "xhs-token";

/// 服务自己有请求间隔（`avg_delay` 默认 6 秒）和重试，一次要等一阵；
/// 但宿主在发过 `progress` 以后 120 秒没有新的就杀整次调用，所以比它短。
const SERVICE_TIMEOUT: Duration = Duration::from_secs(100);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// 一篇笔记的回答远小于这个数。
const ANSWER_BYTES: u64 = 8 << 20;
const SETTINGS_BYTES: u64 = 1 << 20;
const USER_AGENT: &str = "usuyo-fetch-xiaohongshu";

/// 本机的 XHS-Downloader：服务地址和访问令牌。
pub struct Service {
    endpoint: Url,
    token: Option<String>,
    /// 等服务回答多久；测试里换成短的。
    timeout: Duration,
}

impl fmt::Debug for Service {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Service")
            .field("endpoint", &self.endpoint.as_str())
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl Service {
    /// 服务地址要是 `http://` 的本机地址（回环 IP 或 `localhost`），不带账号密码、路径、参数。
    /// 令牌没填是 `None`；填了的不能含空白和控制字符，错误里不写出它。
    pub fn new(endpoint: &str, token: Option<&str>) -> Result<Self, Error> {
        let bad = |why: &str| Error::Missing {
            need: NEED_ENDPOINT,
            message: format!("the `{ENDPOINT_KEY}` setting is not usable: {why}"),
        };
        let mut url = Url::parse(endpoint.trim()).map_err(|_| bad("not a URL"))?;
        if url.scheme() != "http" {
            return Err(bad("the scheme is not http"));
        }
        let local = match url.host() {
            Some(Host::Ipv4(ip)) => ip.is_loopback(),
            Some(Host::Ipv6(ip)) => ip.is_loopback(),
            Some(Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
            None => false,
        };
        if !local {
            return Err(bad("the host is not a local address"));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(bad("it carries a user name or password"));
        }
        if !matches!(url.path(), "" | "/") || url.query().is_some() || url.fragment().is_some() {
            return Err(bad("it carries a path, query or fragment"));
        }
        url.set_path("");
        let token = match token.map(str::trim).filter(|t| !t.is_empty()) {
            None => None,
            Some(t) if t.bytes().all(|b| (0x21..0x7f).contains(&b)) => Some(t.to_owned()),
            Some(_) => {
                return Err(Error::Missing {
                    need: NEED_TOKEN,
                    message: format!(
                        "the `{TOKEN_KEY}` setting has characters a header cannot carry"
                    ),
                });
            }
        };
        Ok(Self {
            endpoint: url,
            token,
            timeout: SERVICE_TIMEOUT,
        })
    }

    /// 换等服务回答的时长。
    pub fn with_timeout(self, timeout: Duration) -> Self {
        Self { timeout, ..self }
    }

    /// 读宿主交来的设置：`USUYO_SETTINGS`（非密钥，`endpoint`）和 `USUYO_SECRETS`（`xhs-token`）。
    /// 文件读不出来的错误不写出文件内容。
    pub fn from_env() -> Result<Self, Error> {
        let settings = read_json("USUYO_SETTINGS")?;
        let secrets = read_json("USUYO_SECRETS")?;
        let endpoint = match settings.as_ref().and_then(|s| s.get(ENDPOINT_KEY)) {
            None | Some(Value::Null) => DEFAULT_ENDPOINT,
            Some(Value::String(s)) if !s.trim().is_empty() => s,
            Some(Value::String(_)) => DEFAULT_ENDPOINT,
            Some(_) => {
                return Err(Error::Missing {
                    need: NEED_ENDPOINT,
                    message: format!("the `{ENDPOINT_KEY}` setting is not a string"),
                });
            }
        };
        let token = match secrets.as_ref().and_then(|s| s.get(TOKEN_KEY)) {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => Some(s.as_str()),
            Some(_) => {
                return Err(Error::Internal(format!(
                    "USUYO_SECRETS: `{TOKEN_KEY}` is not a string"
                )));
            }
        };
        Self::new(endpoint, token)
    }

    /// 服务地址（不含令牌），给日志和提示用。
    pub fn endpoint(&self) -> &str {
        self.endpoint.as_str().trim_end_matches('/')
    }

    /// 服务有没有在这个地址上监听：只连一下 TCP，不发请求。
    fn listening(&self) -> Result<(), std::io::Error> {
        let host = self.endpoint.host_str().unwrap_or_default();
        let port = self.endpoint.port_or_known_default().unwrap_or(80);
        let mut last = std::io::Error::other("the address has no socket address");
        for addr in (host, port).to_socket_addrs()? {
            match TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
                Ok(_) => return Ok(()),
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    fn require_listening(&self) -> Result<(), Error> {
        self.listening().map_err(|e| Error::Missing {
            need: NEED_SERVICE,
            message: format!("nothing answers at {}: {e}", self.endpoint()),
        })
    }

    /// 只连这一个地址、不经代理：回环地址由沙箱直接放行（协议 §3.2 的「网络出口」）。
    fn client(&self) -> Client {
        Client::new(USER_AGENT, Hosts::only(self.endpoint.host_str())).with_policy(Policy::strict())
    }

    fn url(&self, path: &str) -> Result<Url, Error> {
        self.endpoint
            .join(path)
            .map_err(|e| Error::Internal(format!("service URL: {e}")))
    }

    /// 问 `link` 这篇笔记，回服务答的 `data` 对象。
    fn detail(&self, link: &str) -> Result<Value, Error> {
        let token = self.token.as_deref().ok_or_else(|| Error::Missing {
            need: NEED_TOKEN,
            message: format!("the `{TOKEN_KEY}` setting is empty"),
        })?;
        self.require_listening()?;
        let body = serde_json::json!({"url": link, "download": false, "check_record": false});
        let mut response = self
            .client()
            .post(&self.url("/xhs/detail")?, body.to_string())
            .header("Authorization", &format!("Bearer {token}"))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .timeout(self.timeout)
            .allow_private_first_hop(true)
            .send()
            .map_err(service_error)?;
        let status = response.status();
        let raw = response.read_to_vec(ANSWER_BYTES).map_err(service_error)?;
        let text = String::from_utf8_lossy(&raw);
        match status {
            200 => {}
            // 令牌没带或不对（README：未携带或令牌无效时返回 403）
            401 | 403 => {
                return Err(Error::Missing {
                    need: NEED_TOKEN,
                    message: format!("XHS-Downloader answered HTTP {status}"),
                });
            }
            other => {
                return Err(Error::ServiceChanged(format!("HTTP {other}")).answered(&text));
            }
        }
        let answer: Value = serde_json::from_str(&text)
            .map_err(|_| Error::ServiceChanged("the answer is not JSON".into()).answered(&text))?;
        let message = answer
            .get("message")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                Error::ServiceChanged("the answer has no `message`".into()).answered(&text)
            })?;
        match answer.get("data") {
            None | Some(Value::Null) => Err(Error::NotFound(format!(
                "XHS-Downloader returned no data (`{}`)",
                clip(message)
            ))
            .answered(&text)),
            Some(Value::Object(map)) if map.is_empty() => Err(Error::NotFound(format!(
                "XHS-Downloader returned no data (`{}`)",
                clip(message)
            ))
            .answered(&text)),
            // 只在 `check_record` 为真且有下载记录时才会这样；请求里写了 `false`
            Some(Value::Object(map)) if map.len() == 1 && map.contains_key("message") => {
                Err(Error::ServiceChanged(
                    "XHS-Downloader skipped the note by its download record although check_record is false"
                        .into(),
                )
                .answered(&text))
            }
            Some(data @ Value::Object(_)) => Ok(data.clone()),
            Some(_) => Err(Error::ServiceChanged("`data` is not an object".into()).answered(&text)),
        }
    }

    /// `GET /openapi.json` 里的 `info.title` 和 `info.version`；`health` 用它认出这是不是 XHS-Downloader。
    fn about(&self) -> Result<(String, String), String> {
        let url = self.url("/openapi.json").map_err(|e| e.to_string())?;
        let mut response = self
            .client()
            .get(&url)
            .header("Accept", "application/json")
            .timeout(PROBE_TIMEOUT)
            .allow_private_first_hop(true)
            .send()
            .map_err(|e| e.to_string())?;
        if response.status() != 200 {
            return Err(format!("HTTP {}", response.status()));
        }
        let raw = response
            .read_to_vec(ANSWER_BYTES)
            .map_err(|e| e.to_string())?;
        let doc: Value = serde_json::from_slice(&raw).map_err(|_| "not JSON".to_owned())?;
        let field = |name: &str| {
            doc.pointer(&format!("/info/{name}"))
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| format!("no info.{name}"))
        };
        Ok((field("title")?, field("version")?))
    }
}

fn service_error(e: usuyo_plugin_kit::http::Error) -> Error {
    if e.is_refusal() {
        Error::Internal(format!("the request to XHS-Downloader was refused: {e}"))
    } else {
        Error::Network(format!("XHS-Downloader: {e}"))
    }
}

/// 日志里引服务的话时最多留这么多字符。
fn clip(text: &str) -> String {
    text.chars().take(200).collect()
}

fn read_json(var: &str) -> Result<Option<Value>, Error> {
    let Some(path) = std::env::var_os(var) else {
        return Ok(None);
    };
    let mut bytes = Vec::new();
    std::fs::File::open(&path)
        .and_then(|f| f.take(SETTINGS_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|e| Error::Internal(format!("cannot read {var}: {e}")))?;
    if bytes.len() as u64 > SETTINGS_BYTES {
        return Err(Error::Internal(format!("{var} is too large")));
    }
    // serde 的错误文字不含读到的值，但密钥文件的内容不进任何错误：只报文件不对
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| Error::Internal(format!("{var} is not a JSON object")))
}

pub struct Request<'a> {
    /// 原链接或 `canonicalize` 给的 `resolvedUrl`。
    pub input: &'a str,
    pub out: &'a Path,
    /// `None` 是全要。
    pub want: Option<BTreeSet<OutputKind>>,
}

/// 一篇笔记：交给服务的链接只带 `xsec_token`、`xsec_source`（和 `note-page` 一样）；短链原样交给它，
/// 由服务去展开。写进输出的身份和 `note-page` 的一样。
pub fn fetch(site: &Site, service: &Service, request: Request<'_>) -> Result<FetchOutput, Error> {
    let wanted = request.want.clone().unwrap_or_else(|| OUTPUTS.into());
    if let Some(other) = wanted.iter().find(|w| !OUTPUTS.contains(w)) {
        return Err(Error::InvalidArgs(format!("cannot give `{other}`")));
    }
    let (sent, link_id) = match link::classify(site, request.input) {
        Some(Link::Note(note)) => {
            // 没有 xsec_token 的链接，页面只会回「当前笔记暂时无法浏览」，不必去问
            if note.xsec_token().is_none() {
                return Err(Error::LinkExpired("the link carries no xsec_token".into()));
            }
            (note.page_url(site).to_string(), Some(note.id))
        }
        Some(Link::Short(short)) => (short.to_string(), None),
        None => {
            return Err(Error::Unsupported(format!(
                "`{}` is not a note link",
                request.input
            )));
        }
    };
    let fetched_at = Rfc3339::from_timestamp(jiff::Timestamp::now());
    eprintln!(
        "{}",
        usuyo_plugin_kit::protocol::PluginEvent::Progress {
            done: None,
            total: None,
            unit: None,
            message: Some("向 XHS-Downloader 要这篇笔记".into()),
        }
        .stderr_line()
    );
    let data = service.detail(&sent)?;
    let note =
        note_of(data.clone(), link_id.as_deref()).map_err(|e| e.answered(&data.to_string()))?;
    let canonical = Url::parse(&link::canonical_url(&note.id))
        .map_err(|e| Error::Internal(format!("canonical URL: {e}")))?;
    let http_link = link::NoteLink {
        id: note.id.clone(),
        url: canonical,
    };
    let http = crate::http::Http::new(site, BUDGET);
    fetch::write_note(
        &http,
        Page { note, status: None },
        &http_link,
        &wanted,
        false,
        fetched_at,
        request.out,
    )
}

/// 服务答的 `data` 换成 [`Note`]。字段缺了的按没有算，只有 `作品ID`、`作品类型` 缺了或不成样子是错。
fn note_of(data: Value, link_id: Option<&str>) -> Result<Note, Error> {
    let changed = Error::ServiceChanged;
    let text = |key: &str| {
        data.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    let id = text("作品ID")
        .and_then(|id| link::note_id(&id))
        .ok_or_else(|| changed("the answer has no valid `作品ID`".into()))?;
    if let Some(wanted) = link_id
        && wanted != id
    {
        return Err(changed(format!(
            "the answer is for note {id}, not {wanted}"
        )));
    }
    // 类型的值跟着服务的界面语言走：zh_CN 是「视频」「图文」「图集」，en_US 是 video、image、album
    let kind = match text("作品类型")
        .map(|t| t.to_lowercase())
        .as_deref()
        .ok_or_else(|| changed("the answer has no `作品类型`".into()))?
    {
        "视频" | "video" => NoteKind::Video,
        "图文" | "图集" | "image" | "album" => NoteKind::Normal,
        other => {
            return Err(changed(format!(
                "XHS-Downloader did not recognise the note type (`{}`)",
                clip(other)
            )));
        }
    };
    let strings = |key: &str| -> Result<Vec<Option<String>>, Error> {
        match data.get(key) {
            None | Some(Value::Null) => Ok(Vec::new()),
            Some(Value::Array(items)) => items
                .iter()
                .map(|i| match i {
                    Value::Null => Ok(None),
                    Value::String(s) if !s.trim().is_empty() => Ok(Some(s.clone())),
                    Value::String(_) => Ok(None),
                    _ => Err(changed(format!(
                        "`{key}` holds something that is not a string"
                    ))),
                })
                .collect(),
            Some(_) => Err(changed(format!("`{key}` is not a list"))),
        }
    };
    let downloads = strings("下载地址")?;
    let lives = strings("动图地址")?;
    let (images, video) = match kind {
        NoteKind::Video => {
            let cover = text("封面地址");
            let images = cover
                .map(|url| Image {
                    url: Some(url),
                    width: None,
                    height: None,
                    live: None,
                })
                .into_iter()
                .collect();
            let video = downloads.into_iter().flatten().next().map(|url| Stream {
                url,
                backup: Vec::new(),
                codec: "unknown".into(),
                width: None,
                height: None,
                duration_ms: None,
            });
            (images, video)
        }
        NoteKind::Normal => {
            let images = downloads
                .into_iter()
                .enumerate()
                .map(|(i, url)| Image {
                    url,
                    width: None,
                    height: None,
                    live: lives.get(i).cloned().flatten().map(|url| Stream {
                        url,
                        backup: Vec::new(),
                        codec: "unknown".into(),
                        width: None,
                        height: None,
                        duration_ms: None,
                    }),
                })
                .collect();
            (images, None)
        }
    };
    // 服务把没有的互动数记成 "-1"
    let count = |key: &str| text(key).filter(|c| c != "-1");
    let tags = text("作品标签")
        .map(|t| t.split_whitespace().map(str::to_owned).collect())
        .unwrap_or_default();
    Ok(Note {
        id,
        kind,
        title: text("作品标题"),
        desc: text("作品描述"),
        published_ms: data
            .get("时间戳")
            .and_then(Value::as_f64)
            .filter(|t| t.is_finite() && *t > 0.0)
            .map(|secs| (secs * 1000.0).round() as i64),
        updated_ms: None,
        ip_location: None,
        author: Some(Author {
            user_id: text("作者ID"),
            nickname: text("作者昵称"),
        }),
        tags,
        counts: Counts {
            liked: count("点赞数量"),
            collected: count("收藏数量"),
            comments: count("评论数量"),
            shared: count("分享数量"),
        },
        images,
        video,
        raw: data,
    })
}

/// `health`：只看本机，不碰小红书。服务没在监听、没填令牌、地址上答的不像 XHS-Downloader，都只挡这个方法。
pub fn health(health: FetchHealth) -> FetchHealth {
    let methods = [METHOD];
    let service = match Service::from_env() {
        Ok(s) => s,
        Err(Error::Missing { need, message }) => {
            let sentence = if need == NEED_ENDPOINT {
                "插件设置里的 XHS-Downloader 服务地址要写成本机地址，例如 http://127.0.0.1:5556"
                    .to_owned()
            } else {
                "插件设置里的 XHS-Downloader 访问令牌填得不对：请重新粘贴它启动时在终端打印的令牌"
                    .to_owned()
            };
            eprintln!("{message}");
            return health.check("XHS-Downloader 设置", false, Some(sentence), &methods);
        }
        Err(other) => {
            return health.check(
                "XHS-Downloader 设置",
                false,
                Some(format!("读不出宿主交来的设置：{other}")),
                &methods,
            );
        }
    };
    let mut health = health;
    if service.listening().is_err() {
        health = health.check(
            "XHS-Downloader 服务",
            false,
            Some(format!("{SERVICE_DOWN_ZH}（地址 {}）", service.endpoint())),
            &methods,
        );
    } else {
        match service.about() {
            Ok((title, version)) if title == "XHS-Downloader" => {
                health.upstream.push(UpstreamVersion {
                    name: title,
                    version: version.clone(),
                });
                health = health.check(
                    "XHS-Downloader 服务",
                    true,
                    Some(format!(
                        "{} 上有 XHS-Downloader {version} 在运行",
                        service.endpoint()
                    )),
                    &methods,
                );
            }
            other => {
                let seen = other.map_or_else(|why| why, |(title, _)| title);
                health = health.check(
                    "XHS-Downloader 服务",
                    false,
                    Some(format!(
                        "{} 上答话的不像 XHS-Downloader（{}）：请核对插件设置里的服务地址，并确认它是用 API 模式启动的",
                        service.endpoint(),
                        clip(&seen)
                    )),
                    &methods,
                );
            }
        }
    }
    health.check(
        "XHS-Downloader 访问令牌",
        service.token.is_some(),
        Some(if service.token.is_some() {
            "已填访问令牌".to_owned()
        } else {
            "没填访问令牌：请把 XHS-Downloader 启动时在终端打印的令牌（或 uv run main.py token 的输出）填进插件设置的「XHS-Downloader 访问令牌」".to_owned()
        }),
        &methods,
    )
}
