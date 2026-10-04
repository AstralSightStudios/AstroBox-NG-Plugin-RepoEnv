//! 基于 **p2** `wasi:http`（waki）的同步 GET。
//!
//! ## 为什么在 wasip3 上还用 p2 的 http
//!
//! v4 插件跑在 WASI p3 运行时上，但宿主**同时提供 p2 和 p3 两套 wasi 接口**
//! （见 plugin-v4/whats-new「WASI Preview 3」一节）。这里必须选 p2：
//!
//! 一旦组件 import 了 p3 的 `wasi:http/types@0.3.0` / `client@0.3.0`，
//! 组件就会切到 p3 的 task 管理机制，而 wit-bindgen 0.57 的 `start_task`
//! 仍然按 p2 的 `$root:[context-get-0]` 上下文槽做
//! `assert!(context_get().is_null())` —— 于是插件**一启动就 panic**：
//!
//! ```
//! assertion failed: context_get().is_null()
//!   at wit-bindgen-0.57.1/src/rt/async_support.rs:501
//!   in start_task::<...lifecycle::_export_on_load_cabi>
//! ```
//!
//! 本机上能正常运行的 v4 插件（简明天气同步器 3.2.0）import 的正是
//! `wasi:http/types@0.2.2` + `outgoing-handler@0.2.2`，可以对照。
//!
//! ## 阻塞性
//!
//! waki 是阻塞实现（内部 `block_on`）。v4 的导出都是 `async func`，
//! 宿主允许它们阻塞等待，所以这里可以安全使用 —— 但仍要克制：
//! 只在「宿主本来就在等我们」的地方调用（provider-action 回调），
//! 不要放进 `on_ui_render`。

use url::Url;
use waki::bindings::wasi::http::{outgoing_handler, types as http_types};
use waki::bindings::wasi::io::streams::StreamError;

const READ_CHUNK_SIZE: u64 = 64 * 1024;
const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;

/// GET 一个 URL，返回 (HTTP 状态码, 响应体)。
pub fn get(url: &str) -> Result<(u16, Vec<u8>), String> {
    let parsed = Url::parse(url).map_err(|e| format!("URL 解析失败: {url} ({e})"))?;

    let headers = http_types::Headers::from_list(&[(
        String::from("user-agent"),
        format!("AstroBox-Plugin/{}", env!("CARGO_PKG_VERSION")).into_bytes(),
    )])
    .map_err(|e| format!("构造请求头失败: {e:?}"))?;

    let request = http_types::OutgoingRequest::new(headers);
    request
        .set_method(&http_types::Method::Get)
        .map_err(|()| "设置请求方法失败".to_string())?;

    let scheme = match parsed.scheme() {
        "http" => http_types::Scheme::Http,
        _ => http_types::Scheme::Https,
    };
    request
        .set_scheme(Some(&scheme))
        .map_err(|()| "设置协议失败".to_string())?;
    request
        .set_authority(Some(parsed.authority()))
        .map_err(|()| "设置 authority 失败".to_string())?;

    let path = match parsed.query() {
        Some(query) => format!("{}?{}", parsed.path(), query),
        None => parsed.path().to_string(),
    };
    request
        .set_path_with_query(Some(&path))
        .map_err(|()| "设置请求路径失败".to_string())?;

    let body = request
        .body()
        .map_err(|_| "打开请求体失败".to_string())?;
    http_types::OutgoingBody::finish(body, None).map_err(|_| "结束请求体失败".to_string())?;

    let options = http_types::RequestOptions::new();
    let pending = outgoing_handler::handle(request, Some(options))
        .map_err(|e| format!("发起请求失败: {e:?}"))?;

    let response = match pending.get() {
        Some(result) => result.map_err(|()| "响应已被消费".to_string())?,
        None => {
            let pollable = pending.subscribe();
            pollable.block();
            pending
                .get()
                .ok_or_else(|| "响应不可用".to_string())?
                .map_err(|()| "响应已被消费".to_string())?
        }
    }
    .map_err(|e| format!("请求失败: {e:?}"))?;

    let status = response.status();
    let incoming = response
        .consume()
        .map_err(|_| "读取响应体失败".to_string())?;
    let stream = incoming
        .stream()
        .map_err(|_| "打开响应流失败".to_string())?;

    let mut body = Vec::new();
    loop {
        match stream.blocking_read(READ_CHUNK_SIZE) {
            Ok(chunk) => {
                if chunk.is_empty() {
                    break;
                }
                if body.len() + chunk.len() > MAX_BODY_BYTES {
                    return Err("响应体超出上限".to_string());
                }
                body.extend_from_slice(&chunk);
            }
            Err(StreamError::Closed) => break,
            Err(e) => return Err(format!("读取响应失败: {e:?}")),
        }
    }

    Ok((status, body))
}