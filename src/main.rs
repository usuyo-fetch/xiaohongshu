//! 小红书获取插件的入口。子命令照协议 §3.2：`canonicalize`、`fetch`、`health`；按方法分派（§2.3.1）：
//! `note-page`、`xhs-downloader`（问你本机的 XHS-Downloader）、`mediacrawler`（读文件）在这里跑，
//! 浏览器脚本在你的浏览器里交链接，别的方法还没做。

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::ExitCode;

use serde::Serialize;
use usuyo_fetch_xiaohongshu::fetch::{self, Request};
use usuyo_fetch_xiaohongshu::{
    Error, Lang, Site, link, mediacrawler, visitor_cookie, xhsdownloader,
};
use usuyo_plugin_kit::fetch::{Call, USAGE};
use usuyo_plugin_kit::protocol::{CallFailure, Envelope, ErrorCode, FetchHealth, OutputKind};

/// 清单里在插件进程里跑的那一种方法。
const NOTE_PAGE: &str = "note-page";
/// 读 MediaCrawler 结果文件的那一种方法：只列，不联网。
const MEDIACRAWLER: &str = "mediacrawler";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let lang = Lang::from_locale(std::env::var("USUYO_LOCALE").ok().as_deref());
    let site = Site::xiaohongshu();
    let code = match Call::parse(&args) {
        Some(Call::Canonicalize { url }) => {
            print(link::canonicalize(&site, &url).map_err(Into::into), lang)
        }
        Some(Call::Fetch {
            input,
            out,
            method,
            want,
        }) if method == NOTE_PAGE => print(
            run_fetch(&site, &input, out, want.map(|w| w.into_iter().collect())),
            lang,
        ),
        Some(Call::Fetch {
            input,
            out,
            method,
            want,
        }) if method == xhsdownloader::METHOD => print(
            run_xhs_downloader(&site, &input, out, want.map(|w| w.into_iter().collect())),
            lang,
        ),
        Some(Call::List {
            method,
            source,
            cursor,
            ..
        }) if method == MEDIACRAWLER => {
            write_envelope(&match mediacrawler::list_call(&source, cursor.as_deref()) {
                Ok(page) => Envelope::Ok(page),
                Err(failure) => Envelope::Err(failure),
            })
        }
        Some(Call::Health { probe }) => print(health(probe), lang),
        Some(other) => print::<()>(
            Err(Error::InvalidArgs(format!(
                "this call does not apply to method {:?}",
                other.method()
            ))
            .into()),
            lang,
        ),
        None => usage(),
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
    write_envelope(&envelope)
}

/// 把信封写到 stdout，返回退出码。
fn write_envelope<T: Serialize>(envelope: &Envelope<T>) -> u8 {
    match serde_json::to_string(envelope) {
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

fn run_fetch(
    site: &Site,
    input: &str,
    out: PathBuf,
    want: Option<BTreeSet<OutputKind>>,
) -> anyhow::Result<usuyo_plugin_kit::protocol::FetchOutput> {
    check_out(&out)?;
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

fn run_xhs_downloader(
    site: &Site,
    input: &str,
    out: PathBuf,
    want: Option<BTreeSet<OutputKind>>,
) -> anyhow::Result<usuyo_plugin_kit::protocol::FetchOutput> {
    check_out(&out)?;
    let service = xhsdownloader::Service::from_env()?;
    Ok(xhsdownloader::fetch(
        site,
        &service,
        xhsdownloader::Request {
            input,
            out: &out,
            want,
        },
    )?)
}

fn check_out(out: &std::path::Path) -> Result<(), Error> {
    if !out.is_absolute() || !out.is_dir() {
        return Err(Error::InvalidArgs(format!(
            "--out `{}` is not an existing absolute directory",
            out.display()
        )));
    }
    Ok(())
}

/// 不向小红书发请求（协议 §3.10）；访客身份由宿主看着。`xhs-downloader` 的检查只连本机的那个服务。
fn health(probe: bool) -> anyhow::Result<FetchHealth> {
    let mut h = xhsdownloader::health(FetchHealth::new(env!("CARGO_PKG_VERSION")));
    if probe {
        h = h.check(
            "probe",
            true,
            Some(
                "没有发请求：笔记链接里的 xsec_token 几小时就失效，插件没有一篇长期可用的笔记拿来试"
                    .into(),
            ),
            &[],
        );
    }
    Ok(h)
}
