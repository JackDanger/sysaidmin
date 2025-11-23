use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow};

use crate::task::{CommandTask, FileEditTask};

#[derive(Clone)]
pub struct Executor {
    dry_run: bool,
}

pub struct ExecutionResult {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

pub struct FileEditOutcome {
    pub path: PathBuf,
    pub backup_path: Option<PathBuf>,
}

impl Executor {
    pub fn new(dry_run: bool) -> Self {
        Self { dry_run }
    }

    pub fn run_command(&self, task: &CommandTask) -> Result<ExecutionResult> {
        if self.dry_run {
            return Ok(ExecutionResult {
                status: 0,
                stdout: format!("(dry-run) command would execute: {}", task.command),
                stderr: String::new(),
            });
        }
        let mut cmd = Command::new(&task.shell);
        cmd.arg("-c").arg(&task.command);
        if let Some(cwd) = &task.cwd {
            cmd.current_dir(cwd);
        }
        let output = cmd
            .output()
            .with_context(|| format!("failed running shell command '{}'", task.command))?;
        let status = output.status.code().unwrap_or_default();
        Ok(ExecutionResult {
            status,
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        })
    }

    pub fn apply_file_edit(&self, edit: &FileEditTask) -> Result<FileEditOutcome> {
        let path_str = edit
            .path
            .as_ref()
            .ok_or_else(|| anyhow!("file edit missing path"))?;
        let path = PathBuf::from(path_str);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create parent dirs for {}", path.display()))?;
        }

        if self.dry_run {
            return Ok(FileEditOutcome {
                path,
                backup_path: None,
            });
        }

        let backup_path = self.create_backup_if_exists(&path)?;
        fs::write(&path, edit.new_text.as_bytes())
            .with_context(|| format!("failed writing {}", path.display()))?;

        Ok(FileEditOutcome { path, backup_path })
    }

    fn create_backup_if_exists(&self, path: &Path) -> Result<Option<PathBuf>> {
        if !path.exists() {
            return Ok(None);
        }
        let backup = path.with_extension("sysaidmin.bak");
        let contents =
            fs::read(path).with_context(|| format!("failed reading {}", path.display()))?;
        fs::write(&backup, contents)
            .with_context(|| format!("failed writing backup {}", backup.display()))?;
        Ok(Some(backup))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_echo_command() {
        let executor = Executor::new(false);
        let task = CommandTask {
            shell: "/bin/bash".into(),
            command: "echo hello-world".into(),
            cwd: None,
            requires_root: false,
        };
        let result = executor.run_command(&task).expect("command runs");
        assert!(result.stdout.contains("hello-world"));
    }

    #[test]
    fn writes_file_edits() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.conf");
        fs::write(&file, "old").unwrap();

        let executor = Executor::new(false);
        let task = FileEditTask {
            path: Some(file.to_string_lossy().to_string()),
            new_text: "new-content".into(),
            description: None,
        };
        let outcome = executor.apply_file_edit(&task).expect("write works");
        assert_eq!(fs::read_to_string(outcome.path).unwrap(), "new-content");
        assert!(outcome.backup_path.is_some());
    }

    #[test]
    fn dry_run_skips_side_effects() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("dry.conf");
        let executor = Executor::new(true);

        let cmd = CommandTask {
            shell: "/bin/bash".into(),
            command: "echo hi".into(),
            cwd: None,
            requires_root: false,
        };
        let result = executor.run_command(&cmd).expect("dry run command ok");
        assert!(result.stdout.contains("dry-run"));

        let edit = FileEditTask {
            path: Some(file.to_string_lossy().to_string()),
            new_text: "data".into(),
            description: None,
        };
        let outcome = executor.apply_file_edit(&edit).expect("dry run edit ok");
        assert!(outcome.backup_path.is_none());
        assert!(!outcome.path.exists());
    }
}
