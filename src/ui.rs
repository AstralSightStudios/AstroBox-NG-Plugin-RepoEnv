use crate::astrobox::psys_host_v4::{self as host, dialog, ui};

use crate::account;
use crate::source::{self, IndexStats};
use crate::state::{self, RegisterState};

pub const REFRESH_EVENT: &str = "source_refresh";
pub const OPEN_REPO_EVENT: &str = "source_open_repo";
pub const OPEN_COVER_EVENT: &str = "source_open_cover";
pub const ACCOUNT_CONNECT_EVENT: &str = "account_connect";
pub const ACCOUNT_DISCONNECT_EVENT: &str = "account_disconnect";


const COLOR_CARD: &str = "#1E1E1F";
const COLOR_ROW: &str = "#2A2A2A";
const COLOR_TEXT: &str = "#FFFFFF";
const COLOR_MUTED: &str = "#888888";
const COLOR_OK: &str = "#87E9C6";
const COLOR_OK_BG: &str = "#163B2C";
const COLOR_WARN: &str = "#FFD8A8";
const COLOR_WARN_BG: &str = "#3A2C15";
const COLOR_ERROR: &str = "#FFB4B4";
const COLOR_ERROR_BG: &str = "#3B1A1A";

/// 渲染插件页。
///
/// ⚠️ 这里绝对不能做网络请求：`on_ui_render` 的 future 是宿主在等的，
/// 一旦阻塞，网络一慢页面就会一直停在「等待插件响应渲染请求」。
/// 想自检请走 `schedule_probe()`（宿主 timer），由用户点按钮触发。
pub fn render(element_id: &str) {
    state::with_state(|state| state.root_element_id = Some(element_id.to_string()));
    rerender();
}

pub fn rerender() {
    let element_id = state::read_state(|state| state.root_element_id.clone());
    let Some(element_id) = element_id else {
        return;
    };
    host::ui::render(&element_id, build_page());
}

pub async fn handle_ui_event(event_type: ui::Event, event_id: &str) {
    tracing::info!("UI event: type={:?}, id={}", event_type, event_id);

    if event_type != ui::Event::Click {
        return;
    }

    match event_id {
        ACCOUNT_CONNECT_EVENT => run_account_connect().await,
        ACCOUNT_DISCONNECT_EVENT => {
            account::disconnect();
            rerender();
        }
        REFRESH_EVENT => run_probe().await,
        OPEN_REPO_EVENT => dialog::open_url(source::REPO_URL),
        // 拿系统浏览器打开首条资源的封面：图出不来时，一键就能区分
        // 「设备网络到不了 GitHub」还是「URL 拼错了」。
        OPEN_COVER_EVENT => {
            if let Some(url) = first_cover_url() {
                tracing::info!("open cover in browser: {}", url);
                dialog::open_url(&url);
            }
        }
        _ => {}
    }
}

/// 宿主 timer 事件的载荷是 `{"timerId":..,"kind":..,"payload":".."}`。
pub fn handle_timer_payload(payload: &str) {
    let inner = serde_json::from_str::<serde_json::Value>(payload)
        .ok()
        .and_then(|value| {
            value
                .get("payload")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| payload.to_string());

    tracing::info!("timer payload: {}", inner);
}

/// 登录 AstroBox 账号。
///
/// `account::connect()` 会等用户在浏览器里完成登录（`browser.wait_for_intercept`），
/// 期间这个 guest task 是阻塞的。所以先把「等待登录」渲染出来 —— 浏览器窗口随后
/// 盖在应用上方，回来时用户能看到结果而不是空白页。
async fn run_account_connect() {
    let already_running = state::with_state(|state| {
        if state.account_connecting {
            true
        } else {
            state.account_connecting = true;
            state.account_error = None;
            false
        }
    });
    if already_running {
        return;
    }
    rerender();

    let result = account::connect().await;
    state::with_state(|state| {
        state.account_connecting = false;
        state.account_error = result.as_ref().err().map(|e| e.to_string());
    });
    match &result {
        Ok(()) => tracing::info!("AstroBox 账号已连接"),
        Err(e) => tracing::warn!("AstroBox 登录失败: {e}"),
    }
    rerender();
}

/// 先渲染「检测中」，再跑异步探测。
///
/// v4 的导出是 `async fn`，网络请求可以直接 await，不会再阻塞宿主线程，
/// 也不需要绕宿主 timer 了。
async fn run_probe() {
    let already_running = state::with_state(|state| {
        if state.probing {
            true
        } else {
            state.probing = true;
            state.stats = None;
            false
        }
    });
    if already_running {
        return;
    }
    rerender();

    let stats = source::probe_index();
    tracing::info!(
        "probe {} -> ok={} status={} entries={} elapsed={}ms",
        source::INDEX_URL,
        stats.ok(),
        stats.http_status,
        stats.entries,
        stats.elapsed_ms
    );

    let checked_at = now_ms();
    state::with_state(|state| {
        state.probing = false;
        state.stats = Some(stats);
        state.last_checked_ms = checked_at;
    });
    rerender();
}

fn build_page() -> ui::Element {
    ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .flex_direction(ui::FlexDirection::Column)
        .width_full()
        .padding(20)
        .gap(12)
        .child(build_header())
        .child(build_source_card())
        .child(build_account_card())
        .child(build_probe_card())
        .child(build_actions())
        .child(build_hint())
}

fn build_header() -> ui::Element {
    ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .flex_direction(ui::FlexDirection::Column)
        .width_full()
        .gap(4)
        .child(
            ui::Element::new(ui::ElementType::P, Some("ABRepo-TestEnv")).size(20).text_color(COLOR_TEXT),
        )
        .child(
            ui::Element::new(
                ui::ElementType::P,
                Some("把 ABRepo-TestEnv 仓库注册成 AstroBox 社区源，并在这里自检源的可读性。"),
            )
            .size(13)
            .text_color(COLOR_MUTED),
        )
}

fn build_source_card() -> ui::Element {
    let (provider_name, register_state) = state::read_state(|state| {
        (state.provider_name.clone(), state.register_state)
    });

    let (badge_color, badge_bg) = match register_state {
        RegisterState::Granted => (COLOR_OK, COLOR_OK_BG),
        RegisterState::Denied => (COLOR_ERROR, COLOR_ERROR_BG),
        RegisterState::Pending => (COLOR_WARN, COLOR_WARN_BG),
    };

    ui::Element::new(ui::ElementType::Card, None)
        .flex()
        .flex_direction(ui::FlexDirection::Column)
        .width_full()
        .bg(COLOR_CARD)
        .radius(18)
        .padding(16)
        .gap(10)
        .child(
            ui::Element::new(ui::ElementType::Div, None)
                .flex()
                .align_center()
                .width_full()
                .gap(8)
                .child(
                    ui::Element::new(ui::ElementType::P, Some("源信息")).size(15).text_color(COLOR_TEXT),
                )
                .child(ui::Element::new(ui::ElementType::Div, None).flex_grow(1.0))
                .child(
                    ui::Element::new(ui::ElementType::Badge, Some(register_state.label()))
                        .padding(6)
                        .radius(999)
                        .bg(badge_bg)
                        .text_color(badge_color)
                        .size(12),
                ),
        )
        .child(build_row("仓库", &format!("{}/{}", source::SOURCE_OWNER, source::SOURCE_REPO)))
        .child(build_row("Provider 类型", "URL 型（结构同官方源）"))
        .child(build_row("注册名（源选择器里显示）", &provider_name))
        .child(build_row("索引地址", source::INDEX_URL))
        .child(build_row("设备表", source::DEVICES_URL))
}

fn build_probe_card() -> ui::Element {
    let (probing, stats, last_checked, catalog) = state::read_state(|state| {
        (
            state.probing,
            state.stats.clone(),
            state.last_checked_ms,
            state.catalog.as_ref().map(|catalog| {
                (
                    catalog.entries.len(),
                    catalog.device_names.len(),
                    catalog.fetched_at_ms,
                    catalog.from_cache,
                )
            }),
        )
    });

    let mut rows = ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .flex_direction(ui::FlexDirection::Column)
        .width_full()
        .gap(8);

    if probing {
        rows = rows.child(build_row("状态", "正在检测 index_v2.csv（网络较慢时可能要十几秒）..."));
    } else if let Some(stats) = stats {
        rows = rows
            .child(build_row("状态", &status_text(&stats)))
            .child(build_row("资源条目", &stats.summary()))
            .child(build_row("资源类型", &stats.restype_summary()))
            .child(build_row(
                "设备 / 厂商",
                &format!("{} 个型号 / {} 个厂商", stats.devices, stats.vendors),
            ))
            .child(build_row(
                "索引体积",
                &format!("{} 字节 · 解析耗时 {} ms", stats.bytes, stats.elapsed_ms),
            ));
        if stats.skipped_rows > 0 {
            rows = rows.child(build_row("跳过行", &format!("{} 行缺少资源身份", stats.skipped_rows)));
        }
    } else {
        rows = rows.child(build_row("状态", "尚未检测"));
    }

    rows = rows.child(build_row("最近检测", &format_time(last_checked)));

    match catalog {
        Some((entries, devices, fetched_at, from_cache)) => rows = rows.child(build_row(
            "宿主缓存",
            &format!(
                "{} 条资源 / {} 个设备 · {} · 拉取于 {}",
                entries,
                devices,
                if from_cache { "磁盘缓存" } else { "刚联网拉取" },
                format_time(fetched_at)
            ),
        )),
        None => rows = rows.child(build_row(
            "宿主缓存",
            "还没有收到宿主的 refresh 回调；切到该源时会自动拉取",
        )),
    }

    if let Some(cover) = first_cover_url() {
        rows = rows.child(build_row("首条封面 URL", &cover));
    }

    ui::Element::new(ui::ElementType::Card, None)
        .flex()
        .flex_direction(ui::FlexDirection::Column)
        .width_full()
        .bg(COLOR_CARD)
        .radius(18)
        .padding(16)
        .gap(10)
        .child(
            ui::Element::new(ui::ElementType::P, Some("连通性与索引自检")).size(15).text_color(COLOR_TEXT),
        )
        .child(rows)
}

/// 拿目录里第一条资源的封面地址，用来手动验证图片链路。
fn first_cover_url() -> Option<String> {
    let catalog = state::read_state(|state| state.catalog.clone())?;
    let entry = catalog.entries.first()?;
    let base = source::asset_base(entry);
    Some(source::resolve_asset(&base, &entry.cover))
}

fn build_account_card() -> ui::Element {
    let connecting = state::read_state(|state| state.account_connecting);
    let error = state::read_state(|state| state.account_error.clone());
    let connected = account::load().is_some();

    let status_color = if connected { COLOR_OK } else { COLOR_MUTED };
    let mut card = ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .flex_direction(ui::FlexDirection::Column)
        .width_full()
        .gap(8)
        .bg(COLOR_CARD)
        .radius(16)
        .padding(16)
        .child(
            ui::Element::new(ui::ElementType::P, Some("AstroBox 账号"))
                .size(15)
                .text_color(COLOR_TEXT),
        )
        .child(
            ui::Element::new(
                ui::ElementType::P,
                Some(account::status_text().as_str()),
            )
            .size(13)
            .text_color(status_color),
        );

    if connecting {
        card = card.child(
            ui::Element::new(
                ui::ElementType::P,
                Some("请在弹出的浏览器里完成登录，插件会等回调…"),
            )
            .size(12)
            .text_color(COLOR_WARN),
        );
    }

    if let Some(error) = error {
        card = card.child(
            ui::Element::new(ui::ElementType::P, Some(error.as_str()))
                .size(12)
                .text_color(COLOR_ERROR),
        );
    }

    let label = if connected {
        "重新登录"
    } else if connecting {
        "登录中…"
    } else {
        "连接账号"
    };
    let connect = ui::Element::new(ui::ElementType::Button, Some(label))
        .radius(14)
        .padding(12)
        .bg(COLOR_ROW)
        .text_color(COLOR_TEXT)
        .size(14)
        .width_full()
        .align_center()
        .justify_center()
        .on(ui::Event::Click, ACCOUNT_CONNECT_EVENT);
    let connect = if connecting { connect.disabled() } else { connect };

    let mut row = ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .width_full()
        .gap(10)
        .child(connect);

    if connected {
        let disconnect = ui::Element::new(ui::ElementType::Button, Some("断开"))
            .radius(14)
            .padding(12)
            .bg(COLOR_ROW)
            .text_color(COLOR_TEXT)
            .size(14)
            .flex()
            .flex_grow(1.0)
            .align_center()
            .justify_center()
            .on(ui::Event::Click, ACCOUNT_DISCONNECT_EVENT);
        row = row.child(disconnect);
    }

    card.child(row)
}

fn build_actions() -> ui::Element {
    let probing = state::read_state(|state| state.probing);
    let has_cover = state::read_state(|state| state.catalog.is_some());

    let refresh = ui::Element::new(ui::ElementType::Button, Some("重新检测"))
        .radius(14)
        .padding(12)
        .bg(COLOR_ROW)
        .text_color(COLOR_TEXT)
        .size(14)
        .flex()
        .flex_grow(1.0)
        .align_center()
        .justify_center()
        .on(ui::Event::Click, REFRESH_EVENT);
    let refresh = if probing { refresh.disabled() } else { refresh };

    let open_repo = ui::Element::new(ui::ElementType::Button, Some("打开源仓库"))
        .radius(14)
        .padding(12)
        .bg(COLOR_ROW)
        .text_color(COLOR_TEXT)
        .size(14)
        .flex()
        .flex_grow(1.0)
        .align_center()
        .justify_center()
        .on(ui::Event::Click, OPEN_REPO_EVENT);

    let mut actions = ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .width_full()
        .gap(10)
        .child(refresh)
        .child(open_repo);

    if has_cover {
        actions = actions.child(
            ui::Element::new(ui::ElementType::Button, Some("浏览器打开首条封面（排查图片加载）"))
                .radius(14)
                .padding(12)
                .bg(COLOR_ROW)
                .text_color(COLOR_MUTED)
                .size(13)
                .width_full()
                .align_center()
                .justify_center()
                .on(ui::Event::Click, OPEN_COVER_EVENT),
        );
    }

    actions
}

fn build_hint() -> ui::Element {
    ui::Element::new(
        ui::ElementType::P,
        Some("切换方法：全部资源页右上角的源选择器 -> 选择 ABRepo-TestEnv。首次启用时宿主会申请权限，允许后需重启应用。"),
    )
    .size(12)
    .text_color(COLOR_MUTED)
}

fn build_row(label: &str, value: &str) -> ui::Element {
    ui::Element::new(ui::ElementType::Div, None)
        .flex()
        .flex_direction(ui::FlexDirection::Column)
        .width_full()
        .gap(2)
        .child(
            ui::Element::new(ui::ElementType::P, Some(label)).size(12).text_color(COLOR_MUTED),
        )
        .child(
            ui::Element::new(ui::ElementType::P, Some(value))
                .size(13)
                .text_color(COLOR_TEXT),
        )
}

fn status_text(stats: &IndexStats) -> String {
    if let Some(error) = &stats.error {
        return format!("不可用：{error}");
    }
    if !stats.missing_columns.is_empty() {
        return format!("不兼容：缺少列 {}", stats.missing_columns.join(", "));
    }
    if stats.entries == 0 {
        return "不兼容：索引为空".to_string();
    }
    format!("正常（HTTP {}）", stats.http_status)
}

fn format_time(ms: u64) -> String {
    if ms == 0 {
        return "暂无".to_string();
    }
    let seconds = ms / 1000;
    let days = (seconds / 86400) as i64;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02} UTC",
        year,
        month,
        day,
        (seconds / 3600) % 24,
        (seconds / 60) % 60,
        seconds % 60
    )
}

/// Unix 天数 -> 公历年月日（Howard Hinnant 的 civil_from_days）。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
