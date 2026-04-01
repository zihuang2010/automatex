use serde::{Deserialize, Deserializer, Serialize};

fn deserialize_vec_or_null<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Option::<Vec<T>>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Debug, Deserialize)]
pub struct ApiEnvelope<T> {
    pub code: i32,
    pub msg: String,
    pub data: Option<T>,
    #[serde(rename = "serviceCode")]
    pub service_code: Option<i64>,
}

#[derive(Debug, Deserialize, Default)]
pub struct ApiResponse {}

#[derive(Debug, Serialize)]
pub struct PhoneBindRequest {
    #[serde(rename = "clientId")]
    pub client_id: String,
    #[serde(rename = "mobiles")]
    pub phones: Vec<String>,
    #[serde(rename = "forceBind")]
    pub force: bool,
}

#[derive(Debug, Deserialize, Serialize, Default)]
pub struct PhoneBindResponse {
    #[serde(default, deserialize_with = "deserialize_vec_or_null")]
    pub conflicts: Vec<PhoneConflict>,
    #[serde(rename = "taskItems", default, deserialize_with = "deserialize_vec_or_null")]
    pub task_items: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct PhoneConflict {
    #[serde(rename = "mobile")]
    pub phone: String,
    #[serde(rename = "clientId")]
    pub client_id: String,
}

#[derive(Debug, Serialize)]
pub struct BatchTasksRequest {
    #[serde(rename = "taskIds")]
    pub task_ids: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BatchTaskItem {
    #[serde(rename = "taskId")]
    pub task_id: String,
    #[serde(rename = "taskName")]
    pub task_name: String,
    #[serde(rename = "intervalMinute", default)]
    pub interval_minute: Option<i32>,
    pub mobile: String,
    #[serde(rename = "cityItems", default)]
    pub city_items: Vec<BatchTaskCityItem>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BatchTaskCityItem {
    #[serde(rename = "cityName")]
    pub city_name: String,
    #[serde(rename = "pointName", alias = "poiName")]
    pub point_name: String,
    #[serde(default)]
    pub keywords: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct UnbindPhonesRequest {
    #[serde(rename = "clientId")]
    pub client_id: String,
    #[serde(rename = "mobiles")]
    pub phones: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ProgressReportRequest {
    #[serde(rename = "clientId")]
    pub client_id: String,
    #[serde(rename = "taskId")]
    pub task_id: String,
    #[serde(rename = "taskName")]
    pub task_name: String,
    #[serde(rename = "cityName")]
    pub city_name: String,
    #[serde(rename = "keyword")]
    pub keyword_name: String,
    #[serde(rename = "deviceNo")]
    pub device_serial: String,
    #[serde(rename = "roundNo")]
    pub round_no: i32,
    #[serde(rename = "storeList")]
    pub store_list: Vec<String>,
    #[serde(rename = "scanFinishedTime")]
    pub completed_at: i64,
}
