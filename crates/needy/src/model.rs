use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub name: String,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_run_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Run {
    pub id: String,
    pub session_id: String,
    pub state: String,
    pub created_at: String,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
}

impl Run {
    pub fn terminal(&self) -> bool {
        matches!(
            self.state.as_str(),
            "succeeded" | "failed" | "cancelled" | "lost"
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Event {
    pub id: u64,
    pub run_id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub data: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_line: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StartRun {
    pub command: Vec<String>,
    #[serde(default, alias = "input")]
    pub stdin: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Input {
    pub command: Vec<String>,
    #[serde(default, alias = "input")]
    pub stdin: String,
}

pub fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}
pub fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}
