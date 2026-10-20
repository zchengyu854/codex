use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    Responses,
    ChatCompletions,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Coordinator,
    Worker,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Agent {
    pub id: String,
    pub session_id: String,
    pub role: Role,
    pub model: String,
    pub base_url: String,
    pub protocol: Protocol,
    pub context_bytes: usize,
}
impl Agent {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.model.is_empty() && self.model.len() <= 200,
            "model must contain 1..200 bytes"
        );
        ensure!(
            (4096..=1_000_000).contains(&self.context_bytes),
            "contextBytes must be 4096..1000000"
        );
        let url = reqwest::Url::parse(&self.base_url)?;
        ensure!(
            url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none(),
            "URL must not contain credentials, query, or fragment"
        );
        let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
        if url.scheme() != "https" && !(url.scheme() == "http" && local) {
            bail!("use HTTPS, or HTTP on loopback");
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub id: String,
    pub repository: PathBuf,
    pub base_commit: String,
    pub branch: String,
    pub integration_path: PathBuf,
    pub validation: Vec<Vec<String>>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskRequest {
    pub session_id: String,
    pub agent_id: String,
    pub goal: String,
    pub acceptance: String,
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub coding: bool,
    /// The task that created this one through `collab_create_task`; host-written, never client input.
    #[serde(default)]
    pub created_by: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub id: String,
    #[serde(flatten)]
    pub request: TaskRequest,
    pub status: String,
    pub context_version: i64,
    pub worktree: Option<PathBuf>,
    pub base_commit: Option<String>,
    pub commit: Option<String>,
    pub result: Option<String>,
    pub integration_status: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    pub revision: i64,
    pub session_id: String,
    pub agent_id: Option<String>,
    pub task_id: Option<String>,
    pub kind: String,
    pub data: Value,
    pub created_at: i64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Approval {
    pub id: String,
    pub session_id: String,
    pub agent_id: String,
    pub task_id: String,
    pub method: String,
    pub params: Value,
    pub decision: Option<bool>,
}
pub fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}
pub fn clip(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max.saturating_sub(16).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[truncated]", &text[..end])
}
