//! 源元数据的磁盘缓存。
//!
//! 目的很单纯：**别在宿主等着渲染/事件的路径上做网络请求**。
//! `index_v2.csv` 走一次弱网要 1~2 秒，`devices_v2.json` 更慢，
//! 而插件的网络层是阻塞的。所以第一次拉到之后落盘，
//! TTL 内的后续请求直接读文件，宿主那边就是零等待。

use std::time::{SystemTime, UNIX_EPOCH};

use crate::http;

/// 缓存有效期：10 分钟。测试源改得频繁，但也没必要每次切源都联网。
pub const TTL_MS: u64 = 10 * 60 * 1000;

const INDEX_FILE: &str = "cache/index_v2.csv";
const DEVICES_FILE: &str = "cache/devices_v2.json";

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 取一份 URL 的内容：缓存新鲜就直接用，否则联网并落盘。
///
/// 返回 (内容, 是否来自缓存)。
pub fn fetch_cached(url: &str, cache_file: &str, force: bool) -> Result<(Vec<u8>, bool), String> {
    if !force
        && let Some(bytes) = read_fresh(cache_file)
    {
        return Ok((bytes, true));
    }

    let (status, body) = http::get(url)?;
    if !(200..300).contains(&status) {
        // 网络失败时宁可回退到过期缓存，也不要让宿主那边直接失败。
        if let Some(bytes) = read(cache_file) {
            tracing::warn!("{} HTTP {}，回退到过期缓存 {}", url, status, cache_file);
            return Ok((bytes, true));
        }
        return Err(format!("HTTP {status}"));
    }

    write(cache_file, &body);
    Ok((body, false))
}

fn read_fresh(path: &str) -> Option<Vec<u8>> {
    let bytes = read(path)?;
    match modified_ms(path) {
        Some(modified) if now_ms().saturating_sub(modified) <= TTL_MS => Some(bytes),
        _ => None,
    }
}

fn read(path: &str) -> Option<Vec<u8>> {
    match std::fs::read(path) {
        Ok(bytes) => {
            tracing::info!("cache hit: {} ({} bytes)", path, bytes.len());
            Some(bytes)
        }
        Err(_) => None,
    }
}

fn write(path: &str, bytes: &[u8]) {
    if let Some(parent) = std::path::Path::new(path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::write(path, bytes) {
        Ok(()) => tracing::info!("cache write: {} ({} bytes)", path, bytes.len()),
        Err(error) => tracing::warn!("cache write failed {}: {}", path, error),
    }
}

fn modified_ms(path: &str) -> Option<u64> {
    let metadata = std::fs::metadata(path).ok()?;
    let modified = metadata.modified().ok()?;
    let duration = modified.duration_since(UNIX_EPOCH).ok()?;
    Some(duration.as_millis() as u64)
}

pub fn index_file() -> &'static str {
    INDEX_FILE
}

pub fn devices_file() -> &'static str {
    DEVICES_FILE
}
