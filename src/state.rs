use std::sync::{OnceLock, RwLock};

use crate::catalog::Catalog;
use crate::source::IndexStats;

#[derive(Clone, Copy)]
pub enum RegisterState {
    Pending,
    Granted,
    Denied,
}

impl RegisterState {
    pub fn label(&self) -> &'static str {
        match self {
            RegisterState::Pending => "等待授权",
            RegisterState::Granted => "已注册",
            RegisterState::Denied => "被拒绝",
        }
    }
}

pub struct PluginState {
    pub root_element_id: Option<String>,
    pub provider_name: String,
    pub register_state: RegisterState,
    pub probing: bool,
    pub stats: Option<IndexStats>,
    pub last_checked_ms: u64,
    /// 宿主通过 provider-action `refresh` 拉下来的目录快照
    pub catalog: Option<Catalog>,
}

static STATE: OnceLock<RwLock<PluginState>> = OnceLock::new();

pub fn state() -> &'static RwLock<PluginState> {
    STATE.get_or_init(|| {
        RwLock::new(PluginState {
            root_element_id: None,
            provider_name: crate::source::provider_name(),
            register_state: RegisterState::Pending,
            probing: false,
            stats: None,
            last_checked_ms: 0,
            catalog: None,
        })
    })
}

pub fn with_state<T>(f: impl FnOnce(&mut PluginState) -> T) -> T {
    let mut guard = state().write().unwrap_or_else(|poisoned| poisoned.into_inner());
    f(&mut guard)
}

pub fn read_state<T>(f: impl FnOnce(&PluginState) -> T) -> T {
    let guard = state().read().unwrap_or_else(|poisoned| poisoned.into_inner());
    f(&guard)
}
