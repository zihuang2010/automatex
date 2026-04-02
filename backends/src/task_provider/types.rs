use serde::{Deserialize, Serialize};

// ─── 任务数据结构（前端交互用）────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskKeyword {
    pub name: String,
    pub status: String, // "pending" | "run" | "ok"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskCity {
    pub name: String,
    pub poi: String,
    pub progress: i32, // 0-100
    pub total: i32,
    pub done: i32,
    pub status: String, // "pending" | "active" | "done"
    pub keywords: Vec<TaskKeyword>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub name: String,
    pub status: String, // "waiting" | "executing" | "paused" | "success" | "error"
    pub runtime_status: Option<String>,
    pub presentation_status: String,
    pub assigned_device: Option<String>,
    pub cities: Vec<TaskCity>,
    /// 轮次间隔（分钟），>0 表示一轮完成后等待 N 分钟再开始下一轮
    pub interval_minute: Option<i32>,
    /// 当天第几轮（0 = 尚未启动过）
    pub round_no: i32,
    /// 当前轮次 ID（用于内部关联，前端可忽略）
    pub current_round_id: Option<i64>,
    /// 当前执行城市（用于精确恢复，前端可忽略）
    pub current_city_name: Option<String>,
    /// 当前执行关键词（用于精确恢复，前端可忽略）
    pub current_keyword_name: Option<String>,
    /// 下一轮预计开始时间（Unix 时间戳）
    pub next_round_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSummary {
    pub id: String,
    pub name: String,
    pub status: String,
    pub runtime_status: Option<String>,
    pub presentation_status: String,
    pub assigned_device: Option<String>,
    pub city_count: i32,
    pub keyword_total: i32,
    pub keyword_done: i32,
    pub progress: i32,
    pub active_city_name: Option<String>,
    pub active_city_progress: Option<i32>,
    pub active_city_done: Option<i32>,
    pub active_city_total: Option<i32>,
    pub current_city_name: Option<String>,
    pub current_keyword_name: Option<String>,
    /// 轮次间隔（分钟）
    pub interval_minute: Option<i32>,
    pub round_no: i32,
    /// 下一轮预计开始时间（Unix 时间戳），前端展示倒计时
    pub next_round_at: Option<i64>,
}

// ─── 任务定义格式（Mock / HTTP 共用）───────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TaskDef {
    pub id: String,
    pub name: String,
    #[serde(default, alias = "intervalMinute")]
    pub interval_minute: Option<i32>,
    pub cities: Vec<CityDef>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CityDef {
    pub name: String,
    pub poi: String,
    pub keywords: Vec<String>,
}
