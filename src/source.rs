use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use crate::http;

/// 第三方市场源仓库（GitHub）。
pub const SOURCE_OWNER: &str = "AstralSightStudios";
pub const SOURCE_REPO: &str = "ABRepo-TestEnv";
pub const SOURCE_BRANCH: &str = "main";

pub const REPO_URL: &str = "https://github.com/AstralSightStudios/ABRepo-TestEnv";
pub const INDEX_URL: &str =
    "https://raw.githubusercontent.com/AstralSightStudios/ABRepo-TestEnv/refs/heads/main/index_v2.csv";
pub const DEVICES_URL: &str =
    "https://raw.githubusercontent.com/AstralSightStudios/ABRepo-TestEnv/refs/heads/main/devices_v2.json";

/// 官方源 `index_v2.csv` 的必备列，缺任何一列宿主都会拒绝加载该源。
/// 与 AstroBox 宿主的 `parse_index` 保持一致。
pub const REQUIRED_COLUMNS: [&str; 12] = [
    "id",
    "name",
    "restype",
    "repo_owner",
    "repo_name",
    "repo_commit_hash",
    "icon",
    "cover",
    "tags",
    "device_vendors",
    "devices",
    "paid_type",
];

pub const PAYED_FREE: &str = "";
pub const PAID: &str = "paid";
pub const FORCE_PAID: &str = "force_paid";

/// 注册给宿主的社区源名字，也就是资源页右上角源选择器里显示的那一项。
pub const PROVIDER_NAME: &str = "ABRepo-TestEnv";

/// `register-provider` 的名字同时也是宿主派发 `provider-action` 时用的标识，
/// 两者必须一致，否则宿主的回调找不到插件。
pub fn provider_name() -> String {
    PROVIDER_NAME.to_string()
}

/// 图片/文件走哪个 CDN。
///
/// 设备直连 `raw.githubusercontent.com` 经常又慢又超时（实测 24KB 要 1.7s），
/// WebView 直接取图就会空掉。raw 走不通时改这里换镜像即可。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Cdn {
    /// https://raw.githubusercontent.com（官方源默认）
    Raw,
    /// https://cdn.jsdelivr.net/gh
    JsDelivr,
    /// https://ghfast.top（国内常见的 GitHub 代理）
    GhFast,
}

pub const CDN: Cdn = Cdn::Raw;

impl Cdn {
    pub fn raw_url(&self, owner: &str, repo: &str, commit: &str) -> String {
        format!("https://raw.githubusercontent.com/{owner}/{repo}/{commit}")
    }

    pub fn convert(&self, raw: &str) -> String {
        if !raw.contains("https://raw.githubusercontent.com/") {
            return raw.to_string();
        }
        match self {
            Cdn::Raw => raw.to_string(),
            Cdn::JsDelivr => raw
                .replace(
                    "https://raw.githubusercontent.com/",
                    "https://cdn.jsdelivr.net/gh/",
                )
                .replace("/refs/heads/main/", "@main/"),
            Cdn::GhFast => {
                format!("https://ghfast.top/{}", raw.strip_prefix("https://").unwrap_or(raw))
            }
        }
    }
}

/// 资源仓库里某个提交下的原始文件前缀（已按 CDN 设置转换）。
pub fn asset_base(entry: &IndexEntry) -> String {
    CDN.convert(&CDN.raw_url(
        &entry.repo_owner,
        &entry.repo_name,
        &entry.repo_commit_hash,
    ))
}

/// 把 manifest 里的相对路径拼成可访问的绝对地址。
///
/// 路径会做 percent-encode：源 CSV 里存在 `画板 1.png`、`封面.png`、
/// `Q版初音未来表盘 for 小米手环9 Pro/icon.png` 这类带空格和中文的路径，
/// 不编码的话 WebView 很可能直接取不到图。
pub fn resolve_asset(base: &str, path: &str) -> String {
    let path = path.trim();
    if path.is_empty()
        || path.starts_with("http://")
        || path.starts_with("https://")
        || path.starts_with("data:")
        || path.starts_with("blob:")
        || path.starts_with("tauri:")
        || path.starts_with('/')
    {
        return path.to_string();
    }
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        encode_path(path.trim_start_matches('/'))
    )
}

/// 保留 `/` 与 unreserved 字符，其余按 UTF-8 字节 percent-encode。
fn encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for byte in path.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(*byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// `index_v2.csv` 里的一条资源。
#[derive(Clone, Debug, Default)]
pub struct IndexEntry {
    pub id: String,
    pub name: String,
    pub restype: String,
    pub repo_owner: String,
    pub repo_name: String,
    pub repo_commit_hash: String,
    pub icon: String,
    pub cover: String,
    pub tags: Vec<String>,
    pub device_vendors: Vec<String>,
    pub devices: Vec<String>,
    pub paid_type: String,
}

#[derive(Clone, Debug, Default)]
pub struct IndexStats {
    pub http_status: u16,
    pub bytes: usize,
    pub elapsed_ms: u64,
    pub entries: usize,
    pub skipped_rows: usize,
    pub free: usize,
    pub paid: usize,
    pub force_paid: usize,
    pub vendors: usize,
    pub devices: usize,
    pub restypes: Vec<(String, usize)>,
    pub missing_columns: Vec<String>,
    pub error: Option<String>,
}

impl IndexStats {
    pub fn ok(&self) -> bool {
        self.error.is_none() && self.missing_columns.is_empty() && self.entries > 0
    }

    pub fn summary(&self) -> String {
        if let Some(error) = &self.error {
            return format!("检测失败：{error}");
        }
        if !self.missing_columns.is_empty() {
            return format!("缺少列：{}", self.missing_columns.join(", "));
        }
        if self.entries == 0 {
            return "索引为空".to_string();
        }
        format!(
            "{} 条资源 / {} 个设备型号 / 免费 {} · 付费 {} · 强制付费 {}",
            self.entries, self.devices, self.free, self.paid, self.force_paid
        )
    }

    pub fn restype_summary(&self) -> String {
        if self.restypes.is_empty() {
            return "暂无".to_string();
        }
        self.restypes
            .iter()
            .map(|(name, count)| format!("{name} {count}"))
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

pub fn summarize(entries: &[IndexEntry], skipped_rows: usize) -> IndexStats {
    let mut stats = IndexStats {
        entries: entries.len(),
        skipped_rows,
        ..IndexStats::default()
    };

    let mut vendors = BTreeSet::new();
    let mut devices = BTreeSet::new();
    let mut restypes: BTreeMap<String, usize> = BTreeMap::new();

    for entry in entries {
        match entry.paid_type.as_str() {
            PAID => stats.paid += 1,
            FORCE_PAID => stats.force_paid += 1,
            _ => stats.free += 1,
        }
        vendors.extend(entry.device_vendors.iter().cloned());
        devices.extend(entry.devices.iter().cloned());
        if !entry.restype.is_empty() {
            *restypes.entry(entry.restype.clone()).or_insert(0) += 1;
        }
    }

    stats.vendors = vendors.len();
    stats.devices = devices.len();
    stats.restypes = restypes.into_iter().collect();
    stats
}

/// 拉取并解析 index_v2.csv，用于状态页自检。
pub fn probe_index() -> IndexStats {
    let started = Instant::now();
    let mut stats = IndexStats::default();

    let (status, body) = match http::get(INDEX_URL) {
        Ok(result) => result,
        Err(error) => {
            stats.error = Some(error);
            stats.elapsed_ms = started.elapsed().as_millis() as u64;
            return stats;
        }
    };

    stats.http_status = status;
    stats.bytes = body.len();

    if !(200..300).contains(&status) {
        stats.error = Some(format!("HTTP {status}"));
        stats.elapsed_ms = started.elapsed().as_millis() as u64;
        return stats;
    }

    match parse_index(&body) {
        Ok((entries, skipped_rows)) => {
            let mut parsed = summarize(&entries, skipped_rows);
            parsed.bytes = stats.bytes;
            parsed.http_status = stats.http_status;
            parsed.elapsed_ms = started.elapsed().as_millis() as u64;
            parsed
        }
        Err(error) => {
            stats.error = Some(error);
            stats.elapsed_ms = started.elapsed().as_millis() as u64;
            stats
        }
    }
}

/// 解析 index_v2.csv。返回 (有效条目, 被跳过的行数)。
///
/// 列校验与「缺资源身份就丢行」的规则都跟宿主 `parse_index` 一致，
/// 这样状态页报出来的问题就是宿主真正会拒的东西。
pub fn parse_index(bytes: &[u8]) -> Result<(Vec<IndexEntry>, usize), String> {
    let text = std::str::from_utf8(bytes)
        .map_err(|e| format!("index_v2.csv 不是合法 UTF-8: {e}"))?
        .trim_start_matches('\u{feff}');

    let records = parse_csv(text);
    let header = records
        .first()
        .ok_or_else(|| "index_v2.csv 为空".to_string())?
        .iter()
        .map(|field| field.trim().to_string())
        .collect::<Vec<_>>();

    let missing_columns = REQUIRED_COLUMNS
        .iter()
        .filter(|required| !header.iter().any(|column| column == *required))
        .map(|required| required.to_string())
        .collect::<Vec<_>>();
    if !missing_columns.is_empty() {
        return Err(format!("缺少列 {}", missing_columns.join(", ")));
    }

    let column_index = |name: &str| header.iter().position(|column| column == name);
    let id_index = column_index("id");
    let name_index = column_index("name");
    let restype_index = column_index("restype");
    let owner_index = column_index("repo_owner");
    let repo_index = column_index("repo_name");
    let commit_index = column_index("repo_commit_hash");
    let icon_index = column_index("icon");
    let cover_index = column_index("cover");
    let tags_index = column_index("tags");
    let vendors_index = column_index("device_vendors");
    let devices_index = column_index("devices");
    let paid_index = column_index("paid_type");

    let mut entries = Vec::new();
    let mut skipped_rows = 0usize;

    for record in records.iter().skip(1) {
        if record.len() == 1 && record[0].trim().is_empty() {
            continue;
        }

        let owner = field(record, owner_index);
        let repo = field(record, repo_index);
        let commit = field(record, commit_index);
        let id = field(record, id_index);

        if id.is_empty() || owner.is_empty() || repo.is_empty() || commit.is_empty() {
            skipped_rows += 1;
            continue;
        }

        entries.push(IndexEntry {
            id: id.to_string(),
            name: field(record, name_index).to_string(),
            restype: normalize_restype(field(record, restype_index)),
            repo_owner: owner.to_string(),
            repo_name: repo.to_string(),
            repo_commit_hash: commit.to_string(),
            icon: field(record, icon_index).to_string(),
            cover: field(record, cover_index).to_string(),
            tags: list_field(record, tags_index),
            device_vendors: list_field(record, vendors_index),
            devices: list_field(record, devices_index),
            paid_type: field(record, paid_index).to_string(),
        });
    }

    if entries.is_empty() {
        return Err("index_v2.csv 解析后没有任何有效条目".to_string());
    }

    Ok((entries, skipped_rows))
}

/// 宿主的 ResourceTypeV2 只认这几种小写值。
fn normalize_restype(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "" => "quick_app".to_string(),
        "quickapp" | "quick_app" | "rpk" => "quick_app".to_string(),
        "watchface" | "watch_face" | "wf" => "watchface".to_string(),
        "canopus" => "canopus".to_string(),
        "res_pack" | "respack" => "res_pack".to_string(),
        "firmware" => "firmware".to_string(),
        other => other.to_string(),
    }
}

fn field(record: &[String], index: Option<usize>) -> &str {
    match index.and_then(|index| record.get(index)) {
        Some(value) => value.trim(),
        None => "",
    }
}

fn list_field(record: &[String], index: Option<usize>) -> Vec<String> {
    let Some(index) = index else {
        return Vec::new();
    };
    let Some(value) = record.get(index) else {
        return Vec::new();
    };
    value
        .split(';')
        .map(|item| item.trim())
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .collect()
}

/// 够用的 CSV 解析：支持双引号包裹、`""` 转义、`\r\n` 换行。
fn parse_csv(text: &str) -> Vec<Vec<String>> {
    let mut records = Vec::new();
    let mut record = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        if in_quotes {
            match ch {
                '"' => {
                    if chars.peek() == Some(&'"') {
                        chars.next();
                        field.push('"');
                    } else {
                        in_quotes = false;
                    }
                }
                other => field.push(other),
            }
            continue;
        }

        match ch {
            '"' => in_quotes = true,
            ',' => record.push(std::mem::take(&mut field)),
            '\r' => {}
            '\n' => {
                record.push(std::mem::take(&mut field));
                records.push(std::mem::take(&mut record));
            }
            other => field.push(other),
        }
    }

    if !field.is_empty() || !record.is_empty() {
        record.push(field);
        records.push(record);
    }

    records
}
