# ABRepo-TestEnv

一个 AstroBox NG（API Level 3）插件：把 [ABRepo-TestEnv](https://github.com/AstralSightStudios/ABRepo-TestEnv)
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
- 插件页（ui-v3）展示：源信息、注册状态、连通性与 `index_v2.csv` 自检结果
  （HTTP 状态、资源条目数、资源类型分布、设备型号数、免费/付费分布、解析耗时、缺列检查），
  以及宿主最近一次 `refresh` 拉到多少条。

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

`manifest.json` 声明了两项权限：

| 权限 | 用途 |
| --- | --- |
| `network` | 状态页用 waki 拉 `index_v2.csv` 做自检 |
| `register_provider` | 注册社区源（宿主会弹窗询问） |

首次启用时宿主会申请权限，允许后需要重启应用；重启后在「全部资源」页右上角的源选择器里就能看到该源。

## 构建

```bash
# 需要 rustup target add wasm32-wasip2
python scripts/build_dist.py                          # debug
python scripts/build_dist.py --release --package      # release + 打包 .abp
```

产物在 `dist/`：`.wasm`、`manifest.json`、`icon.png`，以及 `ABRepo-TestEnv.abp`。
`.abp` 可直接在 AstroBox 插件页右上角「+」里安装。

## 代码结构

```text
src
├── lib.rs       # lifecycle + event_v3 guest：注册社区源、分发 provider-action / timer
├── provider.rs  # provider-action 协议层：解析载荷、按 action 分发、resolve 回宿主
├── catalog.rs   # 源数据层：目录缓存、分类过滤分页、资源详情、下载地址
├── source.rs    # 源常量、index_v2.csv 解析与统计
├── http.rs      # 基于 waki 的最小 GET
├── state.rs     # 注册状态、目录缓存、自检结果
├── ui.rs        # ui-v3 最小状态页
└── logger.rs    # tracing 初始化（stdout + logs/app.log）
```

## 不要在渲染路径上做网络请求

插件的网络层（waki）是**阻塞**的：调用期间会占住插件所在线程。所以

- `on_ui_render` / `on_ui_event` 里绝对不碰网络 —— 宿主正等着这两个 future，
  一旦阻塞，页面就会一直停在「等待插件响应渲染请求」（真实踩过：弱网下
  `devices_v2.json` 花了 16 秒，宿主的渲染请求直接被饿死）。
- 网络只放在 provider-action 回调里，那是宿主本来就在等我们的地方。
- `index_v2.csv` / `devices_v2.json` 会落到插件目录的 `cache/`（TTL 10 分钟），
  所以切源、翻页基本不发请求；网络失败还会回退到过期缓存。
- 状态页的「重新检测」走宿主 timer，不占用点击回调。

## 排错

| 现象 | 检查 |
| --- | --- |
| 状态页显示「被拒绝」 | 宿主未授予 `register_provider`，重启应用后重新授权 |
| 源选择器里看不到该源 | 确认插件已启用；`register_provider` 成功日志在插件 stdout / `logs/app.log` |
| 状态页显示「不兼容：缺少列 …」 | 源仓库的 `index_v2.csv` 缺官方源必备列，见 `src/source.rs` 的 `REQUIRED_COLUMNS` |
| 状态页显示「不可用：HTTP …」 | 设备网络无法访问 `raw.githubusercontent.com` |
| 源切换后一直空白 | 看日志有没有 `provider-action ... -> ` 开头的返回行；没有说明回调没进来，有但后面报错说明该 action 的响应格式还要调 |
| 日志出现「未处理的 provider action: xxx」 | 宿主用了我还没实现的 action，把名字告诉我即可补上 |
