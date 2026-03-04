//! MQTT 模块
//!
//! 模块化的 MQTT 客户端管理器：
//! - `types.rs` — MqttConfig、MqttStatus 数据结构
//! - `manager.rs` — MqttManager 真实连接管理器（rumqttc）
//! - `router.rs` — 消息路由（Topic → Tauri 事件）

mod manager;
mod router;
pub mod types;

pub use manager::MqttManager;
pub use types::{MqttConfig, MqttStatus};
