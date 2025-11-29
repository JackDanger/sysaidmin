use anyhow::{Context, Result};
use log::{debug, error, info, trace, warn};
use reqwest::blocking::Client;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};

use crate::config::AppConfig;
use crate::tokenizer;

const SYS_PROMPT: &str = r#"
You are an LLM for sysadmins to when fixing their servers. Produce structured JSON that captures a
worklist of shell commands, configuration edits, or investigative notes.
Always respond with ONLY JSON following this shape:
{
  "summary": "one line summary",
  "plan": [
    {
      "id": "task-1",
      "kind": "command" | "file_edit" | "note",
      "description": "short human description",
      "command": "shell command (if kind=command)",
      "shell": "/bin/bash",
      "requires_root": true | false,
      "cwd": "/etc",
      "path": "/etc/ssh/sshd_config",
      "new_text": "replacement text for file edits",
      "details": "extra info for notes"
    }
  ]
}
Never include markdown code fences or commentary outside JSON.
Keep shells POSIX compatible and focus on investigative/sysadmin workflows.
"#;

pub struct AnthropicClient {
    inner: ClientMode,
}

enum ClientMode {
    Remote(RemoteClient),
    Offline,
}

struct RemoteClient {
    http: Client,
    api_url: String,
    model: String,
}

impl AnthropicClient {
    pub fn new(config: &AppConfig) -> Result<Self> {
        info!("Creating AnthropicClient");
        if config.offline_mode {
            warn!("Running in offline mode - API calls will be mocked");
            return Ok(Self {
                inner: ClientMode::Offline,
            });
        }

        trace!("Building HTTP client with API key");
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-api-key",
            HeaderValue::from_str(&config.api_key).context("invalid API key header")?,
        );
        headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        let http = Client::builder().default_headers(headers).build()
            .context("Failed to build HTTP client")?;

        info!("AnthropicClient created: api_url={}, model={}", config.api_url, config.model);
        Ok(Self {
            inner: ClientMode::Remote(RemoteClient {
                http,
                api_url: config.api_url.clone(),
                model: config.model.clone(),
            }),
        })
    }

    pub fn plan(&self, prompt: &str, history: &[crate::conversation::ConversationEntry]) -> Result<String> {
        info!("Requesting plan from API (prompt length: {} chars, history entries: {})", prompt.len(), history.len());
        match &self.inner {
            ClientMode::Remote(remote) => {
                debug!("Using remote API client");
                remote.plan(prompt, history)
            }
            ClientMode::Offline => {
                warn!("Using offline mock plan");
                Ok(mock_plan(prompt))
            }
        }
    }
}

impl RemoteClient {
    fn plan(&self, prompt: &str, history: &[crate::conversation::ConversationEntry]) -> Result<String> {
        trace!("Building API request with {} history entries", history.len());
        
        // Truncate history to fit within token budget
        // Anthropic API typically has limits around 200k tokens for context
        // Reserve space for system prompt, current prompt, and response
        const MAX_CONTEXT_TOKENS: usize = 180_000; // Conservative limit
        let system_tokens = tokenizer::approximate_tokens(SYS_PROMPT);
        let prompt_tokens = tokenizer::approximate_tokens(prompt);
        
        let truncated_history = tokenizer::truncate_history(
            history,
            MAX_CONTEXT_TOKENS,
            system_tokens,
            prompt_tokens,
        );
        
        info!(
            "History: {} entries -> {} entries after truncation ({} -> {} tokens)",
            history.len(),
            truncated_history.len(),
            history.iter().map(tokenizer::entry_tokens).sum::<usize>(),
            truncated_history.iter().map(tokenizer::entry_tokens).sum::<usize>()
        );
        
        // Build conversation messages from truncated history
        let mut messages = Vec::new();
        
        for entry in &truncated_history {
            match entry {
                crate::conversation::ConversationEntry::Prompt { prompt: p, .. } => {
                    messages.push(ChatMessage {
                        role: "user".to_string(),
                        content: vec![ContentBlock {
                            r#type: "text".to_string(),
                            text: p.clone(),
                        }],
                    });
                }
                crate::conversation::ConversationEntry::Plan { response, summary, task_count, .. } => {
                    // Use full response if available, otherwise construct summary
                    let plan_text = if let Some(resp) = response {
                        resp.clone()
                    } else if let Some(summary) = summary {
                        format!("Plan with {} tasks: {}", task_count, summary)
                    } else {
                        format!("Plan with {} tasks", task_count)
                    };
                    messages.push(ChatMessage {
                        role: "assistant".to_string(),
                        content: vec![ContentBlock {
                            r#type: "text".to_string(),
                            text: plan_text,
                        }],
                    });
                }
                crate::conversation::ConversationEntry::Command { description, command, exit_code, stdout, stderr, .. } => {
                    // Include execution results as context
                    let mut context = format!("Executed: {} (command: {})\nExit code: {}", description, command, exit_code);
                    if !stdout.trim().is_empty() {
                        context.push_str(&format!("\nSTDOUT:\n{}", stdout));
                    }
                    if !stderr.trim().is_empty() {
                        context.push_str(&format!("\nSTDERR:\n{}", stderr));
                    }
                    let message_text = format!("[Execution result] {}", context);
                    messages.push(ChatMessage {
                        role: "user".to_string(),
                        content: vec![ContentBlock {
                            r#type: "text".to_string(),
                            text: message_text,
                        }],
                    });
                }
                crate::conversation::ConversationEntry::FileEdit { description, path, .. } => {
                    let message_text = format!("[File edit completed] {}: {}", description, path);
                    messages.push(ChatMessage {
                        role: "user".to_string(),
                        content: vec![ContentBlock {
                            r#type: "text".to_string(),
                            text: message_text,
                        }],
                    });
                }
                crate::conversation::ConversationEntry::Note { description, details, .. } => {
                    let message_text = format!("[Note] {}: {}", description, details);
                    messages.push(ChatMessage {
                        role: "user".to_string(),
                        content: vec![ContentBlock {
                            r#type: "text".to_string(),
                            text: message_text,
                        }],
                    });
                }
            }
        }
        
        // Add current prompt
        messages.push(ChatMessage {
            role: "user".to_string(),
            content: vec![ContentBlock {
                r#type: "text".to_string(),
                text: prompt.to_string(),
            }],
        });
        
        let request = MessageRequest {
            model: self.model.clone(),
            max_tokens: 1024,
            system: SYS_PROMPT.to_string(),
            messages,
            temperature: Some(0.0),
        };

        info!("Sending POST request to {}", self.api_url);
        trace!("Request model: {}, max_tokens: {}", self.model, 1024);
        let resp = self
            .http
            .post(&self.api_url)
            .json(&request)
            .send()
            .context("failed sending request to Anthropic")?;

        let status = resp.status();
        info!("Received response: status={}", status.as_u16());
        
        trace!("Reading response body");
        let raw_body = resp
            .text()
            .context("failed to read Anthropic response body")?;
        debug!("Response body length: {} bytes", raw_body.len());

        if !status.is_success() {
            error!("API request failed with status {}", status.as_u16());
            let snippet = if raw_body.is_empty() {
                "no response body".to_string()
            } else {
                raw_body
                    .lines()
                    .take(3)
                    .collect::<Vec<_>>()
                    .join(" ")
                    .chars()
                    .take(500)
                    .collect()
            };
            error!("Error response snippet: {}", snippet);
            return Err(anyhow::anyhow!(
                "Anthropic API {}: {}",
                status.as_u16(),
                snippet
            ));
        }

        trace!("Parsing JSON response");
        let body: MessageResponse =
            serde_json::from_str(&raw_body).context("failed to decode Anthropic response body")?;
        
        trace!("Extracting text content from response");
        let text = body
            .content
            .iter()
            .filter_map(|block| {
                if block.r#type == "text" {
                    Some(block.text.trim().to_string())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        
        if text.is_empty() {
            error!("Response contained no text content");
            anyhow::bail!("Anthropic response did not include any text content");
        }
        
        info!("Successfully extracted plan text ({} chars)", text.len());
        Ok(text)
    }
}

fn mock_plan(prompt: &str) -> String {
    let escaped = prompt.replace('"', "'");
    format!(
        r#"{{
  "summary": "offline mock response for '{escaped}'",
  "plan": [
    {{
      "id": "task-1",
      "kind": "command",
      "description": "Inspect recent auth log entries",
      "command": "sudo tail -n 100 /var/log/auth.log",
      "shell": "/bin/bash",
      "requires_root": true,
      "cwd": "/"
    }},
    {{
      "id": "task-2",
      "kind": "note",
      "description": "Review output manually",
      "details": "Look for repeated failures or lockouts"
    }}
  ]
}}"#
    )
}

#[derive(Serialize)]
struct MessageRequest {
    model: String,
    max_tokens: u32,
    system: String,
    messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
}

#[derive(Serialize)]
struct ChatMessage {
    role: String,
    content: Vec<ContentBlock>,
}

#[derive(Serialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    r#type: String,
    text: String,
}

#[derive(Deserialize)]
struct MessageResponse {
    content: Vec<ResponseBlock>,
}

#[derive(Deserialize)]
struct ResponseBlock {
    #[serde(rename = "type")]
    r#type: String,
    text: String,
}
