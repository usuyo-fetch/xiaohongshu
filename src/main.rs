//! 小红书获取插件的入口。子命令照协议 §3.2：`canonicalize`、`fetch`、`health`。

use std::collections::BTreeSet;
use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context;
use serde::Serialize;
use usuyo_fetch_xiaohongshu::fetch::{self, Request};
use usuyo_fetch_xiaohongshu::{Error, Lang, Site, VISITOR_COOKIE_KEY, VisitorCookie, link};
use usuyo_plugin::protocol::{CallFailure, Envelope, ErrorCode, OutputKind};

const USAGE: &str = "usage: usuyo-fetch-xiaohongshu canonicalize <url> | fetch <url> --out <dir> [--want a,b] | health [--probe]";
/// 密钥文件只有一个小 JSON，超过这个大小就不是宿主写的。
const SECRETS_BYTES: u64 = 64 << 10;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let lang = Lang::from_locale(std::env::var("USUYO_LOCALE").ok().as_deref());
    let site = Site::xiaohongshu();
    let code = match args.first().map(String::as_str) {
        Some("canonicalize") if args.len() == 2 => print(
            link::canonicalize(&site, &args[1]).map_err(Into::into),
            lang,
        ),
        Some("fetch") => match parse_fetch(&args[1..]) {
            Some((input, out, want)) => print(run_fetch(&site, &input, out, want), lang),
            None => usage(),
        },
        Some("health") if args.len() <= 2 && args.get(1).is_none_or(|a| a == "--probe") => {
            print(health(args.len() == 2), lang)
        }
        _ => usage(),
    };
    ExitCode::from(code)
}

fn usage() -> u8 {
    eprintln!("{USAGE}");
    2
}

/// stdout 上只写一个信封，返回退出码。
fn print<T: Serialize>(result: anyhow::Result<T>, lang: Lang) -> u8 {
    let envelope = match result {
        Ok(body) => Envelope::Ok(body),
        Err(e) => match e.downcast::<Error>() {
            Ok(error) => Envelope::Err(error.to_failure(lang)),
            Err(other) => {
                Envelope::Err(CallFailure::new(ErrorCode::Internal, format!("{other:#}")))
            }
        },
    };
    match serde_json::to_string(&envelope) {
        Ok(json) => {
            println!("{json}");
            if envelope.exit_code() == 0 { 0 } else { 1 }
        }
        Err(e) => {
            eprintln!("cannot write the envelope: {e}");
            1
        }
    }
}

fn parse_fetch(args: &[String]) -> Option<(String, PathBuf, Option<BTreeSet<OutputKind>>)> {
    let (input, rest) = args.split_first()?;
    let mut out = None;
    let mut want = None;
    let mut it = rest.iter();
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--out" => out = Some(PathBuf::from(it.next()?)),
            "--want" => {
                want = Some(
                    it.next()?
                        .split(',')
                        .map(OutputKind::parse)
                        .collect::<Option<BTreeSet<_>>>()?,
                );
            }
            _ => return None,
        }
    }
    Some((input.clone(), out?, want))
}

fn run_fetch(
    site: &Site,
    input: &str,
    out: PathBuf,
    want: Option<BTreeSet<OutputKind>>,
) -> anyhow::Result<usuyo_plugin::protocol::FetchOutput> {
    if !out.is_absolute() || !out.is_dir() {
        return Err(Error::InvalidArgs(format!(
            "--out `{}` is not an existing absolute directory",
            out.display()
        ))
        .into());
    }
    let cookie = visitor_cookie()?;
    Ok(fetch::fetch(
        site,
        Request {
            input,
            out: &out,
            want,
            visitor_cookie: cookie.as_ref(),
        },
    )?)
}

/// 设置里的访客 cookie，经 `USUYO_SECRETS` 传入（协议 §2.11、§3.2）。没填时是 `None`。
fn visitor_cookie() -> anyhow::Result<Option<VisitorCookie>> {
    let Some(path) = std::env::var_os("USUYO_SECRETS") else {
        return Ok(None);
    };
    let mut text = String::new();
    std::fs::File::open(&path)
        .context("open USUYO_SECRETS")?
        .take(SECRETS_BYTES)
        .read_to_string(&mut text)
        .context("read USUYO_SECRETS")?;
    let secrets: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&text).context("USUYO_SECRETS is not a JSON object")?;
    match secrets.get(VISITOR_COOKIE_KEY) {
        None => Ok(None),
        Some(serde_json::Value::String(raw)) if raw.trim().is_empty() => Ok(None),
        Some(serde_json::Value::String(raw)) => Ok(Some(VisitorCookie::parse(raw)?)),
        Some(_) => Err(Error::BadCookie("the setting is not a string".into()).into()),
    }
}

#[derive(Serialize)]
struct Health {
    ready: bool,
    protocol: u32,
    version: &'static str,
    upstream: Vec<serde_json::Value>,
    login: Vec<serde_json::Value>,
    checks: Vec<Check>,
}

#[derive(Serialize)]
struct Check {
    name: &'static str,
    ok: bool,
    message: String,
}

/// 不发请求，只看本机的设置（协议 §3.10）。
fn health(probe: bool) -> anyhow::Result<Health> {
    let cookie = match visitor_cookie() {
        Ok(Some(_)) => Check {
            name: VISITOR_COOKIE_KEY,
            ok: true,
            message: "填了访客 cookie".into(),
        },
        Ok(None) => Check {
            name: VISITOR_COOKIE_KEY,
            ok: true,
            message: "没填访客 cookie（可选）：有的笔记不带它取不到".into(),
        },
        Err(e) => Check {
            name: VISITOR_COOKIE_KEY,
            ok: false,
            message: format!("访客 cookie 用不了：{e:#}"),
        },
    };
    let mut checks = vec![cookie];
    if probe {
        checks.push(Check {
            name: "probe",
            ok: true,
            message:
                "没有发请求：笔记链接里的 xsec_token 几小时就失效，插件没有一篇长期可用的笔记拿来试"
                    .into(),
        });
    }
    Ok(Health {
        ready: checks.iter().all(|c| c.ok),
        protocol: 1,
        version: env!("CARGO_PKG_VERSION"),
        upstream: Vec::new(),
        login: Vec::new(),
        checks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetch_args() {
        let args: Vec<String> = [
            "https://a.b/",
            "--out",
            "/tmp/o",
            "--want",
            "text,media:video",
        ]
        .map(String::from)
        .to_vec();
        let (_, out, want) = parse_fetch(&args).unwrap();
        assert_eq!(out, PathBuf::from("/tmp/o"));
        assert_eq!(want, Some([OutputKind::Text, OutputKind::Video].into()));
        for bad in [
            &["https://a.b/", "--want", "video"][..],
            &["https://a.b/"][..],
            &["https://a.b/", "--out"][..],
            &["https://a.b/", "--out", "/o", "--extra"][..],
        ] {
            let bad: Vec<String> = bad.iter().map(|s| s.to_string()).collect();
            assert!(parse_fetch(&bad).is_none(), "{bad:?}");
        }
    }
}
