# ABRepo-TestEnv

一个 AstroBox NG（**API Level 4**）插件：把 [ABRepo-TestEnv](https://github.com/AstralSightStudios/ABRepo-TestEnv)
这个测试用市场源仓库注册成 AstroBox 社区源（资源页右上角源选择器里显示为 **ABRepo-TestEnv**），
并在插件页里提供一个最小状态页做自检。

源仓库本身与 AstroBox 官方源结构完全一致（`index_v2.csv` + `devices_v2.json` + `resources/<owner>/<name>.json`），
但**宿主不会替插件拉数据**：切到源时它通过 `provider-action` 事件向插件要数据，
所以拉取、解析、分页、搜索、拼下载地址都由本插件实现。

## 做了什么

- `on_load` 里调用 `register_provider(name, ProviderType::Url)` 注册社区源。
- **实现 provider-action 回调**：宿主不会自己去拉 `index_v2.csv`，切到源时它向插件派发
  `provider-action` 事件，插件必须抓到 `index_v2.csv` / `devices_v2.json` 并把结果
  `resolve_provider_action` 回去，源才会进入 Ready。
- 插件页（v4 ui）展示：源信息、注册状态、连通性与 `index_v2.csv` 自检结果
  （HTTP 状态、资源条目数、资源类型分布、设备型号数、免费/付费分布、解析耗时、缺列检查），
  以及宿主最近一次 `refresh` 拉到多少条。
- **AstroBox 账号登录**：用 v4 的 `browser` 拦截 OAuth 回调自己换取 access token，
  用于后续访问 AstroBox 的加密资源接口（详见下面「AstroBox 账号登录」）。

## provider-action 协议

宿主派发的载荷（来自宿主日志 `pluginsystem::provider_action`，`version: 1`）：

```json
{
  "version": 1,
  "provider": "ABRepo-TestEnv",
  "action": "get_page",
  "requestId": "provider-action:ABRepo-TestEnv:get_page:12",
  "params": { "page": 0, "limit": 20, "search": { "filter": null, "sort": "time", "category": null } }
}
```

插件用 `provider_callback::resolve_provider_action(request_id, response)` 回数据，
`response` 就是目标类型的 JSON 文本；下载过程可以用
`provider_callback::report_provider_action_progress(request_id, progress, status)` 报进度。

本插件支持的 action（每个都兼容多种别名，未知 action 会记日志并回 `null`，不让宿主干等）：

| action | params | response |
| --- | --- | --- |
| `refresh` | `{config, configRaw}` | `null`（拉取并缓存索引与设备表） |
| `get_categories` | — | `["hide_paid","hide_force_paid","quick_app","watchface","canopus","res_pack", 设备名…]` |
| `get_index` | `{page, limit, search:{filter,sort,category}}` | `[ManifestItemV2, …]`（宿主实际派发的列表动作，别名 `get_page`） |
| `get_total_items` | — | 条目总数（数字） |
| `get_item_manifest` | `{itemId}` | `ManifestV2`（图片与下载地址已补成绝对 URL；别名含 `get_item`/`item`/`detail`） |
| `download` | `{itemId, device}` | 下载地址字符串 |

字段与宿主 `ManifestV2` / `ManifestItemV2` 对齐：`preview` / `icon` / `cover` 会被拼成
`https://raw.githubusercontent.com/{owner}/{repo}/{commit}/{path}`（路径逐段 percent-encode，
因为源 CSV 里有 `画板 1.png`、`封面.png` 这类带空格和中文的文件名），每个 downloads 项补上
`url` 与 `display_name`（设备名来自 `devices_v2.json`）。

### 图片加载不出来时

图片是由 WebView 直接去 `raw.githubusercontent.com` 取的，插件管不到。排查顺序：

1. 打开插件页 → 「浏览器打开首条封面」，用系统浏览器试同一个 URL。
   - 系统浏览器也打不开 → 设备到 GitHub 的网络问题（国内很常见），不是源的问题。
   - 系统浏览器能打开、App 里不行 → WebView 侧被拦（比如 CSP），换 CDN 也没用。
2. 设备直连 raw 不行时，改 `src/source.rs` 的 `CDN` 常量换镜像：

```rust
pub const CDN: Cdn = Cdn::Raw;        // 默认
pub const CDN: Cdn = Cdn::JsDelivr;   // https://cdn.jsdelivr.net/gh
pub const CDN: Cdn = Cdn::GhFast;     // https://ghfast.top
```

3. 源里确实有坏数据：`index_v2.csv` 最后一行 `smoke.ox.e2e.fallback`
   指向 `ox-alpha-test/astrobox-resource-smoke-test-2`，该仓库在 GitHub 上不存在（404），
   所以那条的封面必然取不到。这是测试源自己埋的冒烟数据。

## 源是怎么交给宿主的

`register-provider` 只有一个字符串参数，插件把源选择器里显示的名字注册进去：

```rust
// src/source.rs
pub const PROVIDER_NAME: &str = "ABRepo-TestEnv";  // 资源页源选择器里显示的名字

pub fn provider_name() -> String {
    PROVIDER_NAME.to_string()
}
```

换源只需要改 `src/source.rs` 顶部的 `SOURCE_OWNER` / `SOURCE_REPO` / `SOURCE_BRANCH` 常量。

> **如果切到这个源时拉不到资源**：不同 AstroBox 版本对 `register-provider` 那个字符串的解释不一样，
> 官方文档的说法是「提供指向 `index_v2.csv` 的 URL」。把 `provider_name()` 换成下面任一写法再试：
>
> ```rust
> INDEX_URL.to_string()                                 // 完整索引 URL
> format!("{SOURCE_OWNER}/{SOURCE_REPO}")               // owner/repo
> ```
>
> 状态页的「注册名」一行会显示实际注册进去的值，方便和源选择器里的条目对照。

## 权限

`manifest.json` 声明了四项权限：

| 权限 | 用途 |
| --- | --- |
| `network` | 拉 `index_v2.csv` 做自检、换取 AstroBox token |
| `register_provider` | 注册社区源（宿主会弹窗询问） |
| `browser` | 登录时打开授权页并拦截 OAuth 回调 |
| `account.profile` | 只读取宿主已登录的账号源（`get_current` 不返回 token） |

首次启用时宿主会申请权限，允许后需要重启应用；重启后在「全部资源」页右上角的源选择器里就能看到该源。
**改完 `permissions` 要重新安装插件并重启应用**，否则宿主按旧清单放行，日志里会出现
`permission 'xxx' not declared by plugin`。

## 构建

```bash
# 需要 rustup target add wasm32-wasip2（stable 即可，不需要 nightly）
python scripts/build_dist.py                          # debug
python scripts/build_dist.py --release --package      # release + 打包 .abp
```

产物在 `dist/`：`.wasm`、`manifest.json`、`icon.png`，以及 `ABRepo-TestEnv.abp`。
`.abp` 可直接在 AstroBox 插件页右上角「+」里安装。

## 代码结构

```text
wit/            AstroBox v4 宿主接口（唯一一次 generate! 的输入）
src
├── lib.rs       # lifecycle + event_v4 guest（导出都是 async fn）+ 唯一一次 generate!
├── account.rs   # AstroBox 账号：OAuth 授权码流程 + token 落盘 + 带 token 发请求
├── provider.rs  # provider-action 协议层：解析载荷、按 action 分发、resolve 回宿主
├── catalog.rs   # 源数据层：目录缓存、分类过滤分页、资源详情、下载地址
├── source.rs    # 源常量、index_v2.csv 解析与统计、CDN 开关、图片 URL 编码
├── http.rs      # 基于 waki(p2 wasi:http) 的最小 GET
├── cache.rs     # index/devices 落盘缓存（TTL + 过期回退）
├── state.rs     # 注册状态、目录缓存、自检结果
├── ui.rs        # v4 ui 最小状态页
└── logger.rs    # tracing 初始化（stdout + logs/app.log）
```

## 为什么 v4 用 wasip2 编译（踩了三次坑才搞清）

manifest 声明 `wasi_version: 3`，但**编译目标仍是 `wasm32-wasip2`**，HTTP 也用 p2 的
`wasi:http`（waki）。这不是妥协，是必须：

1. `psys-plugin-v4` 的 `async func` 是 **WIT 层属性**，与 WASI 版本无关；v4 运行时
   （wasmtime 48）同时装齐 p2 与 p3 接口。文档《WASI Preview 3》一节原话：「宿主同时提供
   p2 接口，因为大多数语言的运行时目前仍按 p2 链接标准库，两边都装齐才不会缺导入」。
2. 只要组件 import 了 **p3** 的 `wasi:http@0.3.0`，task 管理就切到 p3 机制，而
   wit-bindgen 0.57 的 `start_task` 仍按 p2 的 `$root:[context-get-0]` 上下文槽断言，
   于是插件**一启动就 panic**：

```
assertion failed: context_get().is_null()
  at wit-bindgen-0.57.1/src/rt/async_support.rs:501
  in start_task::<...lifecycle::_export_on_load_cabi>   ← on_load 第一行
```

验证方式（两条都要过）：

```bash
# 1. 与本机能跑的 v4 插件对齐：manifest 是 wasi_version 3，但 import 全是 @0.2.x
strings ~/.local/share/moe.astralsight.astrobox/plugins/*/*.wasm | grep -o 'wasi:http[^ ]*'
# 2. 自己产物里不能有任何 p3 残留
strings dist/ab_repo_source_plugin.wasm | grep -c '@0.3.0'   # 必须是 0
```

另外两条相关约束：

- 全仓库只能有**一次** `wit_bindgen::generate!`。重复 generate!（或同时链两份
  wit-bindgen crate）会重复声明 `$root:[context-get-0]`，宿主侧 task 上下文同样对不上。
  所以宿主接口绑定统一从 `wit/` 里的 `psys-world-v4` 自己生成，不再依赖
  `astrobox-ng-wit` crate。
- 网络层是阻塞的（waki 内部 `block_on`）。v4 的导出是 `async func`，宿主允许它们阻塞
  等待，但仍要克制：`on_ui_render` 里绝不碰网络；`index_v2.csv` / `devices_v2.json`
  落盘缓存（TTL 10 分钟），切源、翻页基本不发请求，网络失败回退过期缓存。

## AstroBox 账号登录

插件自己跑一遍标准 OAuth 2.0 授权码流程，拿到 access token 后调 AstroBox 接口。
**不用 `account` 宿主接口拿 token**：v4 的 `account::get_current()` 刻意只返回脱敏资料和绑定状态，
不返回 token / cookie（见 docs/plugin-v4/host-api/identity.md）。

```text
① browser.open（intercept-prefixes = ["https://abox.run/open"]）
   → https://cas.astralsight.space/login/oauth/authorize
       ?response_type=code&client_id=...&scope=openid profile email offline_access
       &state=<随机>&redirect_uri=https://abox.run/open?source=astrobox
② 用户在浏览器里登录 → CAS 跳回 redirect_uri → 命中前缀，导航被取消
③ browser.wait_for_intercept 拿回完整回调 URL → 校验 state → 取 code
④ POST {api_base}/auth/login?code=...  →  {"token":"...","refreshToken":"..."}
⑤ 之后请求带 X-ASTROBOX-TOKEN: <token>，外层信封 {"success","message","data"}
```

几个要点：

- **第 ④ 步无鉴权**：没有 client secret、没有设备指纹，code 本身就是一次性凭据。
  这是插件能独立登录的前提。
- **`state` 必须校验**：用 `RandomState` 的种子（std 内部从系统随机源取键）生成，
  回调 state 不匹配说明这个回调不是我们发起的，直接中止。
- **`ephemeral: true`**：授权页用独立数据区，不污染用户正常浏览的 cookie。
- **token 不进日志**：`tracing` 只记 source 和时间；断开登录即删除 `cache/astrobox_account.json`。
- 宿主已登录时用 `account::get_current()` 的 `source` 沿用同一个账号源，
  省得在另一个 CAS 域重复登录（拿不到就回退到默认源 AstralSight）。
- 账号源有两套端点（`casAstralsight` / `waterFlames`），见 `src/account.rs::SOURCES`。

### 已知限制

`redirect_uri` 只能用 CAS 为该 client_id 注册的 `https://abox.run/open?source=astrobox`，
所以首次登录时 CAS 页面显示的应用名是「AstroBox」，用户看不出是哪个插件在登录。
要独立显示名得在 CAS 侧另注册一个 client_id。

`wait_for_intercept` 期间这个 guest task 是阻塞的（单线程组件），
所以登录前先把「等待登录」渲染出来，浏览器窗口随后盖在应用上方。

## 宿主接口绑定

`wit/` 只放 AstroBox 官方 WIT 的 v4 部分（从 `pluginsystem/wit` 同步）：

```
wit/main.wit                    package astrobox:main → 只保留 psys-world-v4
wit/deps/astrobox-host-v4.wit   宿主接口
wit/deps/astrobox-plugin-v4.wit 插件导出
```

## 排错

| 现象 | 检查 |
| --- | --- |
| 状态页显示「被拒绝」 | 宿主未授予 `register_provider`，重启应用后重新授权 |
| 源选择器里看不到该源 | 确认插件已启用；`register_provider` 成功日志在插件 stdout / `logs/app.log` |
| 状态页显示「不兼容：缺少列 …」 | 源仓库的 `index_v2.csv` 缺官方源必备列，见 `src/source.rs` 的 `REQUIRED_COLUMNS` |
| 状态页显示「不可用：HTTP …」 | 设备网络无法访问 `raw.githubusercontent.com` |
| 源切换后一直空白 | 看日志有没有 `provider-action ... -> ` 开头的返回行；没有说明回调没进来，有但后面报错说明该 action 的响应格式还要调 |
| 日志出现「未处理的 provider action: xxx」 | 宿主用了我还没实现的 action，把名字告诉我即可补上 |
