// 通用工具函数

use chrono::TimeZone;
use std::collections::HashMap;
use tracing::info;

// 时间工具
/// 当前 Unix 时间戳（秒）
pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// 今日日期字符串 (YYYY-MM-DD)
pub fn today_str() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

/// 将 Unix 时间戳（秒）格式化为 `yyyy-MM-dd HH:mm:ss`（本地时区）
pub fn format_datetime(unix_secs: i64) -> String {
    chrono::Local
        .timestamp_opt(unix_secs, 0)
        .single()
        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_default()
}

// HashMap 工具

/// 从 settings HashMap 中取值，不存在或为空则返回 default
pub fn setting_or(map: &HashMap<String, String>, key: &str, default: &str) -> String {
    map.get(key)
        .filter(|v| !v.is_empty())
        .cloned()
        .unwrap_or_else(|| default.to_string())
}

// ─── 机器指纹 ─────────────────────────────────────────────────

/// 基于 UUID v4 生成全局唯一的 clientId
/// 旧版使用机器指纹哈希，存在碰撞风险（特别是 VM/Docker 环境）
/// 现改用 UUID v4 确保唯一性，机器指纹仅用于日志标识
pub fn generate_machine_client_id() -> String {
    let id = format!("atx-{}", uuid::Uuid::new_v4().as_simple());
    // 截取前 20 字符保持 client_id 可读性
    let short_id = &id[..20.min(id.len())];

    // 机器指纹仅用于日志（方便排查同一台机器的多个实例）
    let hostname = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .or_else(|_| {
            std::process::Command::new("hostname")
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        })
        .unwrap_or_else(|_| "unknown-host".to_string());
    let username = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "unknown-user".to_string());

    info!(client_id = short_id, hostname = %hostname, username = %username, "生成新 ID");
    short_id.to_string()
}
