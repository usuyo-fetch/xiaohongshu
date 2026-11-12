//! usuyo 的小红书获取插件：按你存过的笔记链接取公开笔记页，存成个人备份。
//!
//! - [`link`]：认链接、展开短链（`canonicalize`）。
//! - [`fetch`]：取一篇笔记，写进输出目录。
//! - [`mediacrawler`]：读你自己跑 MediaCrawler 留下的帖子文件，列成链接（`list`，不联网）。
//! - [`xhsdownloader`]：问你自己在本机运行的 XHS-Downloader 要一篇笔记（`xhs-downloader` 方法）。
//! - [`state`]：笔记页状态对象的形状，页面改版只改这里。
//! - [`Site`]：能连哪些主机；正式运行只用 [`Site::xiaohongshu`]。

mod error;
pub mod fetch;
mod http;
pub mod link;
pub mod mediacrawler;
mod site;
pub mod state;
pub mod xhsdownloader;

pub use error::{Error, Lang};
pub use http::visitor_cookie;
pub use site::Site;
