//! Scrcpy 投屏模块
//!
//! - `control.rs` — 控制协议（触控、按键、文本注入）
//! - `video.rs` — H.264 视频流读取
//! - `server.rs` — scrcpy-server 生命周期管理
//! - `session.rs` — 投屏会话管理

pub mod control;
pub mod server;
pub mod session;
pub mod video;
