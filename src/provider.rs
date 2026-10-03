use serde::Deserialize;
use serde_json::{json, Value};

use astrobox_ng_wit::astrobox::psys_host::provider_callback;

use crate::cache;
use crate::catalog::{self, Catalog, PageQuery};
use crate::state;

/// 宿主派发的 provider-action 载荷。
///
/// 线上实测（宿主日志 `pluginsystem::provider_action`）：
/// ```json
/// {"version":1,"provider":"<注册名>","action":"refresh",
///  "requestId":"provider-action:<注册名>:<action>:<序号>",
///  "params":{"config":null,"configRaw":""}}
/// ```
#[derive(Debug, Deserialize)]
struct ActionRequest {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    provider: String,
    action: String,
    #[serde(default, alias = "request_id", rename = "requestId")]
    request_id: String,
    #[serde(default)]
    params: Value,
}

/// 处理一次 provider-action，并把结果 resolve 回宿主。
///
/// 宿主是阻塞等 `resolve_provider_action` 的，所以任何分支都必须回，
/// 包括不认识的动作 —— 宁可回空值，也不要让宿主等到超时。
pub fn handle_action(payload: &str) {
    let request: ActionRequest = match serde_json::from_str(payload) {
        Ok(request) => request,
        Err(error) => {
            tracing::error!("provider-action payload 解析失败: {} ({})", payload, error);
            return;
        }
    };

    tracing::info!(
        "provider-action v{} provider={} action={} params={}",
        request.version,
        request.provider,
        request.action,
        stringify(&request.params)
    );

    let response = match dispatch(&request) {
        Ok(response) => response,
        Err(error) => {
            tracing::error!("provider-action {} 处理失败: {}", request.action, error);
            json!(null)
        }
    };

    let body = stringify(&response);
    tracing::info!(
        "provider-action {} -> {} ({} bytes)",
        request.action,
        truncate(&body, 200),
        body.len()
    );

    let accepted = provider_callback::resolve_provider_action(&request.request_id, &body);
    if !accepted {
        tracing::error!(
            "provider-action {} 的 request_id 已被宿主丢弃: {}",
            request.action,
            request.request_id
        );
    }
}

fn dispatch(request: &ActionRequest) -> Result<Value, String> {
    let params = &request.params;
    let action = request.action.as_str();

    match action {
        // 刷新缓存：宿主要求源进入 Ready 才会去拉列表。
        // force=false 表示允许用磁盘缓存，避免弱网下每次切源都卡十几秒。
        "refresh" | "refresh_provider" => {
            let cached = read_fresh_catalog();
            let catalog = match cached {
                Some(catalog) => catalog,
                None => astrobox_ng_wit::block_on(async { catalog::refresh(false) })?,
            };
            state::with_state(|state| state.catalog = Some(catalog));
            Ok(json!(null))
        }

        // 分类：付费隐藏标记 + 资源类型 + 设备名
        "get_categories" | "categories" => {
            let catalog = read_or_refresh()?;
            Ok(json!(catalog::categories(&catalog)))
        }

        // 分页列表。宿主实际派发的是 `get_index`（params: {limit, page, search}），
        // 其余名字保留成别名，防止不同版本换名字。
        "get_index" | "index" | "get_page" | "page" | "get_items" | "list" | "get_resources" => {
            let catalog = read_or_refresh()?;
            let query = parse_page_query(params);
            tracing::info!(
                "get_page page={} limit={} sort={:?} category={:?} filter={:?}",
                query.page,
                query.limit,
                query.sort,
                query.category,
                query.filter
            );
            Ok(json!(catalog::items(&query, &catalog)))
        }

        // 条目总数
        "get_total_items" | "get_total" | "total" | "total_items" | "get_index_total"
        | "get_count" | "count" => {
            let catalog = read_or_refresh()?;
            Ok(json!(catalog::total(&catalog)))
        }

        // 资源详情
        "get_item_manifest" | "get_item" | "item" | "item_manifest" | "get_manifest"
        | "manifest" | "detail" => {
            let catalog = read_or_refresh()?;
            let item_id = string_param(params, &["itemId", "item_id", "id", "resourceId"]);
            let item_id = item_id.ok_or("缺少 itemId")?;
            astrobox_ng_wit::block_on(async { catalog::item_manifest(&catalog, &item_id) })
        }

        // 下载地址
        "download" | "get_download" | "download_url" | "resolve_download" => {
            let catalog = read_or_refresh()?;
            let item_id = string_param(params, &["itemId", "item_id", "id", "resourceId"]);
            let item_id = item_id.ok_or("缺少 itemId")?;
            let device = string_param(params, &["device", "deviceId", "device_id", "downloadKey"])
                .unwrap_or_default();
            if device.is_empty() {
                return Err("缺少 device".to_string());
            }
            astrobox_ng_wit::block_on(async {
                catalog::download_url(&catalog, &item_id, &device)
            })
            .map(|url| json!(url))
        }

        other => {
            // 宿主问了个我们不认识的动作：先记下来，别让宿主干等。
            tracing::warn!("未处理的 provider action: {}", other);
            Ok(json!(null))
        }
    }
}

/// 下载过程可以往回报进度（宿主侧会转给前端）。
pub fn report_progress(request_id: &str, progress: f32, status: &str) {
    provider_callback::report_provider_action_progress(request_id, progress, status);
}

fn read_or_refresh() -> Result<Catalog, String> {
    if let Some(catalog) = read_fresh_catalog() {
        return Ok(catalog);
    }
    let catalog = astrobox_ng_wit::block_on(async { catalog::refresh(false) })?;
    state::with_state(|state| state.catalog = Some(catalog.clone()));
    Ok(catalog)
}

/// 内存里的目录还在 TTL 内就直接复用，一个网络请求都不用发。
fn read_fresh_catalog() -> Option<Catalog> {
    state::read_state(|state| {
        let catalog = state.catalog.as_ref()?;
        let age = catalog::now_ms().saturating_sub(catalog.fetched_at_ms);
        if age <= cache::TTL_MS {
            Some(catalog.clone())
        } else {
            None
        }
    })
}

fn parse_page_query(params: &Value) -> PageQuery {
    let mut query = PageQuery::default();

    // page/limit 既可能在 params 顶层，也可能在 search 里。
    let search = params.get("search");

    if let Some(page) = uint_param(params, &["page"]) {
        query.page = page;
    }
    if let Some(limit) = uint_param(params, &["limit", "pageSize", "page_size", "count"]) {
        query.limit = limit;
    }
    if let Some(search) = search {
        if let Some(page) = uint_param(search, &["page"]) {
            query.page = page;
        }
        if let Some(limit) = uint_param(search, &["limit", "pageSize", "page_size"]) {
            query.limit = limit;
        }
    }

    let filter = ["filter", "keyword", "search", "query"]
        .iter()
        .find_map(|key| string_value(params.get(*key)))
        .or_else(|| {
            search.and_then(|search| {
                ["filter", "keyword", "query"]
                    .iter()
                    .find_map(|key| string_value(search.get(*key)))
            })
        });
    query.filter = filter;

    let sort = ["sort", "sortRule", "sort_rule", "order"]
        .iter()
        .find_map(|key| string_value(params.get(*key)))
        .or_else(|| {
            search.and_then(|search| {
                ["sort", "sortRule", "sort_rule"]
                    .iter()
                    .find_map(|key| string_value(search.get(*key)))
            })
        });
    if let Some(sort) = sort {
        query.sort = sort.to_lowercase();
    }

    let mut category: Vec<String> = ["category", "categories"]
        .iter()
        .find_map(|key| string_list(params.get(*key)))
        .unwrap_or_default();
    if category.is_empty() {
        category = search
            .and_then(|search| {
                ["category", "categories"]
                    .iter()
                    .find_map(|key| string_list(search.get(*key)))
            })
            .unwrap_or_default();
    }
    query.category = category;

    query
}

fn string_param(params: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| string_value(params.get(*key)))
        .filter(|value| !value.is_empty())
}

fn uint_param(params: &Value, keys: &[&str]) -> Option<u32> {
    keys.iter().find_map(|key| {
        params
            .get(*key)
            .and_then(|value| value.as_u64())
            .map(|value| value.min(u32::MAX as u64) as u32)
    })
}

fn string_value(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(text) if !text.trim().is_empty() => Some(text.trim().to_string()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

fn string_list(value: Option<&Value>) -> Option<Vec<String>> {
    match value? {
        Value::Array(items) => Some(
            items
                .iter()
                .filter_map(Value::as_str)
                .map(|item| item.trim().to_string())
                .filter(|item| !item.is_empty())
                .collect(),
        ),
        Value::String(text) => Some(
            text.split([',', ';'])
                .map(|item| item.trim().to_string())
                .filter(|item| !item.is_empty())
                .collect(),
        ),
        _ => None,
    }
}

fn stringify(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".to_string())
}

fn truncate(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_string();
    }
    let head: String = value.chars().take(limit).collect();
    format!("{head}…")
}
