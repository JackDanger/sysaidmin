use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TaskStatus {
    Proposed,
    Ready,
    Blocked(String),
    Running,
    Complete,
}

impl TaskStatus {
    pub fn label(&self) -> &'static str {
        match self {
            TaskStatus::Proposed => "proposed",
            TaskStatus::Ready => "ready",
            TaskStatus::Blocked(_) => "blocked",
            TaskStatus::Running => "running",
            TaskStatus::Complete => "complete",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandTask {
    pub shell: String,
    pub command: String,
    pub cwd: Option<String>,
    pub requires_root: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEditTask {
    pub path: Option<String>,
    pub new_text: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TaskDetail {
    Command(CommandTask),
    FileEdit(FileEditTask),
    Note { details: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub description: String,
    pub detail: TaskDetail,
    pub status: TaskStatus,
    pub created_at: DateTime<Utc>,
    pub annotations: Vec<String>,
}

impl Task {
    pub fn new(description: impl Into<String>, detail: TaskDetail) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            description: description.into(),
            detail,
            status: TaskStatus::Proposed,
            created_at: Utc::now(),
            annotations: Vec::new(),
        }
    }

    pub fn status_text(&self) -> String {
        match &self.status {
            TaskStatus::Blocked(reason) => format!("blocked: {reason}"),
            _ => self.status.label().to_string(),
        }
    }
}
