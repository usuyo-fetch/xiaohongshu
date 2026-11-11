//! 小红书获取插件的入口。子命令照协议 §3.2：`canonicalize`、`fetch`、`health`。

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::ExitCode;

use serde::Serialize;
use usuyo_fetch_xiaohongshu::fetch::{self, Request};
use usuyo_fetch_xiaohongshu::{Error, Lang, Site, link, visitor_cookie};
use usuyo_plugin_kit::protocol::{CallFailure, Envelope, ErrorCode, OutputKind};

const USAGE: &str = "usage: usuyo-fetch-xiaohongshu canonicalize <url> | fetch <url> --out <dir> [--want a,b] | health [--probe]";

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
) -> anyhow::Result<usuyo_plugin_kit::protocol::FetchOutput> {
    if !out.is_absolute() || !out.is_dir() {
        return Err(Error::InvalidArgs(format!(
            "--out `{}` is not an existing absolute directory",
            out.display()
        ))
        .into());
    }
    let cookie = visitor_cookie(site)?;
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

/// 不发请求，也没有本机设置要看（协议 §3.10）；访客身份由宿主看着。
fn health(probe: bool) -> anyhow::Result<Health> {
    let mut checks = Vec::new();
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
