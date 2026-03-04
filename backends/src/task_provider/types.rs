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
    pub status: String, // "WAITING" | "EXECUTING" | "PAUSED" | "SUCCESS" | "ERROR"
    pub assigned_device: Option<String>,
    pub cities: Vec<TaskCity>,
    /// 当天第几轮（0 = 尚未启动过）
    pub round_no: i32,
    /// 当前轮次 ID（用于内部关联，前端可忽略）
    pub current_round_id: Option<i64>,
}

// ─── 任务定义格式（Mock / HTTP 共用）───────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TaskDef {
    pub id: String,
    pub name: String,
    pub cities: Vec<CityDef>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CityDef {
    pub name: String,
    pub poi: String,
    pub keywords: Vec<String>,
}
