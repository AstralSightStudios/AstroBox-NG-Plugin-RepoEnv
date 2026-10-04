//! AstroBox 账号：自己跑一遍 OAuth 2.0 授权码流程。
//!
//! ## 为什么插件要自己登录，而不是用 `account` 宿主接口
//!
//! `account::get_current()` 只返回脱敏资料和绑定状态，**刻意不返回 token/cookie**
//! （见 docs/plugin-v4/host-api/identity.md）。所以插件想用 AstroBox 的加密资源接口，
//! 就得自己拿一份 access token。
//!
//! ## 流程（全部用 v4 特性）
//!
//! 1. `browser.open` 带着 `intercept-prefixes = ["https://abox.run/open"]` 打开
//!    CAS 授权页。这是 v4 文档里「劫持 OAuth 回调」的标准用法。
//! 2. 用户在浏览器里登录，CAS 完成后跳回 `redirect_uri`，导航被宿主拦下。
//! 3. `browser.wait_for_intercept` 拿到**完整回调 URL**，取出 `code` 与 `state`，
//!    校验 `state` 与我们生成的一致（防 CSRF / 回调注入）。
//! 4. 用 `code` 换 token：`POST {api_base}/auth/login?code=...`。
//!    这一步是**无鉴权**的（没有 client secret、没有设备指纹），
//!    返回 `{ token, refreshToken }`。
//! 5. token 落在插件自己的缓存目录里，之后请求 AstroBox 接口时带
//!    `X-ASTROBOX-TOKEN` 头。
//!
//! ## 安全边界
//!
//! * token 只写插件自己的目录，**不写日志**（`tracing` 里只出现 source 和时间）。
//! * 断开登录即删除文件。
//! * 这里只记录协议事实（端点 URL、参数名、头名），不复制宿主客户端的实现。

use serde::{Deserialize, Serialize};

use crate::astrobox::psys_host_v4::{account, browser};
use crate::http;

/// 登录态文件名（相对插件工作目录，与 `cache/` 同级）。
const ACCOUNT_FILE: &str = "cache/astrobox_account.json";

/// CAS 的公开客户端 ID。OAuth 公共客户端没有 secret，这个值随客户端分发的。
const CLIENT_ID: &str = "0fada617eb3ba5e1c2a3";

/// 授权回调地址。CAS 侧按 client_id 注册，插件与宿主用同一个；
/// 命中 `CALLBACK_PREFIX` 后导航被拦截，不会真的跳出去。
const REDIRECT_URI: &str = "https://abox.run/open?source=astrobox";

/// 拦截前缀：`browser.open` 命中它就取消导航并回传完整 URL。
const CALLBACK_PREFIX: &str = "https://abox.run/open";

/// 等待用户完成登录的上限。浏览器被用户直接关掉会立刻返回错误，
/// 这个超时只是兜底，避免一直占着 guest task。
const LOGIN_TIMEOUT_MS: u64 = 5 * 60 * 1000;

/// 账号源。同一套协议，两套端点。
pub struct AccountSource {
    /// 与宿主 `AccountSource` 的 serde 名一致，便于和 `account::get_current()` 对齐。
    pub key: &'static str,
    pub label: &'static str,
    /// CAS：授权页 + 换 token。
    pub cas_base: &'static str,
    /// AstroBox API：换 token + 业务接口。
    pub api_base: &'static str,
}

pub const SOURCES: [AccountSource; 2] = [
    AccountSource {
        key: "casAstralsight",
        label: "AstralSight",
        cas_base: "https://cas.astralsight.space",
        api_base: "https://astrobox-api.astralsight.space",
    },
    AccountSource {
        key: "waterFlames",
        label: "水与火",
        cas_base: "https://ascas.waterflames.cn",
        api_base: "https://asastrobox-api.waterflames.cn",
    },
];

pub fn default_source() -> &'static AccountSource {
    &SOURCES[0]
}

pub fn source_by_key(key: &str) -> Option<&'static AccountSource> {
    SOURCES.iter().find(|source| source.key == key)
}

/// 落盘的登录态。
#[derive(Serialize, Deserialize, Clone)]
pub struct StoredAccount {
    pub source_key: String,
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: String,
    pub saved_at_ms: u64,
}

pub fn load() -> Option<StoredAccount> {
    let raw = std::fs::read_to_string(ACCOUNT_FILE).ok()?;
    serde_json::from_str(&raw).ok()
}

pub fn clear() {
    let _ = std::fs::remove_file(ACCOUNT_FILE);
}

fn save(account: &StoredAccount) -> Result<(), String> {
    if let Some(parent) = std::path::Path::new(ACCOUNT_FILE).parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {e}"))?;
    }
    let json = serde_json::to_string_pretty(account).map_err(|e| format!("序列化失败: {e}"))?;
    std::fs::write(ACCOUNT_FILE, json).map_err(|e| format!("写入登录态失败: {e}"))
}

/// 页面上一行状态描述。
pub fn status_text() -> String {
    match load() {
        Some(account) => match source_by_key(&account.source_key) {
            Some(source) => format!("已连接 · {}", source.label),
            None => format!("已连接 · {}", account.source_key),
        },
        None => "未连接".to_string(),
    }
}

/// 当前 access token，供后续调用 AstroBox 接口时使用。
pub fn access_token() -> Option<String> {
    load().map(|account| account.access_token)
}

/// 带 `X-ASTROBOX-TOKEN` 发一次 POST。响应外层是 `{success, message, data}`。
pub fn authorized_post(
    path: &str,
    body: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let account = load().ok_or_else(|| "尚未连接 AstroBox 账号".to_string())?;
    let source = source_by_key(&account.source_key)
        .ok_or_else(|| format!("未知的账号源: {}", account.source_key))?;

    let url = format!("{}{}", source.api_base, path);
    let (status, raw) = http::post_json(&url, &[("X-ASTROBOX-TOKEN", &account.access_token)])?;
    let text = String::from_utf8_lossy(&raw).to_string();
    if !(200..300).contains(&status) {
        return Err(format!("HTTP {status}: {}", truncate(&text, 200)));
    }

    let envelope: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("解析响应失败: {e}"))?;
    if envelope.get("success").and_then(|v| v.as_bool()) == Some(false) {
        let message = envelope
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("未知错误");
        return Err(message.to_string());
    }
    let _ = body;
    Ok(envelope.get("data").cloned().unwrap_or(serde_json::Value::Null))
}

/// 跑完整登录流程。成功返回 Ok，失败返回可直接展示的错误文案。
///
/// 这一步会阻塞到用户在浏览器里完成登录（`wait_for_intercept`），
/// 但期间浏览器窗口盖在应用上方，且这是唯一能拿到 token 的路径。
pub async fn connect() -> Result<(), String> {
    // 宿主里已经登录过的话，沿用同一个账号源，省得用户在另一个 CAS 域再登一次。
    let source = preferred_source().await;
    let state = random_state()?;

    let authorize_url = build_authorize_url(source, &state);
    tracing::info!("打开 AstroBox 授权页: source={}", source.key);

    let id = browser::open(browser::OpenOptions {
        url: authorize_url,
        title: Some("登录 AstroBox".to_string()),
        user_agent: None,
        intercept_prefixes: vec![CALLBACK_PREFIX.to_string()],
        close_on_intercept: true,
        ephemeral: true,
        width: None,
        height: None,
    })
    .await
    .map_err(|e| format!("打开授权页失败: {e}"))?;

    let callback = browser::wait_for_intercept(id, Some(LOGIN_TIMEOUT_MS))
        .await
        .map_err(|e| format!("未完成登录: {e}"))?;

    let code = parse_callback(&callback, &state)?;
    let token_pair = exchange_code(source, &code).await?;

    save(&StoredAccount {
        source_key: source.key.to_string(),
        access_token: token_pair.access_token,
        refresh_token: token_pair.refresh_token,
        saved_at_ms: crate::cache::now_ms(),
    })
    .map_err(|e| format!("保存登录态失败: {e}"))?;

    // 只记 source 和时间，token 不进日志。
    tracing::info!("AstroBox 登录成功: source={}", source.key);
    Ok(())
}

pub fn disconnect() {
    clear();
    tracing::info!("已断开 AstroBox 账号");
}

/// 宿主已登录时优先沿用它的账号源。
async fn preferred_source() -> &'static AccountSource {
    match account::get_current().await {
        Ok(Some(profile)) => {
            if let Some(source) = source_by_key(&profile.source) {
                tracing::info!("沿用宿主账号源: {}", source.key);
                return source;
            }
            tracing::info!("宿主账号源无法识别: {}", profile.source);
        }
        Ok(None) => tracing::info!("宿主未登录，使用默认账号源"),
        Err(e) => tracing::info!("查询宿主账号失败: {e}"),
    }
    default_source()
}

fn build_authorize_url(source: &AccountSource, state: &str) -> String {
    format!(
        "{}/login/oauth/authorize?response_type=code\
&client_id={CLIENT_ID}\
&scope={}\
&state={state}\
&redirect_uri={}",
        source.cas_base,
        urlencode("openid profile email offline_access"),
        urlencode(REDIRECT_URI),
    )
}

/// 从被拦截的回调 URL 里取 `code`，并校验 `state`。
fn parse_callback(callback: &str, expect_state: &str) -> Result<String, String> {
    let parsed = url::Url::parse(callback).map_err(|e| format!("回调 URL 无法解析: {e}"))?;

    let mut code = None;
    let mut state = None;
    for (key, value) in parsed.query_pairs() {
        match key.as_ref() {
            "code" => code = Some(value.to_string()),
            "state" => state = Some(value.to_string()),
            // 用户在授权页点了「取消」时 CAS 会带 error 回来
            "error" => {
                let detail = value.to_string();
                let desc = parsed
                    .query_pairs()
                    .find(|(k, _)| k == "error_description")
                    .map(|(_, v)| v.to_string())
                    .unwrap_or_default();
                return Err(format!("授权被拒绝: {detail} {desc}").trim_end().to_string());
            }
            _ => {}
        }
    }

    let state = state.ok_or_else(|| "回调缺少 state 参数".to_string())?;
    if state != expect_state {
        // state 不匹配意味着这个回调不是我们发起的，不能拿来换 token。
        return Err("state 校验失败，已中止登录".to_string());
    }
    code.ok_or_else(|| "回调缺少 code 参数".to_string())
}

struct TokenPair {
    access_token: String,
    refresh_token: String,
}

/// `POST {api_base}/auth/login?code=...` 换 token。
///
/// 这一步不需要鉴权头：CAS 的 code 本身就是一次性凭据。
async fn exchange_code(source: &AccountSource, code: &str) -> Result<TokenPair, String> {
    let url = format!("{}/auth/login?code={}", source.api_base, urlencode(code));
    let (status, raw) = http::post_json(&url, &[])?;
    let text = String::from_utf8_lossy(&raw).to_string();

    if !(200..300).contains(&status) {
        return Err(format!("换取 token 失败 HTTP {status}: {}", truncate(&text, 200)));
    }

    let parsed: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("解析 token 响应失败: {e}"))?;

    let error = parsed
        .get("error")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if !error.is_empty() {
        return Err(format!("后端返回错误: {error}"));
    }

    let access_token = parsed
        .get("token")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "token 响应缺少 token 字段".to_string())?
        .to_string();
    let refresh_token = parsed
        .get("refreshToken")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    Ok(TokenPair {
        access_token,
        refresh_token,
    })
}

/// 生成 CSRF `state`。
///
/// 用 `RandomState` 的种子：std 内部从系统随机源取两个 u64 键，
/// 这里混入时间戳和计数器做哈希，够 CSRF 用（只需不可预测，无需密码学强度）。
fn random_state() -> Result<String, String> {
    use std::hash::{BuildHasher, Hasher};

    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u64(crate::cache::now_ms());
    hasher.write_usize(&hasher as *const _ as usize);
    Ok(format!("{:016x}{:016x}", hasher.finish(), hasher.finish()))
}

/// 只编码 URL 查询串里真正会出现的字符。
fn urlencode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn truncate(text: &str, max: usize) -> String {
    let cleaned = text.replace(['\n', '\r'], " ");
    if cleaned.chars().count() <= max {
        return cleaned;
    }
    let head: String = cleaned.chars().take(max).collect();
    format!("{head}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn callback_requires_matching_state() {
        let url = "https://abox.run/open?source=astrobox&code=abc&state=xyz";
        assert_eq!(parse_callback(url, "xyz").unwrap(), "abc");
        assert!(parse_callback(url, "other").is_err());
    }

    #[test]
    fn callback_reports_cas_error() {
        let url = "https://abox.run/open?error=access_denied&error_description=user+cancelled";
        let err = parse_callback(url, "xyz").unwrap_err();
        assert!(err.contains("access_denied"), "{err}");
    }

    #[test]
    fn authorize_url_carries_client_and_redirect() {
        let url = build_authorize_url(&SOURCES[0], "st");
        assert!(url.starts_with("https://cas.astralsight.space/login/oauth/authorize?"));
        assert!(url.contains("client_id=0fada617eb3ba5e1c2a3"));
        assert!(url.contains("state=st"));
        assert!(url.contains("redirect_uri=https%3A%2F%2Fabox.run%2Fopen%3Fsource%3Dastrobox"));
        // scope 的空格必须编码，否则 CAS 只会收到第一个 scope
        assert!(url.contains("scope=openid%20profile%20email%20offline_access"));
    }

    #[test]
    fn urlencode_keeps_unreserved_chars() {
        assert_eq!(urlencode("a-b_c.d~e"), "a-b_c.d~e");
        assert_eq!(urlencode("a b&c"), "a%20b%26c");
    }
}