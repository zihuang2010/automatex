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
