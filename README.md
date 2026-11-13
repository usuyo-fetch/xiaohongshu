# usuyo-fetch/xiaohongshu

小红书的获取插件，供 [usuyo](https://github.com/usuyo/usuyo) 调用：按笔记链接取回正文、图片和视频。

| 方法 | 清单里的 id | 取得 | 账号 | 层级 | 现状 |
|---|---|---|---|---|---|
| 公开笔记页 | `note-page` | 单条笔记：正文、作者、发布时间、互动数、标签、图片、视频；认 `xhslink.com`、`xhslink.cn` 短链 | 访客身份 | `public-web` | 已实现 |
| 浏览器脚本 | `userscript` | 收藏、赞过的链接 | 只在使用者的浏览器内 | `browser-session` | 已实现 |
| XHS-Downloader | `xhs-downloader` | 同 `note-page`，问本机运行的 [XHS-Downloader](https://github.com/JoeanAmier/XHS-Downloader)（GPL-3.0，独立进程） | 不使用 | `public-web` | 已实现，只对假服务测过 |
| MediaCrawler 的结果文件 | `mediacrawler` | 读 MediaCrawler 的 json、jsonl 或 zip，列笔记链接 | 不使用（只读文件） | `private-api`，默认关 | 已实现，只对手写文件测过 |
| iPhone 镜像 | `iphone` | 收藏、赞过 | 只在手机的 App 内 | `vision-ui` | 未实现 |
| 安卓手机 | `android` | 收藏、赞过 | 只在手机的 App 内 | `vision-ui` | 未实现 |
| RSSHub | `rsshub` | 按创作者 | 不使用 | `public-web` | 未实现 |

测试：`cargo test`，只连本机假服务；页面改版时改 `src/state.rs`。依赖同级目录下 usuyo 仓库的 `usuyo-plugin-kit`。许可证：MIT。
