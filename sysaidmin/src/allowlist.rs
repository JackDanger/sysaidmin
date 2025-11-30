use anyhow::{Result, anyhow};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::task::{Task, TaskDetail, TaskStatus};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AllowlistConfig {
    #[serde(default)]
    pub command_patterns: Vec<String>,
    #[serde(default)]
    pub file_patterns: Vec<String>,
    #[serde(default = "default_max_edit_kb")]
    pub max_edit_size_kb: usize,
}

fn default_max_edit_kb() -> usize {
    64
}

impl Default for AllowlistConfig {
    fn default() -> Self {
        Self {
            command_patterns: default_command_patterns()
                .iter()
                .map(|s| s.to_string())
                .collect(),
            file_patterns: default_file_patterns()
                .iter()
                .map(|s| s.to_string())
                .collect(),
            max_edit_size_kb: default_max_edit_kb(),
        }
    }
}

fn default_command_patterns() -> &'static [&'static str] {
    &[
        r"^(sudo\s+)?systemctl\s+",
        r"^(sudo\s+)?service\s+",
        r"^(sudo\s+)?journalctl(\s|$)",
        r"^tail\s+-f\s+",
        r"^tail\s+-n\s+\d+\s+",
        r"^head\s+-n\s+\d+\s+",
        r"^cat\s+",
        r"^less\s+",
        r"^grep\s+",
        r"^rg\s+",
        r"^(sudo\s+)?apt(-get)?\s+",
        r"^(sudo\s+)?dpkg\s+",
        r"^ls(\s|$)",
        r"^pwd$",
        r"^whoami$",
        r"^id$",
        r"^df\s+",
        r"^du\s+",
        r"^mount(\s|$)",
        r"^umount(\s|$)",
        r"^ip\s+",
        r"^ifconfig",
        r"^netstat",
        r"^ss\s+",
        r"^(sudo\s+)?ufw\s+",
        r"^(sudo\s+)?iptables\s+",
        r"^curl\s+",
        r"^wget\s+",
        r"^dig\s+",
        r"^host\s+",
        r"^ping\s+",
        r"^traceroute\s+",
        r"^top$",
        r"^htop$",
        r"^ps\s+",
        r"^(sudo\s+)?kill",
        r"^journalctl",
        r"^(sudo\s+)?systemd-analyze",
    ]
}

fn default_file_patterns() -> &'static [&'static str] {
    &[
        r"^/etc/.*",
        r"^/var/log/.*",
        r"^/usr/lib/systemd/system/.*",
        r"^/lib/systemd/system/.*",
        r"^/etc/ssh/.*",
        r"^/etc/network/.*",
        r"^/etc/sysctl\.conf$",
    ]
}

#[derive(Debug, Clone)]
pub struct Allowlist {
    command_regexes: Vec<Regex>,
    file_regexes: Vec<Regex>,
    max_edit_size_kb: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum AllowlistError {
    #[error("command '{0}' is not allowlisted")]
    CommandDenied(String),
    #[error("file '{0}' is not allowlisted")]
    FileDenied(String),
    #[error("edit for '{0}' exceeds {1} KiB limit")]
    EditTooLarge(String, usize),
}

impl Allowlist {
    pub fn from_config(cfg: AllowlistConfig) -> Result<Self> {
        let command_regexes = cfg
            .command_patterns
            .iter()
            .map(|pat| {
                Regex::new(pat).map_err(|err| anyhow!("invalid command regex '{}': {err}", pat))
            })
            .collect::<Result<Vec<_>>>()?;
        let file_regexes = cfg
            .file_patterns
            .iter()
            .map(|pat| {
                Regex::new(pat).map_err(|err| anyhow!("invalid file regex '{}': {err}", pat))
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(Self {
            command_regexes,
            file_regexes,
            max_edit_size_kb: cfg.max_edit_size_kb,
        })
    }

    pub fn evaluate(&self, task: &Task) -> Result<TaskStatus, AllowlistError> {
        match &task.detail {
            TaskDetail::Command(cmd) => {
                if self
                    .command_regexes
                    .iter()
                    .any(|re| re.is_match(&cmd.command))
                {
                    Ok(TaskStatus::Ready)
                } else {
                    Err(AllowlistError::CommandDenied(cmd.command.clone()))
                }
            }
            TaskDetail::FileEdit(edit) => {
                if let Some(path) = &edit.path {
                    let matches_path = self.file_regexes.iter().any(|re| re.is_match(path));
                    if !matches_path {
                        return Err(AllowlistError::FileDenied(path.clone()));
                    }
                }
                let size_kb = edit.new_text.len() / 1024;
                if size_kb > self.max_edit_size_kb {
                    return Err(AllowlistError::EditTooLarge(
                        edit.path.clone().unwrap_or_else(|| "<buffer>".into()),
                        self.max_edit_size_kb,
                    ));
                }
                Ok(TaskStatus::Ready)
            }
            TaskDetail::Note { .. } => Ok(TaskStatus::Ready),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::{CommandTask, Task, TaskDetail};

    fn make_task(cmd: &str) -> Task {
        Task::new(
            "test",
            TaskDetail::Command(CommandTask {
                shell: "/bin/bash".into(),
                command: cmd.into(),
                cwd: None,
                requires_root: false,
            }),
        )
    }

    #[test]
    fn denies_unlisted_command() {
        let cfg = AllowlistConfig {
            command_patterns: vec![r"^ls".into()],
            file_patterns: vec![],
            max_edit_size_kb: 64,
        };
        let allowlist = Allowlist::from_config(cfg).unwrap();
        let task = make_task("rm -rf /tmp/foo");
        let result = allowlist.evaluate(&task);
        assert!(matches!(result, Err(AllowlistError::CommandDenied(_))));
    }

    #[test]
    fn allows_matching_command() {
        let cfg = AllowlistConfig {
            command_patterns: vec![r"^ls".into()],
            file_patterns: vec![],
            max_edit_size_kb: 64,
        };
        let allowlist = Allowlist::from_config(cfg).unwrap();
        let task = make_task("ls -la /var");
        let result = allowlist.evaluate(&task).unwrap();
        assert!(matches!(result, TaskStatus::Ready));
    }
}
