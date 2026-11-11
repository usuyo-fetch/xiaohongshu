# usuyo 小红书获取插件

给 [usuyo](https://github.com/usuyo/usuyo) 用的获取插件：把你在小红书上已经收藏、赞过的笔记，按你交来的链接取下正文、图片和视频，存到你自己的电脑上，做个人备份和离线阅读。

插件是一个独立的命令行程序，按 usuyo 插件协议和宿主对话；你一般不会直接运行它。

<a id="它做什么"></a>
## 它做什么

- **认链接**：`www.xiaohongshu.com` 上的四种笔记链接（`explore/<id>`、`search_result/<id>`、`discovery/item/<id>`、`user/profile/<作者>/<id>`），和手机分享出来的 `xhslink.com`、`xhslink.cn` 短链。笔记 id 是 24 位十六进制。短链只请求短链主机、只看它跳去哪里，不带任何 cookie，一看到笔记链接就停，不去打开笔记页。
- **取一篇笔记**：用普通桌面浏览器的请求头打开这条链接，先不带 cookie；请求里只带 `xsec_token` 和 `xsec_source` 两个参数，手机分享链接里 `shareRedId` 一类指向分享人的参数不发出去（usuyo 记下的原链接不变）；从页面里的状态对象读出标题、正文、作者、发布时间、标签、互动数，下载页面给出的图片和视频。
- **如实说缺了什么**：图文笔记没有视频、某张图没下载下来、页面没给视频地址，都写进 `missing` 和 `missingReason`，不假装拿全了。
- **输出目录**：

```
<out>/
  note.json          页面里这篇笔记的原样对象（以后解析改进了可以重算）
  meta.json          标题、作者、时间、互动数、每张图的原地址、没下载下来的和原因
  images/001.webp    图片，扩展名按服务器给的类型
  media/video.mp4    视频笔记的视频
  media/live-002.mp4 实况图的动态部分（如果有）
```

<a id="它不做什么"></a>
## 它不做什么

- 不登录，不读你浏览器里的 cookie，不用你的账号。
- 不复现平台的请求签名，不调要签名的接口；评论第一版不取。
- 不列清单：插件没有 `list`。你存过哪些笔记，由下面的油猴脚本在你自己的浏览器里记下，再交给 usuyo。
- 不自己拼图片、视频地址，只下载页面本身给出的地址；地址不在 `*.xhscdn.com` 上的不去取，写进原因。
- 不循环重试：被送回首页或登录页时，有访客身份就带上再试**一次**，还不行就报 `login-required`；其余失败都只请求一次。
- 不取你没要的东西：宿主只要文字时，图片和视频都不下载。

<a id="访客身份可选"></a>
## 访客身份（可选）

2026 年 10 月起，有的笔记链接不带 cookie 会被送回首页，带一份**没登录的访客 cookie** 才取得到（XHS-Downloader #495、#499）。这份 cookie 不是你的账号。

清单在 `visitors` 里声明它（见 usuyo 插件协议的「访客身份」一节）。在 usuyo 的「获取插件」页，小红书下面点「取访客身份」：usuyo 开一个不登录的窗口打开首页，存下页面自己生成的 cookie，和你以后登录的账号分开存。取笔记时宿主经 `USUYO_VISITOR` 交给插件。

插件只把它发给 `www.xiaohongshu.com` 的笔记页、只走 https，不发给短链和图片主机，不写进日志和输出目录。它过期时插件会报 `login-required`，提示你换一份。

注意：开发机和你平时刷小红书的手机、电脑大概率是同一个出口 IP，平台可能把这台电脑的访问和你的账号联系起来（推断）。所以节奏要慢。

<a id="节奏"></a>
## 节奏

清单里给宿主的建议是：`batch: true`、`interval: 15s`、`daily: 100`。也就是：

- 只取你刚交来的那一批，不自己去找新的；
- 同一平台两次取之间至少隔 15 秒；
- 一天最多 100 次批量调用（你手动收下的单条不算在里面）。

`xsec_token` 只在几个小时内有效（有项目实测几小时后链接就打不开），所以一批清单交过来，最好当天取完。第一次用，先交几条试试，过一两天看账号有没有提示，再交几十条。

<a id="油猴脚本"></a>
## 油猴脚本：把你的收藏和赞过交给 usuyo

`userscript/usuyo-xhs-saved.user.js` 改写自 [wkwunju/xhs-vault](https://github.com/wkwunju/xhs-vault)（MIT，许可声明在脚本开头和 `userscript/LICENSE.xhs-vault`）。它在你**日常用的浏览器**里跑，只看小红书页面自己请求回来的列表：

- 不发任何额外请求，不自动滚动，不读、不导出 cookie；
- 只记你**自己**的「收藏」「赞过」：请求里的 `user_id` 要等于页面里登录的那个人，别人的公开收藏不记；
- 记下的是笔记 id、`xsec_token` 和标题，存在脚本管理器里（不在页面的 localStorage 里），12 小时后自动作废；
- 点「交给 usuyo」时只连本机的 `http://127.0.0.1:47615`（脚本头里 `@connect 127.0.0.1`，经脚本管理器的 `GM_xmlhttpRequest`，5 秒超时），不往别处发，也不向小红书发任何东西。先握手：发一个新的随机数和令牌的 id，对面要用整个令牌签回 HMAC-SHA256 证明（和 usuyo 的 `crates/usuyo-api/src/handshake.rs` 同一套），脚本用自己存着的令牌核对；核对不过就什么都不发，令牌只存在脚本管理器里，从浏览器自己的对话框或脚本管理器的菜单填，页面上没有输入框。脚本和页面脚本跑在同一个环境里时（例如 Violentmonkey 的页面模式），页面能改掉 `String`、`RegExp`、`Function` 的原型方法；脚本碰令牌的每一步只用下标、字符串拼接和一开始就拿住的函数，测试里把这些方法换成会记账的间谍也没看到令牌。这只在脚本先于页面脚本跑起来时有用，脚本管理器注入得晚就挡不住；这个令牌只能交链接，页面的来源也进不了 usuyo。核对过了才带上令牌，收藏一批（`favorite`）、赞过一批（`like`）交给 `POST /v1/batches`，每条是 `https://www.xiaohongshu.com/explore/<id>?xsec_token=…` 加标题，超过 12 小时的不交。交出去的从脚本里去掉。

用法：

1. 装一个脚本管理器：Chrome、Edge、Firefox 上用 Violentmonkey 或 Tampermonkey；Safari 上用 Userscripts，并在 Safari 的设置里让它能访问所有网站（不然它连本机端口时会被 Safari 的跨域检查挡下，见下）。新建脚本，把 `usuyo-xhs-saved.user.js` 的内容贴进去保存。Tampermonkey 第一次连本机时可能问一次要不要放行 `127.0.0.1`，选允许。
2. 在 usuyo 的导入页，「交进来的清单」里点「发一个令牌」，复制它。这个令牌只能交链接，读不了你的资料库。
3. 打开你自己的主页，切到「收藏」或「赞过」。右下角会出现一小块：`usuyo · 收藏 0 · 赞过 0`。**自己往下翻**，像平时一样看，翻到的会记下来；页面说到底了会显示「已到底」。如果出现「小红书回了 code …，先别往下翻了」，就停下来，过一阵再继续。
4. 点「交给 usuyo」。第一次会弹出浏览器的对话框让你贴令牌；对话框没弹出来时，从脚本管理器的菜单里点「填 usuyo 令牌」。之后角落里只显示令牌 id 的前 6 位，点「忘掉令牌」或菜单里的「忘掉 usuyo 令牌」就删掉。交完会写「交给 usuyo 了：收藏 N 条、赞过 M 条」，回 usuyo 的导入页确认，确认了才按节奏去取。

交不成时角落里照实说：usuyo 没有开着；端口上的程序没能证明自己是 usuyo（也可能令牌在 usuyo 里撤掉了），令牌没有发出去；usuyo 不认这个令牌；usuyo 没让这个脚本管理器的请求进来。出了这几种就停下，没交的条目留着。小红书的「专辑」接口看不出是不是你自己的，这一版不记。

<a id="脚本管理器带什么-origin"></a>
### 脚本管理器带什么 Origin

usuyo 的本机接口只放行不带 `Origin` 的请求和浏览器扩展的来源（`chrome-extension://`、`moz-extension://`、`safari-web-extension://`）。各脚本管理器的 `GM_xmlhttpRequest`（2026-11 查的源码和 issue，没在真浏览器里跑过）：

- Violentmonkey：不带 `Origin`（Chrome 上用 declarativeNetRequest 去掉，Firefox 上在 webRequest 里去掉自己的来源）。
- Tampermonkey：Firefox 上带 `moz-extension://…`（Tampermonkey issue #2747）；Chrome 上推断带 `chrome-extension://…`。
- Userscripts（Safari）：带 `safari-web-extension://…`（userscripts issue #897）；没有网站访问权限时 Safari 先发 CORS 预检，usuyo 现在不答这种预检，请求会失败。

测试里用这几种 `Origin` 对着一次性的 usuyo 都交成了，页面自己的来源（`https://www.xiaohongshu.com`）在握手那一步就被挡下。

<a id="脚本的测试"></a>
### 脚本的测试

```
USUYO_BIN=<编好的 usuyo> node --test userscript/test/userscript.test.mjs
```

测试只连本机：一个假的小红书站点答两个列表接口，脚本的 `GM_*`、`unsafeWindow`、`document` 都是假的；端口那一行（`const PORT = 47615;`）在载入时换掉。给了 `USUYO_BIN` 时另起一个一次性的 usuyo（端口 47702，`USUYO_TEST_PORT` 可改，家目录是新的临时目录，测完停掉并送进废纸篓），没给就跳过对着 usuyo 的几条。

<a id="只有你能核对的"></a>
## 只有你能核对的

插件和脚本都只对着手写的样例测过，没有对小红书发过请求。这些要你用自己的浏览器和账号看：

- 在你的收藏页、赞过页往下翻，计数会不会涨；赞过页的接口是不是 `note/like/page`；翻到底时会不会显示「已到底」。
- 脚本认不认得出你的账号（认不出时角落会写「没认出登录的账号」）。
- 你用的脚本管理器交不交得过去；交过去的两批在 usuyo 的导入页里条数对不对。
- 交过去的链接当天能不能取到；不带访客 cookie 和带访客 cookie 各有多少条成功。
- 手机分享的短链展开后，去掉哪些参数仍然取得到。
- 取了几十条以后，账号有没有提示或限制。

<a id="开发"></a>
## 开发

```
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```

测试只连本机：一个假笔记页服务、一个假短链服务，页面是 `tests/fixtures/` 里照参考项目记下的形状手写的。正式运行只认 `Site::xiaohongshu()` 的主机名单；测试用的 `Site::loopback()` 只接受回环地址。页面改版时，只改 `src/state.rs`。

依赖 usuyo 仓库里的 `usuyo-plugin-kit` crate（协议类型和发请求的客户端：每个请求只连名单里那一类主机、只连公网地址），按路径引用，要和 usuyo 仓库放在同一个上级目录下。

许可证：MIT。
