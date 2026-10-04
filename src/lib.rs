// 整个组件只有这一次 `generate!`。重复 generate!（或同时链两份 wit-bindgen）
// 会重复声明 `$root:[context-get-0]` 这个宿主侧 task 上下文槽，
// 表现为插件一启动就 panic：assertion failed: context_get().is_null()
wit_bindgen::generate!({
    path: "wit",
    world: "psys-world-v4",
    generate_all,
});

use crate::exports::astrobox::psys_plugin_v4::{event, lifecycle};

pub mod cache;
pub mod catalog;
pub mod http;
pub mod logger;
pub mod provider;
pub mod source;
pub mod state;
pub mod ui;

struct RepoSourcePlugin;

impl event::Guest for RepoSourcePlugin {
    async fn on_event(event_type: event::EventType, event_payload: String) -> String {
        tracing::info!("event: type={:?}, payload={}", event_type, event_payload);

        match event_type {
            // 宿主拉源数据全靠这个回调，必须回结果，否则源永远不就绪。
            event::EventType::ProviderAction => provider::handle_action(&event_payload).await,
            event::EventType::Timer => {
                ui::handle_timer_payload(&event_payload);
                String::new()
            }
            _ => String::new(),
        }
    }

    async fn on_ui_event(
        event_id: String,
        event_type: event::UiEvent,
        event_payload: String,
    ) -> String {
        tracing::info!(
            "ui event: id={}, type={:?}, payload={}",
            event_id,
            event_type,
            event_payload
        );
        ui::handle_ui_event(event_type, &event_id).await;
        String::new()
    }

    async fn on_ui_render(element_id: String) {
        ui::render(&element_id);
    }

    async fn on_card_render(card_id: String) {
        tracing::info!("on_card_render: {}", card_id);
    }
}

impl lifecycle::Guest for RepoSourcePlugin {
    async fn on_load() {
        logger::init();
        tracing::info!("ABRepo-TestEnv 社区源插件已加载（API Level 4）");

        let provider_name = source::provider_name();
        tracing::info!("registering url provider: {}", provider_name);

        // v4 的 on_load 是 async，可以直接 await 宿主接口，不用 block_on
        let result = crate::astrobox::psys_host_v4::register::register_provider(
            provider_name.clone(),
            crate::astrobox::psys_host_v4::register::ProviderType::Url,
        )
        .await;

        let granted = result.is_ok();
        tracing::info!("register_provider result: {:?}", result);

        state::with_state(|state| {
            state.provider_name = provider_name;
            state.register_state = if granted {
                state::RegisterState::Granted
            } else {
                state::RegisterState::Denied
            };
        });
    }
}

crate::export!(RepoSourcePlugin);