// 通用工具函数

use std::collections::HashMap;

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

// HashMap 工具

/// 从 settings HashMap 中取值，不存在或为空则返回 default
pub fn setting_or(map: &HashMap<String, String>, key: &str, default: &str) -> String {
    map.get(key)
        .filter(|v| !v.is_empty())
        .cloned()
        .unwrap_or_else(|| default.to_string())
}

// ─── 机器指纹 ─────────────────────────────────────────────────

/// 基于机器指纹生成稳定唯一的 clientId
/// 采集 hostname + username + OS + arch，hash 后生成 16 位 hex 标识
pub fn generate_machine_client_id() -> String {
    use std::hash::{Hash, Hasher};

    let hostname = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .or_else(|_| {
            // macOS/Linux fallback
            std::process::Command::new("hostname")
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        })
        .unwrap_or_else(|_| "unknown-host".to_string());

    let username = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "unknown-user".to_string());

    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    let fingerprint = format!("{}|{}|{}|{}", hostname, username, os, arch);

    // 使用两轮不同种子的 hash 来生成 16 位 hex（128 bit 空间）
    let mut hasher1 = std::collections::hash_map::DefaultHasher::new();
    fingerprint.hash(&mut hasher1);
    let h1 = hasher1.finish();

    let mut hasher2 = std::collections::hash_map::DefaultHasher::new();
    format!("salt-v1-{}", fingerprint).hash(&mut hasher2);
    let h2 = hasher2.finish();

    let id = format!("atx-{:08x}{:08x}", h1 as u32, h2 as u32);
    eprintln!("[client_id] 机器指纹: {} → {}", fingerprint, id);
    id
}
