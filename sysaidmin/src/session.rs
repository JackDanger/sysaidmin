use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::task::Task;

#[derive(Clone)]
pub struct SessionStore {
    plan_path: PathBuf,
    log_path: PathBuf,
}

impl SessionStore {
    pub fn new(root: PathBuf) -> Result<Self> {
        let timestamp = Utc::now().format("%Y%m%d-%H%M%S");
        fs::create_dir_all(&root)
            .with_context(|| format!("failed to create session root {}", root.display()))?;
        let plan_path = root.join(format!("plan-{timestamp}.json"));
        let log_path = root.join(format!("session-{timestamp}.log"));
        Ok(Self {
            plan_path,
            log_path,
        })
    }

    pub fn write_plan(&self, summary: Option<&str>, tasks: &[Task]) -> Result<()> {
        let payload = PlanExport {
            summary: summary.map(|s| s.to_string()),
            generated_at: Utc::now(),
            tasks: tasks.to_vec(),
        };
        let data = serde_json::to_string_pretty(&payload)?;
        fs::write(&self.plan_path, data)
            .with_context(|| format!("failed writing {}", self.plan_path.display()))
    }

    pub fn append_log(&self, line: &str) -> Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)
            .with_context(|| format!("failed opening log {}", self.log_path.display()))?;
        writeln!(file, "[{}] {line}", Utc::now().to_rfc3339())?;
        Ok(())
    }
}

#[derive(Serialize)]
struct PlanExport {
    summary: Option<String>,
    generated_at: DateTime<Utc>,
    tasks: Vec<Task>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::{Task, TaskDetail};

    #[test]
    fn writes_plan_and_logs() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SessionStore::new(tmp.path().to_path_buf()).unwrap();
        let mut task = Task::new(
            "check disk",
            TaskDetail::Note {
                details: "note".into(),
            },
        );
        task.annotations.push("test".into());
        store.write_plan(Some("summary"), &[task]).unwrap();
        store.append_log("hello world").unwrap();
        let plan_files = fs::read_dir(tmp.path())
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .ok()
                    .and_then(|e| e.file_name().to_str().map(|name| name.contains("plan-")))
                    .unwrap_or(false)
            })
            .count();
        assert_eq!(plan_files, 1);
    }
}
