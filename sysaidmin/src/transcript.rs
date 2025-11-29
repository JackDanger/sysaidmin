//! Transcript management for conversation history.
//! 
//! Maintains a JSONL transcript file similar to Claude Code's transcript format,
//! with proper role/content structure for API compatibility.

use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Transcript message following Claude API format
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptMessage {
    pub role: String, // "user" or "assistant"
    pub content: Vec<TranscriptContentBlock>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptContentBlock {
    #[serde(rename = "type")]
    pub r#type: String, // "text"
    pub text: String,
}

/// Transcript manager that maintains a JSONL transcript file
pub struct TranscriptManager {
    file: Arc<Mutex<File>>,
    path: PathBuf,
}

impl TranscriptManager {
    pub fn new(transcript_path: PathBuf) -> std::io::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&transcript_path)?;
        
        Ok(Self {
            file: Arc::new(Mutex::new(file)),
            path: transcript_path,
        })
    }

    /// Append a message to the transcript
    pub fn append(&self, message: TranscriptMessage) -> std::io::Result<()> {
        let json = serde_json::to_string(&message)?;
        if let Ok(mut file) = self.file.lock() {
            writeln!(file, "{}", json)?;
            file.flush()?;
        }
        Ok(())
    }

    /// Load all messages from transcript
    pub fn load(&self) -> std::io::Result<Vec<TranscriptMessage>> {
        Self::load_from_path(&self.path)
    }

    /// Load messages from a transcript file
    pub fn load_from_path(path: &PathBuf) -> std::io::Result<Vec<TranscriptMessage>> {
        if !path.exists() {
            return Ok(vec![]);
        }
        
        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let mut messages = Vec::new();
        
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<TranscriptMessage>(&line) {
                Ok(msg) => messages.push(msg),
                Err(e) => {
                    eprintln!("Failed to parse transcript message: {} - {}", e, line);
                }
            }
        }
        
        Ok(messages)
    }

    /// Get the transcript path
    pub fn path(&self) -> &PathBuf {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn test_transcript_append_and_load() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().to_path_buf();
        drop(temp_file); // Close file so manager can open it
        
        let manager = TranscriptManager::new(path.clone()).unwrap();
        
        let message = TranscriptMessage {
            role: "user".to_string(),
            content: vec![TranscriptContentBlock {
                r#type: "text".to_string(),
                text: "test message".to_string(),
            }],
        };
        
        manager.append(message.clone()).unwrap();
        
        let loaded = manager.load().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].role, "user");
        assert_eq!(loaded[0].content[0].text, "test message");
    }

    #[test]
    fn test_transcript_load_empty() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().to_path_buf();
        drop(temp_file);
        
        let manager = TranscriptManager::new(path).unwrap();
        let loaded = manager.load().unwrap();
        assert_eq!(loaded.len(), 0);
    }
}

