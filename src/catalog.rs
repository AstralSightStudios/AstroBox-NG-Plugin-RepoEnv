use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::cache;
use crate::http;
use crate::source::{self, IndexEntry, FORCE_PAID, PAID};

/// 宿主 `get_categories` 里固定带的分类标记。
pub const HIDE_PAID: &str = "hide_paid";
pub const HIDE_FORCE_PAID: &str = "hide_force_paid";

/// 宿主分类里的资源类型。
const RESOURCE_TYPES: [&str; 4] = ["quick_app", "watchface", "canopus", "res_pack"];

/// 一份源目录快照：index_v2.csv + devices_v2.json。
#[derive(Clone, Debug, Default)]
pub struct Catalog {
    pub entries: Vec<IndexEntry>,
    /// 设备 id -> 设备名（devices_v2.json，含 vivo）
    pub device_names: BTreeMap<String, String>,
    /// 只放小米设备名：宿主分类列表目前也只给 xiaomi 分类
    pub category_devices: Vec<String>,
    pub fetched_at_ms: u64,
    /// 本次目录是不是直接来自磁盘缓存
    pub from_cache: bool,
}

/// 拉取整份目录。宿主 `refresh` / 状态页自检都走这里。
///
/// 走磁盘缓存：TTL 内重复切源不会产生任何网络请求，
/// 也就不会因为弱网把宿主线程堵住。
pub fn refresh(force: bool) -> Result<Catalog, String> {
    tracing::info!(
        "catalog refresh start: {} (force={})",
        source::INDEX_URL,
        force
    );

    let (index_bytes, index_cached) =
        cache::fetch_cached(source::INDEX_URL, cache::index_file(), force)?;
    let (entries, skipped_rows) = source::parse_index(&index_bytes)?;
    tracing::info!(
        "catalog index ok: entries={} skipped={} bytes={} cached={}",
        entries.len(),
        skipped_rows,
        index_bytes.len(),
        index_cached
    );

    // 设备表拿不到不影响浏览，只是没有设备分类和 display_name。
    let mut device_names = BTreeMap::new();
    let mut category_devices = Vec::new();
    match cache::fetch_cached(source::DEVICES_URL, cache::devices_file(), force) {
        Ok((body, _)) => {
            let xiaomi = parse_device_map(&body, "xiaomi");
            category_devices = names_of(&xiaomi);
            device_names = merge_device_maps(xiaomi, parse_device_map(&body, "vivo"));
            tracing::info!("catalog devices ok: xiaomi={}", category_devices.len());
        }
        Err(error) => tracing::warn!("devices_v2.json unavailable: {}", error),
    }

    Ok(Catalog {
        entries,
        device_names,
        category_devices,
        fetched_at_ms: now_ms(),
        from_cache: index_cached,
    })
}

fn merge_device_maps(
    base: BTreeMap<String, String>,
    extra: BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut merged = base;
    for (id, name) in extra {
        merged.entry(id).or_insert(name);
    }
    merged
}

fn names_of(map: &BTreeMap<String, String>) -> Vec<String> {
    let mut names: Vec<String> = map.values().cloned().collect();
    names.dedup();
    names
}

fn parse_device_map(bytes: &[u8], vendor: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
        return map;
    };
    let Some(devices) = value.get(vendor).and_then(Value::as_object) else {
        return map;
    };
    for device in devices.values() {
        let Some(id) = device.get("id").and_then(Value::as_str) else {
            continue;
        };
        let name = device
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(id)
            .to_string();
        map.insert(id.to_string(), name);
    }
    map
}

pub fn categories(catalog: &Catalog) -> Vec<String> {
    let mut list: Vec<String> = vec![
        HIDE_PAID.to_string(),
        HIDE_FORCE_PAID.to_string(),
    ];
    list.extend(RESOURCE_TYPES.iter().map(|value| value.to_string()));
    for name in catalog.category_devices.iter() {
        if !list.contains(name) {
            list.push(name.clone());
        }
    }
    list
}

/// 列表页条目，对应宿主的 `ManifestItemV2`。
pub fn items(page: &PageQuery, catalog: &Catalog) -> Vec<Value> {
    let filtered = filter(catalog, page);
    let total = filtered.len();
    let start = (page.page as usize).saturating_mul(page.limit as usize);
    if start >= total {
        return Vec::new();
    }
    let end = std::cmp::min(start + page.limit as usize, total);

    filtered[start..end].iter().map(|entry| item_json(entry)).collect()
}

pub fn total(catalog: &Catalog) -> u64 {
    filter(catalog, &PageQuery::default()).len() as u64
}

/// 详情，对应宿主的 `ManifestV2`。
pub fn item_manifest(catalog: &Catalog, item_id: &str) -> Result<Value, String> {
    let entry = catalog
        .entries
        .iter()
        .find(|entry| entry.id == item_id)
        .ok_or_else(|| format!("资源不存在: {item_id}"))?;

    let base = source::asset_base(entry);
    let manifest = fetch_manifest(&base)?;
    let mut manifest = normalize_manifest(manifest)?;

    if let Some(item) = manifest.get_mut("item").and_then(Value::as_object_mut) {
        item.insert(
            "id".to_string(),
            json!(if item
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .is_empty()
            {
                entry.id.clone()
            } else {
                item.get("id").and_then(Value::as_str).unwrap_or_default().to_string()
            }),
        );
        item.insert("name".to_string(), json!(entry.name));
        item.insert("restype".to_string(), json!(entry.restype));
        if !entry.paid_type.is_empty() {
            item.insert("paid_type".to_string(), json!(entry.paid_type));
        }
        item.insert("icon".to_string(), json!(source::resolve_asset(&base, &entry.icon)));

        let manifest_cover = item
            .get("cover")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let cover = if manifest_cover.is_empty() {
            source::resolve_asset(&base, &entry.cover)
        } else {
            source::resolve_asset(&base, &manifest_cover)
        };
        item.insert("cover".to_string(), json!(cover.clone()));

        let preview = item
            .get("preview")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut preview: Vec<Value> = preview
            .iter()
            .filter_map(Value::as_str)
            .map(|path| json!(source::resolve_asset(&base, path)))
            .collect();
        if preview.is_empty() && !cover.is_empty() {
            preview.push(json!(cover));
        }
        item.insert("preview".to_string(), Value::Array(preview));

        if !item
            .get("author")
            .map(Value::is_array)
            .unwrap_or(false)
            || item.get("author").and_then(Value::as_array).map(Vec::is_empty).unwrap_or(true)
        {
            item.insert(
                "author".to_string(),
                json!([{ "name": entry.repo_owner, "bindABAccount": false }]),
            );
        }
    }

    // 补上宿主 ManifestV2 必填的 links / downloads / ext，并给下载项补绝对地址与设备名。
    let downloads = downloads_from(&manifest, &base, catalog);
    let object = manifest
        .as_object_mut()
        .ok_or_else(|| "manifest 不是对象".to_string())?;
    object.entry("links").or_insert_with(|| json!([]));
    object.entry("ext").or_insert_with(|| json!({}));
    object.insert("downloads".to_string(), downloads);

    Ok(manifest)
}

/// 解析某个资源在指定设备上的下载地址。
pub fn download_url(catalog: &Catalog, item_id: &str, device: &str) -> Result<String, String> {
    let entry = catalog
        .entries
        .iter()
        .find(|entry| entry.id == item_id)
        .ok_or_else(|| format!("资源不存在: {item_id}"))?;

    let base = source::asset_base(entry);
    let manifest = fetch_manifest(&base)?;
    let file_name = manifest
        .get("downloads")
        .and_then(|downloads| downloads.get(device))
        .and_then(|entry| {
            entry
                .get("url")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| {
                    entry
                        .get("file_name")
                        .and_then(Value::as_str)
                        .map(|name| format!("{base}/{name}"))
                })
        })
        .ok_or_else(|| format!("{device} 没有可用下载地址"))?;

    Ok(source::resolve_asset(&base, &file_name))
}

pub fn fetch_manifest(base: &str) -> Result<Value, String> {
    let url = format!("{base}/manifest_v2.json");
    match http::get(&url) {
        Ok((status, body)) if (200..300).contains(&status) => {
            serde_json::from_slice(&body).map_err(|e| format!("manifest_v2.json 解析失败: {e}"))
        }
        Ok((404, _)) => {
            // 老资源只有 v1 manifest.json，字段基本兼容，缺的地方后面统一补。
            let legacy = format!("{base}/manifest.json");
            match http::get(&legacy) {
                Ok((status, body)) if (200..300).contains(&status) => {
                    serde_json::from_slice(&body).map_err(|e| format!("manifest.json 解析失败: {e}"))
                }
                Ok((status, _)) => Err(format!("manifest 404 / HTTP {status}")),
                Err(error) => Err(error),
            }
        }
        Ok((status, _)) => Err(format!("manifest_v2.json HTTP {status}")),
        Err(error) => Err(error),
    }
}

/// 把 v1/v2 manifest 统一成宿主 ManifestV2 的形状。
pub fn normalize_manifest(manifest: Value) -> Result<Value, String> {
    let mut manifest = manifest;
    let object = manifest
        .as_object_mut()
        .ok_or_else(|| "manifest 不是对象".to_string())?;

    let mut item = object
        .remove("item")
        .and_then(|item| item.as_object().cloned())
        .unwrap_or_default();
    for key in ["id", "restype", "name", "description", "preview", "icon", "cover"] {
        if !item.contains_key(key) {
            item.insert(key.to_string(), default_for(key));
        }
    }
    if !item
        .get("preview")
        .map(Value::is_array)
        .unwrap_or(false)
    {
        item.insert("preview".to_string(), json!([]));
    }
    if !item.get("author").map(Value::is_array).unwrap_or(false) {
        item.insert("author".to_string(), json!([]));
    }
    object.insert("item".to_string(), Value::Object(item));

    if !object.get("links").map(Value::is_array).unwrap_or(false) {
        object.insert("links".to_string(), json!([]));
    }
    if !object.get("downloads").map(Value::is_object).unwrap_or(false) {
        object.insert("downloads".to_string(), json!({}));
    }
    if !object.contains_key("ext") {
        object.insert("ext".to_string(), json!({}));
    }

    Ok(manifest)
}

fn default_for(key: &str) -> Value {
    match key {
        "preview" | "author" => json!([]),
        _ => json!(""),
    }
}

pub fn downloads_from(manifest: &Value, base: &str, catalog: &Catalog) -> Value {
    let downloads = manifest
        .get("downloads")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    let mut out = serde_json::Map::new();
    for (device, entry) in downloads {
        let Value::Object(mut entry) = entry else {
            continue;
        };
        entry.entry("version").or_insert_with(|| json!(""));
        if let Some(file_name) = entry.get("file_name").and_then(Value::as_str) {
            entry.insert(
                "url".to_string(),
                json!(source::resolve_asset(base, file_name)),
            );
        }
        if !entry.contains_key("display_name")
            && let Some(name) = catalog.device_names.get(&device)
        {
            entry.insert("display_name".to_string(), json!(name));
        }

        out.insert(device, Value::Object(entry));
    }

    Value::Object(out)
}

fn item_json(entry: &IndexEntry) -> Value {
    let base = source::asset_base(entry);
    let icon = source::resolve_asset(&base, &entry.icon);
    let cover = source::resolve_asset(&base, &entry.cover);

    let mut item = serde_json::Map::new();
    item.insert("id".to_string(), json!(entry.id));
    item.insert("name".to_string(), json!(entry.name));
    item.insert("restype".to_string(), json!(entry.restype));
    item.insert("description".to_string(), json!(entry.tags.join("；")));
    item.insert("preview".to_string(), json!([cover]));
    item.insert("icon".to_string(), json!(icon));
    item.insert("cover".to_string(), json!(cover));
    item.insert(
        "paid_type".to_string(),
        json!(match entry.paid_type.as_str() {
            PAID => PAID,
            FORCE_PAID => FORCE_PAID,
            _ => source::PAYED_FREE,
        }),
    );
    item.insert(
        "author".to_string(),
        json!([{ "name": entry.repo_owner, "bindABAccount": false }]),
    );

    Value::Object(item)
}

#[derive(Clone, Debug)]
pub struct PageQuery {
    pub page: u32,
    pub limit: u32,
    pub filter: Option<String>,
    pub sort: String,
    pub category: Vec<String>,
}

impl Default for PageQuery {
    fn default() -> Self {
        PageQuery {
            page: 0,
            limit: 20,
            filter: None,
            sort: "time".to_string(),
            category: Vec::new(),
        }
    }
}

fn filter<'a>(catalog: &'a Catalog, query: &PageQuery) -> Vec<&'a IndexEntry> {
    let hide_paid = query.category.iter().any(|c| c == HIDE_PAID);
    let hide_force_paid = query.category.iter().any(|c| c == HIDE_FORCE_PAID);
    let selected_types: Vec<&str> = RESOURCE_TYPES
        .iter()
        .copied()
        .filter(|kind| query.category.iter().any(|selected| selected == kind))
        .collect();

    // 分类里给的是设备名，先映射回设备 id。
    let mut selected_devices: Vec<&str> = Vec::new();
    for (id, name) in catalog.device_names.iter() {
        if query.category.iter().any(|selected| selected == name) {
            selected_devices.push(id);
        }
    }

    let keyword = query
        .filter
        .as_ref()
        .map(|value| value.trim().to_lowercase())
        .filter(|value| !value.is_empty());

    let mut result: Vec<&IndexEntry> = catalog
        .entries
        .iter()
        .filter(|entry| {
            if selected_types.is_empty() && selected_devices.is_empty() {
                // 没有分类筛选时才应用 hide_paid，跟官方行为一致
                !(hide_paid && entry.paid_type == PAID || hide_force_paid && entry.paid_type == FORCE_PAID)
            } else {
                let type_ok = selected_types.is_empty()
                    || selected_types.contains(&entry.restype.as_str());
                let device_ok = selected_devices.is_empty()
                    || entry.devices.iter().any(|device| selected_devices.contains(&device.as_str()));
                type_ok && device_ok
            }
        })
        .filter(|entry| match &keyword {
            None => true,
            Some(keyword) => {
                entry.id.to_lowercase().starts_with(keyword)
                    || entry.name.to_lowercase().contains(keyword)
                    || entry.repo_owner.to_lowercase().contains(keyword)
                    || entry
                        .tags
                        .iter()
                        .any(|tag| tag.to_lowercase().contains(keyword))
            }
        })
        .collect();

    match query.sort.as_str() {
        "random" => shuffle(&mut result),
        "name" => result.sort_by(|a, b| a.name.cmp(&b.name)),
        // 官方源按时间排序其实是把索引倒序（索引本身按加入顺序排）
        _ => result.reverse(),
    }

    result
}

/// 没有 rand 依赖，用时间做种子的 xorshift 洗牌，够用。
fn shuffle<T>(items: &mut [T]) {
    if items.len() < 2 {
        return;
    }
    let mut state = now_ms() | 1;
    for index in (1..items.len()).rev() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        items.swap(index, (state % (index as u64 + 1)) as usize);
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
