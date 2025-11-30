use std::time::Duration;

use anyhow::{Context, Result};
use log::{debug, error, info, trace, warn};
use reqwest::blocking::{Client, RequestBuilder};
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};

use crate::config::AppConfig;
use crate::tokenizer;

const SYS_PROMPT: &str = r#"
You are an LLM assistant for sysadmins debugging live, highly-available production servers.

CRITICAL: This is for PRODUCTION debugging. Safety is paramount. Your plans should be:
- Tight, clear, and focused on investigation and safe operations
- Conservative: prefer read-only commands and safe diagnostics
- Explicit: ask the user to manually run anything risky (writes, deletes, restarts, etc.)
- Informative: provide clear recommendations and next steps

All commands MUST be bash commands. No MCP or other tool invocations.

Always respond with ONLY JSON following this shape:
{
  "summary": "one line summary",
  "plan": [
    {
      "id": "task-1",
      "kind": "command" | "file_edit" | "note",
      "description": "short human description",
      "command": "bash command (if kind=command)",
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

For production debugging:
- Prefer "command" tasks for safe read-only operations (checking logs, status, configs)
- Use "note" tasks to recommend actions the user should perform manually
- Use "file_edit" ONLY for safe, well-understood configuration changes
- When in doubt, use a "note" task asking the user to run the command themselves

All commands will be logged to sysaidmin.history.sh. The user can paste output back for analysis.
"#;

const SYNTHESIS_PROMPT: &str = r#"
You are an LLM assistant helping sysadmins analyze server information and execution results from production debugging sessions.

When given execution results from commands or file operations, provide a clear, concise analysis focused on:

- Identifying key findings and patterns in the output
- Highlighting important information (errors, warnings, anomalies)
- Explaining what the results mean in the context of production debugging
- Suggesting safe next steps for investigation
- Recommending actions the user should perform manually (if risky)

Keep your analysis focused on production debugging: be clear, actionable, and safety-conscious.
Respond in plain text (not JSON). Be direct and informative.
"#;

#[derive(Clone)]
pub struct AnthropicClient {
    inner: ClientMode,
}

#[derive(Clone)]
enum ClientMode {
    Remote(RemoteClient),
    Offline,
}

#[derive(Clone)]
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
        let http = Client::builder()
            .default_headers(headers)
            .build()
            .context("Failed to build HTTP client")?;

        info!(
            "AnthropicClient created: api_url={}, model={}",
            config.api_url, config.model
        );
        Ok(Self {
            inner: ClientMode::Remote(RemoteClient {
                http,
                api_url: config.api_url.clone(),
                model: config.model.clone(),
            }),
        })
    }

    pub fn plan(
        &self,
        prompt: &str,
        history: &[crate::conversation::ConversationEntry],
    ) -> Result<String> {
        info!(
            "Requesting plan from API (prompt length: {} chars, history entries: {})",
            prompt.len(),
            history.len()
        );
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

    pub fn synthesize(
        &self,
        prompt: &str,
        history: &[crate::conversation::ConversationEntry],
    ) -> Result<String> {
        info!(
            "Requesting synthesis from API (prompt length: {} chars, history entries: {})",
            prompt.len(),
            history.len()
        );
        match &self.inner {
            ClientMode::Remote(remote) => {
                debug!("Using remote API client for synthesis");
                remote.synthesize(prompt, history)
            }
            ClientMode::Offline => {
                warn!("Using offline mock synthesis");
                Ok(format!(
                    "Mock analysis for: {}",
                    prompt.chars().take(100).collect::<String>()
                ))
            }
        }
    }
}

impl RemoteClient {
    fn plan(
        &self,
        prompt: &str,
        history: &[crate::conversation::ConversationEntry],
    ) -> Result<String> {
        trace!(
            "Building API request with {} history entries",
            history.len()
        );

        // Truncate history to fit within token budget
        // Anthropic API typically has limits around 200k tokens for context
        // Reserve space for system prompt, current prompt, and response
        const MAX_CONTEXT_TOKENS: usize = 180_000; // Conservative limit
        let system_tokens = tokenizer::approximate_tokens(SYS_PROMPT);
        let prompt_tokens = tokenizer::approximate_tokens(prompt);

        let truncated_history =
            tokenizer::truncate_history(history, MAX_CONTEXT_TOKENS, system_tokens, prompt_tokens);

        info!(
            "History: {} entries -> {} entries after truncation ({} -> {} tokens)",
            history.len(),
            truncated_history.len(),
            history.iter().map(tokenizer::entry_tokens).sum::<usize>(),
            truncated_history
                .iter()
                .map(tokenizer::entry_tokens)
                .sum::<usize>()
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
                crate::conversation::ConversationEntry::Plan {
                    response,
                    summary,
                    task_count,
                    ..
                } => {
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
                crate::conversation::ConversationEntry::Command {
                    description,
                    command,
                    exit_code,
                    stdout,
                    stderr,
                    ..
                } => {
                    // Include execution results as context
                    let mut context = format!(
                        "Executed: {} (command: {})\nExit code: {}",
                        description, command, exit_code
                    );
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
                crate::conversation::ConversationEntry::FileEdit {
                    description, path, ..
                } => {
                    let message_text = format!("[File edit completed] {}: {}", description, path);
                    messages.push(ChatMessage {
                        role: "user".to_string(),
                        content: vec![ContentBlock {
                            r#type: "text".to_string(),
                            text: message_text,
                        }],
                    });
                }
                crate::conversation::ConversationEntry::Note {
                    description,
                    details,
                    ..
                } => {
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

        // Use maximum tokens to avoid truncation - most Claude models support up to 16384
        // This ensures we get the complete response without artificial limits
        let request = MessageRequest {
            model: self.model.clone(),
            max_tokens: 16384, // Maximum for most Claude models - ensures complete responses
            system: SYS_PROMPT.to_string(),
            messages,
            temperature: Some(0.0),
        };

        info!("Sending POST request to {}", self.api_url);
        trace!("Request model: {}, max_tokens: {}", self.model, 16384);
        let resp = send_with_retry(
            || self.http.post(&self.api_url).json(&request),
            "plan request",
        )?;

        let status = resp.status();
        info!("Received response: status={}", status.as_u16());

        trace!("Reading complete response body");
        // Read the entire response body - resp.text() reads until EOF, ensuring we get everything
        let raw_body = resp
            .text()
            .context("failed to read Anthropic response body")?;
        debug!("Response body length: {} bytes", raw_body.len());

        // Verify we got a complete response (not empty)
        if raw_body.is_empty() {
            anyhow::bail!("Received empty response body from Anthropic API");
        }

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

        // Check if response was truncated due to max_tokens
        if let Some(ref stop_reason) = body.stop_reason
            && stop_reason == "max_tokens" {
                warn!(
                    "Response was truncated due to max_tokens limit. Consider increasing max_tokens or reducing prompt size."
                );
                anyhow::bail!(
                    "Response truncated: API stopped generating due to max_tokens limit. Increase max_tokens or reduce input size."
                );
            }

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

    fn synthesize(
        &self,
        prompt: &str,
        history: &[crate::conversation::ConversationEntry],
    ) -> Result<String> {
        trace!(
            "Building synthesis API request with {} history entries",
            history.len()
        );

        // Build conversation messages from history (same as plan)
        let mut messages = Vec::new();

        for entry in history {
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
                crate::conversation::ConversationEntry::Plan {
                    response,
                    summary,
                    task_count,
                    ..
                } => {
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
                crate::conversation::ConversationEntry::Command {
                    description,
                    command,
                    exit_code,
                    stdout,
                    stderr,
                    ..
                } => {
                    let mut context = format!(
                        "Executed: {} (command: {})\nExit code: {}",
                        description, command, exit_code
                    );
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
                crate::conversation::ConversationEntry::FileEdit {
                    description, path, ..
                } => {
                    let message_text = format!("[File edit completed] {}: {}", description, path);
                    messages.push(ChatMessage {
                        role: "user".to_string(),
                        content: vec![ContentBlock {
                            r#type: "text".to_string(),
                            text: message_text,
                        }],
                    });
                }
                crate::conversation::ConversationEntry::Note {
                    description,
                    details,
                    ..
                } => {
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

        // Add current synthesis prompt
        messages.push(ChatMessage {
            role: "user".to_string(),
            content: vec![ContentBlock {
                r#type: "text".to_string(),
                text: prompt.to_string(),
            }],
        });

        let request = MessageRequest {
            model: self.model.clone(),
            max_tokens: 2048, // More tokens for analysis
            system: SYNTHESIS_PROMPT.to_string(),
            messages,
            temperature: Some(0.3), // Slightly higher for more natural analysis
        };

        info!("Sending synthesis POST request to {}", self.api_url);
        trace!("Request model: {}, max_tokens: {}", self.model, 2048);
        let resp = send_with_retry(
            || self.http.post(&self.api_url).json(&request),
            "synthesis request",
        )?;

        let status = resp.status();
        let raw_body = resp
            .text()
            .context("failed to read synthesis response body")?;

        if !status.is_success() {
            let snippet: String = raw_body.chars().take(500).collect();
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

        let text = body
            .content
            .iter()
            .find_map(|block| {
                if block.r#type == "text" {
                    Some(block.text.as_str())
                } else {
                    None
                }
            })
            .unwrap_or("")
            .to_string();

        if text.is_empty() {
            error!("Response contained no text content");
            anyhow::bail!("Anthropic response did not include any text content");
        }

        info!(
            "Successfully extracted synthesis text ({} chars)",
            text.len()
        );
        Ok(text)
    }
}

/// Send HTTP request with retry logic for timeouts
/// Retries up to 3 times with exponential backoff: 1s, 2s, 4s
fn send_with_retry<F>(build_request: F, request_type: &str) -> Result<reqwest::blocking::Response>
where
    F: Fn() -> RequestBuilder,
{
    const MAX_RETRIES: u32 = 3;
    const INITIAL_DELAY_SECS: u64 = 1;

    for attempt in 0..=MAX_RETRIES {
        match build_request().send() {
            Ok(resp) => {
                if attempt > 0 {
                    info!(
                        "{} succeeded on retry attempt {}",
                        request_type, attempt
                    );
                }
                return Ok(resp);
            }
            Err(e) => {
                let is_timeout = e.is_timeout() || e.is_connect() || e.is_request();
                
                if is_timeout && attempt < MAX_RETRIES {
                    let delay_secs = INITIAL_DELAY_SECS * (1 << attempt);
                    warn!(
                        "{} timed out (attempt {}/{}), retrying in {}s...",
                        request_type,
                        attempt + 1,
                        MAX_RETRIES + 1,
                        delay_secs
                    );
                    std::thread::sleep(Duration::from_secs(delay_secs));
                    continue;
                } else {
                    // Not a timeout, or we've exhausted retries
                    return Err(e).context(format!(
                        "failed sending {} to Anthropic",
                        request_type
                    ));
                }
            }
        }
    }

    // Should never reach here, but handle it anyway
    Err(anyhow::anyhow!(
        "Failed to send {} after {} retries",
        request_type,
        MAX_RETRIES
    ))
    .context(format!("failed sending {} to Anthropic", request_type))
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
    #[serde(default)]
    stop_reason: Option<String>, // "end_turn", "max_tokens", "stop_sequence", etc.
}

#[derive(Deserialize)]
struct ResponseBlock {
    #[serde(rename = "type")]
    r#type: String,
    text: String,
}
