//! Hook system for intercepting and modifying behavior.
//!
//! Similar to Claude Code's hook system, allowing plugins or configuration
//! to intercept events like tool usage, prompt submission, etc.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Hook event types matching Claude Code's hook system
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HookEvent {
    /// Before a tool is executed
    PreToolUse,
    /// After a tool is executed
    PostToolUse,
    /// When user submits a prompt
    UserPromptSubmit,
    /// When Claude wants to stop/complete
    Stop,
}

/// Hook execution result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookResult {
    /// System message to inject into conversation
    pub system_message: Option<String>,
    /// Whether to block the operation (for PreToolUse)
    pub block: bool,
    /// Hook-specific output
    pub hook_specific_output: Option<serde_json::Value>,
}

impl Default for HookResult {
    fn default() -> Self {
        Self {
            system_message: None,
            block: false,
            hook_specific_output: None,
        }
    }
}

/// Hook definition
#[derive(Debug, Clone)]
pub struct Hook {
    pub event: HookEvent,
    pub command: String,
    pub timeout_seconds: u64,
}

/// Hook manager that executes hooks for events
pub struct HookManager {
    hooks: HashMap<HookEvent, Vec<Hook>>,
}

impl HookManager {
    pub fn new() -> Self {
        Self {
            hooks: HashMap::new(),
        }
    }

    /// Load hooks from configuration file
    pub fn load_from_file(_path: &PathBuf) -> std::io::Result<Self> {
        // For now, return empty manager
        // TODO: Implement hook loading from JSON config
        Ok(Self::new())
    }

    /// Register a hook
    pub fn register(&mut self, hook: Hook) {
        self.hooks
            .entry(hook.event)
            .or_insert_with(Vec::new)
            .push(hook);
    }

    /// Execute hooks for an event
    pub fn execute(&self, event: HookEvent, input_data: &serde_json::Value) -> Vec<HookResult> {
        let hooks = self.hooks.get(&event).cloned().unwrap_or_default();
        let mut results = Vec::new();

        for hook in hooks {
            match self.execute_hook(&hook, input_data) {
                Ok(result) => results.push(result),
                Err(e) => {
                    eprintln!("Hook execution error: {}", e);
                    // Continue with other hooks
                }
            }
        }

        results
    }

    fn execute_hook(
        &self,
        hook: &Hook,
        input_data: &serde_json::Value,
    ) -> Result<HookResult, String> {
        // Serialize input data to JSON
        let input_json = serde_json::to_string(input_data)
            .map_err(|e| format!("Failed to serialize hook input: {}", e))?;

        // Execute hook command
        let output = Command::new("sh")
            .arg("-c")
            .arg(&hook.command)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("HOOK_INPUT", &input_json)
            .output()
            .map_err(|e| format!("Failed to execute hook: {}", e))?;

        // Parse output as JSON
        let output_str = String::from_utf8_lossy(&output.stdout);
        if output_str.trim().is_empty() {
            return Ok(HookResult::default());
        }

        // Try to parse as HookResult JSON
        match serde_json::from_str::<HookResult>(output_str.trim()) {
            Ok(result) => Ok(result),
            Err(_) => {
                // If not JSON, treat as system message
                Ok(HookResult {
                    system_message: Some(output_str.trim().to_string()),
                    block: false,
                    hook_specific_output: None,
                })
            }
        }
    }
}

impl Default for HookManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hook_manager_creation() {
        let manager = HookManager::new();
        assert!(manager.hooks.is_empty());
    }

    #[test]
    fn test_hook_registration() {
        let mut manager = HookManager::new();
        manager.register(Hook {
            event: HookEvent::PreToolUse,
            command: "echo test".to_string(),
            timeout_seconds: 10,
        });

        assert_eq!(manager.hooks.get(&HookEvent::PreToolUse).unwrap().len(), 1);
    }

    #[test]
    fn test_hook_execution_empty() {
        let manager = HookManager::new();
        let input = serde_json::json!({});
        let results = manager.execute(HookEvent::PreToolUse, &input);
        assert_eq!(results.len(), 0);
    }
}
