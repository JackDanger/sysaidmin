use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Serialize, Deserialize)]
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
        #[serde(skip_serializing_if = "Option::is_none")]
        response: Option<String>, // Full JSON response for context
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
    path: PathBuf,
}

impl ConversationLogger {
    pub fn new(log_path: PathBuf) -> std::io::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)?;

        Ok(Self {
            file: Arc::new(Mutex::new(file)),
            path: log_path,
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

    pub fn load_history(&self) -> std::io::Result<Vec<ConversationEntry>> {
        Self::load_history_from_path(&self.path)
    }

    pub fn load_history_from_path(path: &PathBuf) -> std::io::Result<Vec<ConversationEntry>> {
        // If file doesn't exist, return empty history
        if !path.exists() {
            return Ok(vec![]);
        }

        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let mut entries = Vec::new();

        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<ConversationEntry>(&line) {
                Ok(entry) => entries.push(entry),
                Err(e) => {
                    // Log but don't fail - corrupted lines shouldn't break everything
                    eprintln!("Failed to parse conversation entry: {} - {}", e, line);
                }
            }
        }

        Ok(entries)
    }
}
