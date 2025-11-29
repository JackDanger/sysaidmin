use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConversationEntry {
    Prompt {
        timestamp: String,
        prompt: String,
    },
    Plan {
        timestamp: String,
        summary: Option<String>,
        task_count: usize,
    },
    Command {
        timestamp: String,
        task_id: String,
        description: String,
        command: String,
        shell: String,
        exit_code: i32,
        stdout: String,
        stderr: String,
    },
    FileEdit {
        timestamp: String,
        task_id: String,
        description: String,
        path: String,
        backup_path: Option<String>,
    },
    Note {
        timestamp: String,
        task_id: String,
        description: String,
        details: String,
    },
}

pub struct ConversationLogger {
    file: Arc<Mutex<File>>,
}

impl ConversationLogger {
    pub fn new(log_path: PathBuf) -> std::io::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)?;
        
        Ok(Self {
            file: Arc::new(Mutex::new(file)),
        })
    }

    pub fn log(&self, entry: ConversationEntry) -> std::io::Result<()> {
        let json = serde_json::to_string(&entry)?;
        if let Ok(mut file) = self.file.lock() {
            writeln!(file, "{}", json)?;
            file.flush()?;
        }
        Ok(())
    }
}

