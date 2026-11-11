//! 取一篇笔记（协议 §3.6、§3.7）：先不带 cookie 请求笔记页；被送去首页或登录页时，
//! 有访客 cookie 就带上再试一次，只试一次；解析状态对象，按 `--want` 下载图片和视频。

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

use serde_json::json;
use url::Url;
use usuyo_plugin_kit::protocol::{
    Access, Author, Completeness, Evidence, FetchOutput, ItemKind, Media, MediaKind, OutputKind,
    PluginEvent, RelPath, Rfc3339, TextFormat,
};

use crate::error::Error;
use crate::http::{self, Http, Purpose, VisitorCookie};
use crate::link::{self, Link, NoteLink};
use crate::site::{CANONICAL_ORIGIN, Site};
use crate::state::{self, Note, NoteKind, StateError, Stream};

/// 本插件能给的输出，和清单里的 `outputs` 一致。
pub const OUTPUTS: [OutputKind; 4] = [
    OutputKind::Text,
    OutputKind::Images,
    OutputKind::Video,
    OutputKind::Meta,
];

/// 整次调用留给网络的时间，比宿主默认的 300 s 短，留出收尾。
const BUDGET: Duration = Duration::from_secs(280);
const PAGE_TIMEOUT: Duration = Duration::from_secs(30);
const PAGE_BYTES: u64 = 16 << 20;
const PAGE_HOPS: usize = 3;
const IMAGE_TIMEOUT: Duration = Duration::from_secs(30);
const IMAGE_BYTES: u64 = 40 << 20;
const IMAGES_TOTAL_BYTES: u64 = 600 << 20;
/// 一篇笔记最多 18 张图，留些余量。
const MAX_IMAGES: usize = 40;
const VIDEO_TIMEOUT: Duration = Duration::from_secs(240);
const VIDEO_BYTES: u64 = 2 << 30;
/// 实况图的动态部分只有几秒。
const LIVE_BYTES: u64 = 64 << 20;
const PROGRESS_STEP: u64 = 8 << 20;

pub struct Request<'a> {
    /// 原链接或 `canonicalize` 给的 `resolvedUrl`。
    pub input: &'a str,
    pub out: &'a Path,
    /// `None` 是全要。
    pub want: Option<BTreeSet<OutputKind>>,
    pub visitor_cookie: Option<&'a VisitorCookie>,
}

pub fn fetch(site: &Site, request: Request<'_>) -> Result<FetchOutput, Error> {
    let wanted = request.want.clone().unwrap_or_else(|| OUTPUTS.into());
    if let Some(other) = wanted.iter().find(|w| !OUTPUTS.contains(w)) {
        return Err(Error::InvalidArgs(format!("cannot give `{other}`")));
    }
    let http = Http::new(site, BUDGET);
    let note_link = match link::classify(site, request.input) {
        Some(Link::Note(n)) => n,
        Some(Link::Short(short)) => link::expand(&http, &short)?.ok_or_else(|| {
            Error::Unsupported(format!("`{}` does not lead to a note", request.input))
        })?,
        None => {
            return Err(Error::Unsupported(format!(
                "`{}` is not a note link",
                request.input
            )));
        }
    };
    // 没有 xsec_token 的链接，页面只会回「当前笔记暂时无法浏览」，不必去问
    if note_link.xsec_token().is_none() {
        return Err(Error::LinkExpired("the link carries no xsec_token".into()));
    }

    let fetched_at = Rfc3339::from_timestamp(jiff::Timestamp::now());
    let (page, cookie_used) = match load_page(&http, &note_link, None) {
        Ok(page) => (page, false),
        Err(Error::LoginRequired { at, .. }) => match request.visitor_cookie {
            Some(cookie) => match load_page(&http, &note_link, Some(cookie)) {
                Ok(page) => (page, true),
                Err(Error::LoginRequired { at, .. }) => {
                    return Err(Error::LoginRequired {
                        at,
                        cookie_tried: true,
                    });
                }
                Err(other) => return Err(other),
            },
            None => {
                return Err(Error::LoginRequired {
                    at,
                    cookie_tried: false,
                });
            }
        },
        Err(other) => return Err(other),
    };

    write_note(
        &http,
        page,
        &note_link,
        &wanted,
        cookie_used,
        fetched_at,
        request.out,
    )
}

struct Page {
    note: Note,
    status: u16,
}

/// 请求一次笔记页（加上最多几跳指向同一篇笔记的重定向），认出拿到的是什么。
/// 指向首页、登录页、验证页、404 页的重定向只看地址，不去请求。
fn load_page(
    http: &Http<'_>,
    note: &NoteLink,
    cookie: Option<&VisitorCookie>,
) -> Result<Page, Error> {
    let mut at = note.page_url(http.site());
    for _ in 0..PAGE_HOPS {
        let mut response = http.get(&at, Purpose::Page { cookie }, PAGE_TIMEOUT)?;
        let status = response.status();
        if let Some(next) = http::location(&response, &at)? {
            refuse_redirect(http.site(), &next, &note.id)?;
            at = link::page_url(http.site(), &next);
            continue;
        }
        match status {
            200 | 404 => {}
            429 => {
                return Err(Error::RateLimited {
                    message: "note page answered HTTP 429".into(),
                    retry_after: http::retry_after(&response),
                });
            }
            403 | 406 | 461 => {
                return Err(Error::Blocked(format!("note page answered HTTP {status}")));
            }
            500..=599 => return Err(Error::Network(format!("note page answered HTTP {status}"))),
            _ => {
                return Err(Error::UpstreamChanged(format!(
                    "note page answered HTTP {status}"
                )));
            }
        }
        let content_type = http::header(&response, "content-type")
            .unwrap_or_default()
            .to_ascii_lowercase();
        let body = http.read_capped(&mut response, PAGE_BYTES)?;
        let text = String::from_utf8_lossy(&body);
        if content_type.contains("json") || text.trim_start().starts_with('{') {
            return Err(risk_json(&text));
        }
        if !state::has_state(&text) {
            return Err(if looks_like_verification(&text) {
                Error::Blocked("note page is a verification page".into())
            } else if status == 404 {
                Error::NotFound("note page answered HTTP 404".into())
            } else {
                Error::State(StateError::NoState)
            });
        }
        let parsed = state::extract(&text)?;
        return match state::note(&parsed, &note.id) {
            Ok(n) => Ok(Page { note: n, status }),
            // 页面结构在、这篇的内容没给：和被送回首页一样，是要访客 cookie 的样子
            Err(StateError::Absent(id)) => Err(Error::LoginRequired {
                at: format!("a page without content for note {id}"),
                cookie_tried: cookie.is_some(),
            }),
            Err(other) => Err(other.into()),
        };
    }
    Err(Error::UpstreamChanged(format!(
        "note page redirected more than {PAGE_HOPS} times"
    )))
}

/// 重定向去处的分类（依据 xiaohongshu-cli `status.go`、ParseHub、xiaohongshu-mcp
/// `page_check.go`）。只有指向同一篇笔记的才返回 `Ok`，接着跟。
fn refuse_redirect(site: &Site, next: &Url, id: &str) -> Result<(), Error> {
    if !site.is_web(next) {
        return Err(Error::UpstreamChanged(format!(
            "note page redirected to another host `{}`",
            next.host_str().unwrap_or_default()
        )));
    }
    if let Some(other) = link::note_id_in_path(next) {
        return if other == id {
            Ok(())
        } else {
            Err(Error::UpstreamChanged(format!(
                "note page redirected to another note {other}"
            )))
        };
    }
    let path = next.path();
    let at = path.to_owned();
    if path.starts_with("/website-login/captcha") {
        return Err(Error::Blocked(format!("redirected to {path}")));
    }
    if path.starts_with("/login") || path.starts_with("/website-login") {
        return Err(Error::LoginRequired {
            at,
            cookie_tried: false,
        });
    }
    if path.starts_with("/404/sec_") {
        let code = next
            .query_pairs()
            .find(|(k, _)| k == "error_code")
            .map(|(_, v)| v.into_owned());
        return Err(match code.as_deref() {
            Some("300031") => {
                Error::LinkExpired(format!("redirected to {path} (error_code=300031)"))
            }
            other => Error::Blocked(format!(
                "redirected to {path} (error_code={})",
                other.unwrap_or("none")
            )),
        });
    }
    if path.starts_with("/404") {
        return Err(Error::NotFound(format!("redirected to {path}")));
    }
    if matches!(path, "/" | "/explore" | "/explore/") {
        return Err(Error::LoginRequired {
            at,
            cookie_tried: false,
        });
    }
    Err(Error::UpstreamChanged(format!("redirected to {path}")))
}

/// 笔记页回了 JSON 而不是页面：按 code 分（xiaohongshu-cli `codeStatus`）。
fn risk_json(text: &str) -> Error {
    let value: serde_json::Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => return Error::UpstreamChanged("note page answered unreadable JSON".into()),
    };
    let code = value.get("code").and_then(serde_json::Value::as_i64);
    let msg = value
        .get("msg")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let described = format!("note page answered JSON code {code:?} `{msg}`");
    match code {
        Some(300012 | 300013 | 300015 | 461 | -1) => Error::Blocked(described),
        Some(-510 | 10013) => Error::RateLimited {
            message: described,
            retry_after: None,
        },
        Some(-100 | -101) => Error::LoginRequired {
            at: described,
            cookie_tried: false,
        },
        Some(300031) => Error::LinkExpired(described),
        _ if matches!(msg, "笔记不存在" | "该笔记不存在或已删除") => {
            Error::NotFound(described)
        }
        _ => Error::UpstreamChanged(described),
    }
}

/// 验证页、风控页的字样（OpenCLI #2562、xiaohongshu-mcp）。
fn looks_like_verification(html: &str) -> bool {
    [
        "website-login/captcha",
        "Security Verification",
        "安全验证",
        "请求太频繁",
        "访问频次异常",
        "安全限制",
    ]
    .iter()
    .any(|m| html.contains(m))
}

/// 下载要的东西，写 `note.json`、`meta.json`，拼出输出。
fn write_note(
    http: &Http<'_>,
    page: Page,
    link: &NoteLink,
    wanted: &BTreeSet<OutputKind>,
    cookie_used: bool,
    fetched_at: Rfc3339,
    out: &Path,
) -> Result<FetchOutput, Error> {
    let note = page.note;
    let mut missing = Vec::new();
    let mut reasons: Vec<String> = Vec::new();
    let mut failed: Vec<serde_json::Value> = Vec::new();

    fs::write(out.join("note.json"), pretty(&note.raw)?)?;

    let text = note
        .desc
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(str::to_owned);
    let text = if wanted.contains(&OutputKind::Text) {
        if text.is_none() {
            missing.push(OutputKind::Text);
            reasons.push("这篇笔记没有正文文字".into());
        }
        text
    } else {
        None
    };

    let mut images = Vec::new();
    let mut image_records = Vec::new();
    if wanted.contains(&OutputKind::Images) {
        let mut total = 0u64;
        for (index, image) in note.images.iter().take(MAX_IMAGES).enumerate() {
            let Some(raw) = &image.url else {
                failed.push(json!({"index": index + 1, "why": "页面没有给出图片地址"}));
                continue;
            };
            match download_image(http, raw, index + 1, out, &mut total) {
                Ok(path) => {
                    image_records.push(json!({
                        "path": path.as_str(), "url": raw,
                        "width": image.width, "height": image.height
                    }));
                    images.push(path);
                }
                Err(why) => failed.push(json!({"index": index + 1, "url": raw, "why": why})),
            }
        }
        if note.images.len() > MAX_IMAGES {
            reasons.push(format!("只取了前 {MAX_IMAGES} 张图"));
        }
        let lost = note.images.len().min(MAX_IMAGES) - images.len();
        if images.is_empty() {
            missing.push(OutputKind::Images);
            reasons.push(if note.images.is_empty() {
                "页面里没有图片".into()
            } else {
                "图片都没有下载下来".into()
            });
        } else if lost > 0 {
            reasons.push(format!("有 {lost} 张图片没有下载下来"));
        }
    }

    let mut media = Vec::new();
    let mut video_record = serde_json::Value::Null;
    if wanted.contains(&OutputKind::Video) {
        match (&note.kind, &note.video) {
            (NoteKind::Video, Some(stream)) => {
                match download_stream(http, stream, "media/video.mp4", out, VIDEO_BYTES) {
                    Ok((path, mime)) => {
                        video_record = json!({
                            "path": path.as_str(), "url": stream.url, "codec": stream.codec,
                            "width": stream.width, "height": stream.height,
                            "durationMs": stream.duration_ms
                        });
                        media.push(video_media(path, mime, stream));
                    }
                    Err(why) => {
                        failed.push(json!({"video": stream.url, "why": why}));
                        missing.push(OutputKind::Video);
                        reasons.push(format!("视频没有下载下来：{why}"));
                    }
                }
            }
            (NoteKind::Video, None) => {
                missing.push(OutputKind::Video);
                reasons.push("页面没有给出视频地址".into());
            }
            (NoteKind::Normal, _) => {}
        }
        // 实况图的动态部分
        for (index, image) in note.images.iter().take(MAX_IMAGES).enumerate() {
            let Some(live) = &image.live else { continue };
            let name = format!("media/live-{:03}.mp4", index + 1);
            match download_stream(http, live, &name, out, LIVE_BYTES) {
                Ok((path, mime)) => media.push(video_media(path, mime, live)),
                Err(why) => failed.push(json!({"live": live.url, "index": index + 1, "why": why})),
            }
        }
        if note.kind == NoteKind::Normal && media.is_empty() {
            missing.push(OutputKind::Video);
            reasons.push(if note.images.iter().any(|i| i.live.is_some()) {
                "实况图的视频没有下载下来".into()
            } else {
                "这是图文笔记，没有视频".into()
            });
        }
    }

    let published_at = note
        .published_ms
        .and_then(|ms| jiff::Timestamp::from_millisecond(ms).ok())
        .map(Rfc3339::from_timestamp);
    let author = note.author.as_ref().and_then(|a| {
        Some(Author {
            name: a.nickname.clone().filter(|n| !n.is_empty())?,
            platform_id: a.user_id.clone(),
            url: a
                .user_id
                .as_ref()
                .map(|u| format!("{CANONICAL_ORIGIN}/user/profile/{u}")),
        })
    });
    let meta_wanted = wanted.contains(&OutputKind::Meta);
    if meta_wanted {
        let meta = json!({
            "noteId": note.id,
            "url": link::canonical_url(&note.id),
            "type": match note.kind { NoteKind::Normal => "normal", NoteKind::Video => "video" },
            "title": note.title,
            "author": note.author.as_ref().map(|a| json!({"userId": a.user_id, "nickname": a.nickname})),
            "publishedAtMs": note.published_ms,
            "updatedAtMs": note.updated_ms,
            "ipLocation": note.ip_location,
            "tags": note.tags,
            "counts": {
                "liked": note.counts.liked, "collected": note.counts.collected,
                "comments": note.counts.comments, "shared": note.counts.shared
            },
            "images": image_records,
            "video": video_record,
            "failed": failed,
            "visitorCookieUsed": cookie_used,
        });
        fs::write(out.join("meta.json"), pretty(&meta)?)?;
        if note.title.is_none()
            && author.is_none()
            && published_at.is_none()
            && note.tags.is_empty()
        {
            missing.push(OutputKind::Meta);
            reasons.push("页面里没有标题、作者、时间和标签".into());
        }
    }

    missing.sort();
    missing.dedup();
    let completeness = if missing.is_empty() {
        Completeness::Full
    } else {
        Completeness::Partial
    };
    Ok(FetchOutput {
        identity: Some(link::identity(&link.id)),
        kind: match note.kind {
            NoteKind::Normal => ItemKind::Post,
            NoteKind::Video => ItemKind::Video,
        },
        title: meta_wanted
            .then(|| note.title.clone())
            .flatten()
            .filter(|t| !t.is_empty()),
        author: author.filter(|_| meta_wanted),
        published_at: published_at.filter(|_| meta_wanted),
        text_format: text.as_ref().map(|_| TextFormat::Plain),
        text,
        html: None,
        images,
        media,
        platform_tags: if meta_wanted {
            note.tags.clone()
        } else {
            Vec::new()
        },
        comments: None,
        completeness,
        missing,
        missing_reason: (!reasons.is_empty()).then(|| reasons.join("；")),
        evidence: Evidence {
            fetched_at,
            access: Access::PublicWeb,
            site: None,
            upstream: None,
            status: Some(page.status),
        },
        credentials_update: None,
    })
}

fn pretty(value: &serde_json::Value) -> Result<Vec<u8>, Error> {
    serde_json::to_vec_pretty(value).map_err(|e| Error::Internal(format!("encode JSON: {e}")))
}

fn video_media(path: RelPath, mime: String, stream: &Stream) -> Media {
    Media {
        path,
        kind: MediaKind::Video,
        mime: Some(mime),
        lang: None,
        duration_sec: stream.duration_ms.map(|ms| ms as f64 / 1000.0),
    }
}

fn rel(path: &str) -> RelPath {
    RelPath::try_from(path).expect("plugin-made relative path")
}

/// 下载一张图。返回的错误是写进 `meta.json` 的原因；写盘失败另算，见调用处。
fn download_image(
    http: &Http<'_>,
    raw: &str,
    index: usize,
    out: &Path,
    total: &mut u64,
) -> Result<RelPath, String> {
    let url = http
        .site()
        .media_url(raw)
        .ok_or_else(|| "图片地址不在插件的主机名单里，没有去取".to_owned())?;
    let mut response = http
        .get(&url, Purpose::Image, IMAGE_TIMEOUT)
        .map_err(|e| e.to_string())?;
    if !(200..300).contains(&response.status()) {
        return Err(format!("HTTP {}", response.status()));
    }
    let ext = http::header(&response, "content-type")
        .and_then(image_extension)
        .ok_or_else(|| "不是图片".to_owned())?;
    let bytes = http
        .read_capped(&mut response, IMAGE_BYTES)
        .map_err(|e| e.to_string())?;
    *total += bytes.len() as u64;
    if *total > IMAGES_TOTAL_BYTES {
        return Err("图片加起来超过上限".into());
    }
    let name = format!("images/{index:03}.{ext}");
    fs::create_dir_all(out.join("images")).map_err(|e| e.to_string())?;
    fs::write(out.join(&name), &bytes).map_err(|e| e.to_string())?;
    Ok(rel(&name))
}

fn image_extension(content_type: &str) -> Option<&'static str> {
    let mime = content_type.split(';').next()?.trim().to_ascii_lowercase();
    Some(match mime.as_str() {
        "image/jpeg" | "image/jpg" => "jpg",
        "image/png" => "png",
        "image/webp" => "webp",
        "image/avif" => "avif",
        "image/heic" => "heic",
        "image/gif" => "gif",
        _ => return None,
    })
}

/// 下载一路视频：主地址不行试备用地址；边下边写，超过上限就停，失败时删掉写了一半的文件。
fn download_stream(
    http: &Http<'_>,
    stream: &Stream,
    name: &str,
    out: &Path,
    limit: u64,
) -> Result<(RelPath, String), String> {
    let mut last = String::from("没有可用的地址");
    let candidates =
        std::iter::once(&stream.url).chain(stream.backup.iter().filter(|b| **b != stream.url));
    for raw in candidates {
        let Some(url) = http.site().media_url(raw) else {
            last = "视频地址不在插件的主机名单里，没有去取".into();
            continue;
        };
        match save_stream(http, &url, name, out, limit) {
            Ok(mime) => return Ok((rel(name), mime)),
            Err(why) => last = why,
        }
    }
    Err(last)
}

fn save_stream(
    http: &Http<'_>,
    url: &Url,
    name: &str,
    out: &Path,
    limit: u64,
) -> Result<String, String> {
    let response = http
        .get(url, Purpose::Video, VIDEO_TIMEOUT)
        .map_err(|e| e.to_string())?;
    if !(200..300).contains(&response.status()) {
        return Err(format!("HTTP {}", response.status()));
    }
    let mime = http::header(&response, "content-type")
        .unwrap_or("video/mp4")
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if !(mime.starts_with("video/") || mime == "application/octet-stream") {
        return Err(format!("不是视频（{mime}）"));
    }
    let total = http::header(&response, "content-length").and_then(|v| v.parse::<u64>().ok());
    let path = out.join(name);
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let cap = http.byte_cap(limit);
    let mut done = 0u64;
    let result = (|| {
        let mut reader = response.into_reader(cap);
        let mut file = File::create(&path).map_err(|e| e.to_string())?;
        let mut buf = vec![0u8; 256 << 10];
        let mut reported = 0u64;
        loop {
            let n = reader.read(&mut buf).map_err(|e| {
                if e.kind() == std::io::ErrorKind::FileTooLarge && cap < limit {
                    http::BYTES_SPENT.to_owned()
                } else {
                    e.to_string()
                }
            })?;
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
            done += n as u64;
            if done - reported >= PROGRESS_STEP {
                reported = done;
                progress(done, total);
            }
        }
        file.flush().map_err(|e| e.to_string())?;
        if done == 0 {
            return Err("视频是空的".to_owned());
        }
        Ok(())
    })();
    // 超了总量时最后那一下读到的没算进 done，直接算用完
    let over = matches!(&result, Err(why) if why == http::BYTES_SPENT);
    http.spend(if over { cap } else { done });
    match result {
        Ok(()) => Ok(if mime == "application/octet-stream" {
            "video/mp4".into()
        } else {
            mime
        }),
        Err(why) => {
            // 写了一半的文件不留在输出里；删不掉也只是多一个没被引用的文件
            if let Err(e) = fs::remove_file(&path) {
                eprintln!("could not remove partial {name}: {e}");
            }
            Err(why)
        }
    }
}

fn progress(done: u64, total: Option<u64>) {
    let mb = |b: u64| (b as f64 / (1u64 << 20) as f64 * 10.0).round() / 10.0;
    let event = PluginEvent::Progress {
        done: Some(mb(done)),
        total: total.map(mb),
        unit: Some("MB".into()),
        message: Some("下载视频".into()),
    };
    eprintln!("{}", event.stderr_line());
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader};
    use std::net::{IpAddr, TcpListener};

    use usuyo_plugin_kit::http::Policy;

    use super::*;

    const CLIP: usize = 400 << 10;

    /// 本机服务：`/media/*` 回 400 KiB 的假视频，`/img/*` 回一张小 jpeg。
    fn serve_media() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap_or(0);
                let path = line.split_whitespace().nth(1).unwrap_or("/").to_owned();
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).unwrap_or(0) == 0 || header == "\r\n" {
                        break;
                    }
                }
                let (ctype, body) = if path.starts_with("/img/") {
                    ("image/jpeg", b"\xff\xd8\xff\xe0fake".to_vec())
                } else {
                    let mut v = b"\0\0\0\x18ftypmp42".to_vec();
                    v.resize(CLIP, 0);
                    ("video/mp4", v)
                };
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream.write_all(head.as_bytes()).ok();
                stream.write_all(&body).ok();
            }
        });
        base
    }

    fn local_policy() -> Policy {
        Policy {
            lookup: |_, _| Err(std::io::Error::other("no DNS in tests")),
            public: |ip| ip == IpAddr::from([127, 0, 0, 1]),
            cookies_over_http: true,
            ..Policy::strict()
        }
    }

    fn stream(url: String) -> Stream {
        Stream {
            url,
            backup: Vec::new(),
            codec: "h264".into(),
            width: None,
            height: None,
            duration_ms: None,
        }
    }

    /// 图片和几路视频共用整次调用的字节总量：用完以后不再下载，也不再发请求。
    #[test]
    fn downloads_share_the_call_byte_budget() {
        let base = serve_media();
        let site = Site::loopback(&base, &base, local_policy()).unwrap();
        let http = Http::new(&site, Duration::from_secs(10)).with_bytes(1 << 20);
        let out = tempfile::tempdir().unwrap();
        let results: Vec<_> = (1..=3)
            .map(|i| {
                let name = format!("media/live-{i:03}.mp4");
                let s = stream(format!("{base}/media/{i}.mp4"));
                download_stream(&http, &s, &name, out.path(), VIDEO_BYTES)
            })
            .collect();
        assert!(results[0].is_ok() && results[1].is_ok(), "{results:?}");
        let why = results[2].as_ref().unwrap_err();
        assert!(why.contains("byte budget"), "{why}");
        let image = download_image(&http, &format!("{base}/img/a.jpg"), 1, out.path(), &mut 0);
        assert!(image.unwrap_err().contains("byte budget"));

        let written: u64 = fs::read_dir(out.path().join("media"))
            .unwrap()
            .map(|e| e.unwrap().metadata().unwrap().len())
            .sum();
        assert_eq!(written, 2 * CLIP as u64);
    }

    const ID: &str = "671234567890abcdef012345";

    fn redirect(path: &str) -> Result<(), Error> {
        let site = Site::xiaohongshu();
        refuse_redirect(
            &site,
            &Url::parse(&format!("https://www.xiaohongshu.com{path}")).unwrap(),
            ID,
        )
    }

    #[test]
    fn redirect_mapping() {
        assert!(redirect(&format!("/explore/{ID}?xsec_token=t")).is_ok());
        assert!(matches!(
            redirect("/explore/aaaaaaaaaaaaaaaaaaaaaaaa"),
            Err(Error::UpstreamChanged(_))
        ));
        assert!(matches!(redirect("/"), Err(Error::LoginRequired { .. })));
        assert!(matches!(
            redirect("/explore"),
            Err(Error::LoginRequired { .. })
        ));
        assert!(matches!(
            redirect("/login?redirectPath=x"),
            Err(Error::LoginRequired { .. })
        ));
        assert!(matches!(
            redirect("/website-login/error"),
            Err(Error::LoginRequired { .. })
        ));
        assert!(matches!(
            redirect("/website-login/captcha?verifyType=1"),
            Err(Error::Blocked(_))
        ));
        assert!(matches!(
            redirect("/404/sec_ab12?redirectPath=%2Fexplore&error_code=300031"),
            Err(Error::LinkExpired(_))
        ));
        assert!(matches!(
            redirect("/404/sec_ab12?error_code=300017"),
            Err(Error::Blocked(_))
        ));
        assert!(matches!(redirect("/404/sec_ab12"), Err(Error::Blocked(_))));
        assert!(matches!(
            redirect("/404?source=note"),
            Err(Error::NotFound(_))
        ));
        assert!(matches!(
            redirect("/somewhere"),
            Err(Error::UpstreamChanged(_))
        ));
        let site = Site::xiaohongshu();
        assert!(matches!(
            refuse_redirect(&site, &Url::parse("https://example.com/").unwrap(), ID),
            Err(Error::UpstreamChanged(_))
        ));
    }

    #[test]
    fn risk_json_mapping() {
        assert!(matches!(
            risk_json(r#"{"code":300013,"msg":"访问频次异常"}"#),
            Error::Blocked(_)
        ));
        assert!(matches!(
            risk_json(r#"{"code":-1,"success":false}"#),
            Error::Blocked(_)
        ));
        assert!(matches!(
            risk_json(r#"{"code":-510}"#),
            Error::RateLimited { .. }
        ));
        assert!(matches!(
            risk_json(r#"{"code":-101,"msg":"无登录信息"}"#),
            Error::LoginRequired { .. }
        ));
        assert!(matches!(
            risk_json(r#"{"code":300031}"#),
            Error::LinkExpired(_)
        ));
        assert!(matches!(
            risk_json(r#"{"code":-2,"msg":"笔记不存在"}"#),
            Error::NotFound(_)
        ));
        assert!(matches!(
            risk_json(r#"{"code":0,"data":{}}"#),
            Error::UpstreamChanged(_)
        ));
        assert!(matches!(risk_json("{oops"), Error::UpstreamChanged(_)));
    }
}
