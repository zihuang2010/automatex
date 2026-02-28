// 状态字面量统一管理
// 前后端共享同一套状态值，修改时只需改这里

/// 任务状态
pub mod task_status {
    pub const WAITING: &str = "WAITING";
    pub const EXECUTING: &str = "EXECUTING";
    pub const PAUSED: &str = "PAUSED";
    pub const SUCCESS: &str = "SUCCESS";
    pub const ERROR: &str = "ERROR";
}

/// 城市状态
pub mod city_status {
    pub const PENDING: &str = "pending";
    pub const ACTIVE: &str = "active";
    pub const DONE: &str = "done";
}

/// 关键词状态
pub mod keyword_status {
    pub const PENDING: &str = "pending";
    pub const RUN: &str = "run";
    pub const OK: &str = "ok";
}

/// 设备状态
pub mod device_state {
    pub const OFFLINE: &str = "Offline";
    pub const DEVICE: &str = "Device";
    pub const UNKNOWN: &str = "unknown";
}

/// 同步状态（a_task_progress.sync_status）
pub mod sync_status {
    pub const PENDING: &str = "pending";
    pub const SYNCED: &str = "synced";
}

/// 执行记录状态（a_task_runs.status）
pub mod run_status {
    pub const RUNNING: &str = "running";
    pub const COMPLETED: &str = "completed";
    pub const STOPPED: &str = "stopped";
    pub const PAUSED: &str = "paused";
}

/// 时间间隔常量（秒）
pub mod timing {
    /// 电池/温度定时刷新间隔
    pub const BATTERY_REFRESH_INTERVAL_SECS: u64 = 20;
    /// ADB track_devices 断开后重连等待
    pub const ADB_RECONNECT_WAIT_SECS: u64 = 5;
    /// WiFi 设备连接超时
    pub const WIFI_CONNECT_TIMEOUT_SECS: u64 = 5;
    /// MQTT keep-alive 间隔
    pub const MQTT_KEEP_ALIVE_SECS: u64 = 30;
}

/// 允许保存的设置键白名单
pub mod settings {
    pub const ALLOWED_KEYS: &[&str] =
        &["mqtt_host", "mqtt_port", "mqtt_client_id", "mqtt_username", "mqtt_password"];
}
