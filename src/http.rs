use url::Url;
use waki::bindings::wasi::http::{outgoing_handler, types as http_types};
use waki::bindings::wasi::io::streams::StreamError;

const READ_CHUNK_SIZE: u64 = 64 * 1024;
const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;

/// 同步 GET，返回 (HTTP 状态码, 响应体)。
///
/// ⚠️ 这是**阻塞**实现（waki 内部 `block_on`）：调用期间会占住插件所在线程。
/// 所以只允许在宿主本来就在等我们的时候调用（provider-action 回调），
/// 千万不要放进 `on_ui_render` / `on_ui_event` 这类宿主等着渲染/事件的路径，
/// 否则网络一慢，宿主的渲染 future 永远轮不到被 poll，
/// 页面就会一直停在“等待插件响应渲染请求”。
/// 配合 `cache.rs` 的磁盘缓存，常态下根本不会走到这里。
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
