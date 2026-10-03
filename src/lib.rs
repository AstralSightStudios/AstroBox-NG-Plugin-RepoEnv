use astrobox_ng_wit::exports::astrobox::psys_plugin::{event_v3 as event, lifecycle};
use astrobox_ng_wit::FutureReader;

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
    fn on_event(event_type: event::EventType, event_payload: String) -> FutureReader<String> {
        let (writer, reader) = astrobox_ng_wit::wit_future::new::<String>(|| String::new());

        tracing::info!("event: type={:?}, payload={}", event_type, event_payload);

        match event_type {
            // 宿主拉源数据全靠这个回调，必须回结果，否则源永远不就绪。
            event::EventType::ProviderAction => provider::handle_action(&event_payload),
            event::EventType::Timer => ui::handle_timer_payload(&event_payload),
            _ => {}
        }

        astrobox_ng_wit::spawn(async move {
            let _ = writer.write(String::new()).await;
        });

        reader
    }

    fn on_ui_event_v3(
        event_id: String,
        event_type: event::Event,
        event_payload: String,
    ) -> FutureReader<String> {
        let (writer, reader) = astrobox_ng_wit::wit_future::new::<String>(|| String::new());

        tracing::info!(
            "ui event: id={}, type={:?}, payload={}",
            event_id,
            event_type,
            event_payload
        );
        ui::handle_ui_event(event_type, &event_id);

        astrobox_ng_wit::spawn(async move {
            let _ = writer.write(String::new()).await;
        });

        reader
    }

    fn on_ui_render(element_id: String) -> FutureReader<()> {
        let (writer, reader) = astrobox_ng_wit::wit_future::new::<()>(|| ());

        ui::render(&element_id);

        astrobox_ng_wit::spawn(async move {
            let _ = writer.write(()).await;
        });

        reader
    }

    fn on_card_render(card_id: String) -> FutureReader<()> {
        let (writer, reader) = astrobox_ng_wit::wit_future::new::<()>(|| ());

        tracing::info!("on_card_render: {}", card_id);

        astrobox_ng_wit::spawn(async move {
            let _ = writer.write(()).await;
        });

        reader
    }
}

impl lifecycle::Guest for RepoSourcePlugin {
    fn on_load() {
        logger::init();
        tracing::info!("ABRepo-TestEnv 社区源插件已加载");

        let provider_name = source::provider_name();
        tracing::info!("registering url provider: {}", provider_name);

        let registered_name = provider_name.clone();
        let result = astrobox_ng_wit::block_on(async move {
            astrobox_ng_wit::astrobox::psys_host::register::register_provider(
                &provider_name,
                astrobox_ng_wit::astrobox::psys_host::register::ProviderType::Url,
            )
            .await
        });

        let granted = result.is_ok();
        tracing::info!("register_provider result: {:?}", result);

        state::with_state(|state| {
            state.provider_name = registered_name;
            state.register_state = if granted {
                state::RegisterState::Granted
            } else {
                state::RegisterState::Denied
            };
        });
    }
}

astrobox_ng_wit::export!(RepoSourcePlugin);
