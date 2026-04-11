// 状态字面量统一管理
// 前后端共享同一套状态值，修改时只需改这里

/// 任务状态
pub mod task_status {
    pub const WAITING: &str = "waiting";
    pub const EXECUTING: &str = "executing";
    pub const PAUSED: &str = "paused";
    pub const SUCCESS: &str = "success";
    pub const ERROR: &str = "error";
}

/// 任务展示状态（前后端统一消费）
pub mod task_presentation_status {
    pub const READY: &str = "ready";
    pub const RUNNING: &str = "running";
    pub const WAITING_NEXT_ROUND: &str = "waiting_next_round";
    pub const PAUSED_MANUAL: &str = "paused_manual";
    pub const PAUSED_WAITING: &str = "paused_waiting";
    pub const ERROR_PAUSED: &str = "error_paused";
    pub const COMPLETED: &str = "completed";
}

/// 城市状态
pub mod city_status {
    pub const PENDING: &str = "pending";
    pub const ACTIVE: &str = "active";
    pub const DONE: &str = "done";
    /// 手机端发生不可恢复错误（如切换城市超时），本轮跳过该城市
    pub const ERROR: &str = "error";
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
    /// 通知前端需要绑定手机号（跳转到绑定页面）
    pub const REQUIRE_PHONE_BIND: &str = "require-phone-bind";
    /// 启动同步状态通知
    pub const STARTUP_SYNC_STATUS: &str = "startup-sync-status";
    /// 已同步手机号变更通知（payload: { phones: string[] }）
    pub const ACCOUNT_SYNC_CHANGED: &str = "account://sync-changed";
    /// scrcpy 会话状态变化
    pub const SCRCPY_SESSION_STATE: &str = "scrcpy-session-state";
    /// scrcpy 文本路由变化
    pub const SCRCPY_TEXT_ROUTE: &str = "scrcpy-text-route";
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
}

/// 轮次状态（a_task_rounds.status）
pub mod round_status {
    pub const RUNNING: &str = "running";
    pub const COMPLETED: &str = "completed";
    pub const STOPPED: &str = "stopped";
}

/// 手机无障碍 App TCP 通信配置
pub mod phone_client {
    /// 手机端无障碍 App 固定监听端口（每台设备相同）
    pub const PHONE_PORT_ON_DEVICE: u16 = 7899;
    /// PC 端端口分配起点（第一台设备使用此端口，依次递增）
    pub const PORT_BASE: u16 = 7899;
    /// 每次扫描任务的默认最大翻页数
    pub const DEFAULT_MAX_PAGES: u32 = 3;
    /// TCP 连接超时（秒）——超时即判定设备离线
    pub const CONNECT_TIMEOUT_SECS: u64 = 10;
    /// 健康检测（ping）超时（秒）——比正常连接超时短，快速判定
    pub const PING_TIMEOUT_SECS: u64 = 3;
}

/// 时间间隔常量（秒）
pub mod timing {
    /// 电池/温度定时刷新间隔
    pub const BATTERY_REFRESH_INTERVAL_SECS: u64 = 20;
    /// ADB track_devices 断开后重连等待
    pub const ADB_RECONNECT_WAIT_SECS: u64 = 5;
    /// WiFi 设备连接超时
    pub const WIFI_CONNECT_TIMEOUT_SECS: u64 = 5;
    /// MQTT keep-alive 间隔（10s：避免 NAT/防火墙 idle 超时，保持连接活跃）
    pub const MQTT_KEEP_ALIVE_SECS: u64 = 10;
    /// ADB shell/cmd 命令超时（防止永久阻塞）
    pub const ADB_COMMAND_TIMEOUT_SECS: u64 = 30;
    /// 设备列表缓存 TTL（毫秒）
    pub const DEVICE_CACHE_TTL_MS: u64 = 3000;
    /// 设备变化事件去抖时间（毫秒）
    pub const DEVICE_EVENT_DEBOUNCE_MS: u64 = 250;
    /// HTTP 连接超时
    pub const HTTP_CONNECT_TIMEOUT_SECS: u64 = 5;
    /// HTTP 请求总超时
    pub const HTTP_REQUEST_TIMEOUT_SECS: u64 = 15;
    /// HTTP 基础重试退避（毫秒）
    pub const HTTP_RETRY_BASE_DELAY_MS: u64 = 300;
    /// 启动期等待首轮设备就绪的最长时间
    pub const STARTUP_DEVICE_READY_TIMEOUT_SECS: u64 = 12;
    /// 任务默认调度节拍
    pub const TASK_DISPATCH_INTERVAL_SECS: u64 = 10;
    /// WiFi 设备重连初始退避（秒）
    pub const WIFI_RECONNECT_BASE_DELAY_SECS: u64 = 30;
    /// WiFi 设备重连最大退避（秒）
    pub const WIFI_RECONNECT_MAX_DELAY_SECS: u64 = 300;
}

/// 并发限制
pub mod limits {
    /// 设备属性获取最大并发线程数
    pub const MAX_PROP_FETCH_THREADS: usize = 4;
    /// 电池刷新最大并发线程数
    pub const MAX_BATTERY_REFRESH_THREADS: usize = 8;
    /// 单轮 WiFi 重连最大并发数
    pub const MAX_WIFI_RECONNECT_PER_CYCLE: usize = 3;
    /// 单次 batchTasks 请求的 taskId 数量上限
    pub const MAX_BATCH_TASK_IDS_PER_REQUEST: usize = 50;
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

/// DB 设置键名（a_settings.key）
pub mod setting_key {
    pub const MQTT_HOST: &str = "mqtt_host";
    pub const MQTT_PORT: &str = "mqtt_port";
    pub const MQTT_CLIENT_ID: &str = "mqtt_client_id";
    pub const MQTT_USERNAME: &str = "mqtt_username";
    pub const MQTT_PASSWORD: &str = "mqtt_password";
    pub const MQTT_AUTO_CONNECT: &str = "mqtt_auto_connect";
    pub const API_BASE_URL: &str = "api_base_url";
    pub const SYNCED_PHONES: &str = "synced_phones";
    pub const LAST_ACTIVE_DATE: &str = "last_active_date";
    pub const MOCK_SCENARIO: &str = "mock_scenario";
    pub const THEME: &str = "theme";
}

/// MQTT 连接默认值
/// 优先从环境变量读取，硬编码值仅作为开发阶段 fallback
pub mod mqtt_default {
    use std::sync::OnceLock;

    fn env_or(var: &str, fallback: &str) -> String {
        std::env::var(var).unwrap_or_else(|_| fallback.to_string())
    }

    /// 以下硬编码值仅用于开发环境，生产环境应通过环境变量 AUTOMATEX_MQTT_* 覆盖
    pub fn host() -> &'static str {
        static V: OnceLock<String> = OnceLock::new();
        V.get_or_init(|| env_or("AUTOMATEX_MQTT_HOST", "39.98.170.208"))
    }
    pub fn port() -> &'static str {
        static V: OnceLock<String> = OnceLock::new();
        V.get_or_init(|| env_or("AUTOMATEX_MQTT_PORT", "30002"))
    }
    pub fn port_num() -> u16 {
        port().parse().unwrap_or(30002)
    }
    pub fn username() -> &'static str {
        static V: OnceLock<String> = OnceLock::new();
        V.get_or_init(|| env_or("AUTOMATEX_MQTT_USERNAME", "automatex"))
    }
    pub fn password() -> &'static str {
        static V: OnceLock<String> = OnceLock::new();
        V.get_or_init(|| env_or("AUTOMATEX_MQTT_PASSWORD", "zihuang2010=-0"))
    }
    pub const FALLBACK_HOST: &str = "127.0.0.1";
}

/// 允许保存的设置键白名单
pub mod settings {
    use super::setting_key;
    pub const ALLOWED_KEYS: &[&str] = &[
        setting_key::MQTT_HOST,
        setting_key::MQTT_PORT,
        setting_key::MQTT_CLIENT_ID,
        setting_key::MQTT_USERNAME,
        setting_key::MQTT_PASSWORD,
        setting_key::MQTT_AUTO_CONNECT,
        setting_key::API_BASE_URL,
        setting_key::SYNCED_PHONES,
        setting_key::LAST_ACTIVE_DATE,
        setting_key::MOCK_SCENARIO,
        setting_key::THEME,
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

// 时间工具函数已迁移至 utils.rs，此处为兼容性 re-export
pub use crate::utils::format_datetime;
pub use crate::utils::now_unix;
pub use crate::utils::today_str;
