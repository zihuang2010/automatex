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
    pub const UNAUTHORIZED: &str = "Unauthorized";
    pub const UNKNOWN: &str = "unknown";
}

/// 设备类型
pub mod device_type {
    pub const WIFI: &str = "wifi";
    pub const USB: &str = "usb";
}

/// Tauri 前后端事件名（emit / listen）
pub mod tauri_event {
    pub const DEVICES_CHANGED: &str = "devices-changed";
    pub const MQTT_STATUS: &str = "mqtt-status";
    pub const MQTT_DEVICE_KICK: &str = "mqtt-device-kick";
    pub const MQTT_TASK_RELOAD: &str = "mqtt-task-reload";
    pub const MQTT_PHONES_UNBIND: &str = "mqtt-phones-unbind";
    pub const MQTT_MESSAGE: &str = "mqtt-message";
    pub const TASK_UPDATE: &str = "task://update";
    pub const RISK_CONTROL: &str = "risk-control";
}

/// MQTT 状态推送到前端的字符串值
pub mod mqtt_emit_status {
    pub const CONNECTED: &str = "connected";
    pub const CONNECTING: &str = "connecting";
    pub const DISCONNECTED: &str = "disconnected";
}

/// 通用响应字面量
pub mod response {
    pub const OK: &str = "ok";
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
    pub const CRASHED: &str = "crashed";
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
    /// ADB shell/cmd 命令超时（防止永久阻塞）
    pub const ADB_COMMAND_TIMEOUT_SECS: u64 = 30;
    /// 设备列表缓存 TTL（毫秒）
    pub const DEVICE_CACHE_TTL_MS: u64 = 3000;
}

/// 并发限制
pub mod limits {
    /// 设备属性获取最大并发线程数
    pub const MAX_PROP_FETCH_THREADS: usize = 4;
    /// 电池刷新最大并发线程数
    pub const MAX_BATTERY_REFRESH_THREADS: usize = 8;
}

/// Mock / 调试开关
pub mod debug {
    /// 模拟风控是否启用（正式版设为 false）
    pub const MOCK_RISK_ENABLED: bool = false;
    /// 模拟风控触发概率
    pub const MOCK_RISK_PROBABILITY: f64 = 0.05;
    /// 心跳 publish 超时秒数
    pub const HEARTBEAT_PUBLISH_TIMEOUT_SECS: u64 = 5;
    /// emit_update 最小间隔毫秒（节流）
    pub const EMIT_THROTTLE_MS: u64 = 500;
}

/// 允许保存的设置键白名单
pub mod settings {
    pub const ALLOWED_KEYS: &[&str] = &[
        "mqtt_host",
        "mqtt_port",
        "mqtt_client_id",
        "mqtt_username",
        "mqtt_password",
        "mqtt_auto_connect",
        "api_base_url",
        "synced_phones",
        "theme",
    ];
}

/// MQTT Topic 常量
pub mod mqtt_topic {
    // ── 上行（客户端 → 服务端）──
    /// 设备上线事件
    pub const UP_DEVICE_ONLINE: &str = "upstream/device/online";
    /// 设备下线事件
    pub const UP_DEVICE_OFFLINE: &str = "upstream/device/offline";
    /// 心跳上报
    pub const UP_HEARTBEAT: &str = "upstream/heartbeat";
    /// 任务状态事件
    pub const UP_TASK_EVENT: &str = "upstream/task/event";

    // ── 下行（服务端 → 客户端）──
    /// 踢设备下线
    pub const DOWN_DEVICE_KICK: &str = "downstream/device/kick";
    /// 任务数据变更通知
    pub const DOWN_TASK_RELOAD: &str = "downstream/task/reload";
    /// 手机号被抢占/解绑通知
    pub const DOWN_PHONES_UNBIND: &str = "downstream/phones/unbind";
    /// 下行通配订阅
    pub const DOWN_WILDCARD: &str = "downstream/#";

    // ── 广播 ──
    /// 全局任务变更广播
    pub const BROADCAST_TASK_UPDATE: &str = "broadcast/task/update";
    /// 广播通配订阅
    pub const BROADCAST_WILDCARD: &str = "broadcast/#";

    // ── LWT 遗嘱 ──
    pub const UP_OFFLINE: &str = "upstream/offline";

    /// 拼接客户端专属 Topic: automatex/{client_id}/{suffix}
    pub fn client_topic(client_id: &str, suffix: &str) -> String {
        format!("automatex/{}/{}", client_id, suffix)
    }

    /// 拼接广播 Topic: automatex/{suffix}
    pub fn broadcast_topic(suffix: &str) -> String {
        format!("automatex/{}", suffix)
    }

    /// 心跳间隔（秒）
    pub const HEARTBEAT_INTERVAL_SECS: u64 = 30;
}

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
