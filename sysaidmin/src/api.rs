use anyhow::{Context, Result};
use log::{debug, error, info, trace, warn};
use reqwest::blocking::Client;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};

use crate::config::AppConfig;

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

    pub fn plan(&self, prompt: &str) -> Result<String> {
        info!("Requesting plan from API (prompt length: {} chars)", prompt.len());
        match &self.inner {
            ClientMode::Remote(remote) => {
                debug!("Using remote API client");
                remote.plan(prompt)
            }
            ClientMode::Offline => {
                warn!("Using offline mock plan");
                Ok(mock_plan(prompt))
            }
        }
    }
}

impl RemoteClient {
    fn plan(&self, prompt: &str) -> Result<String> {
        trace!("Building API request");
        let request = MessageRequest {
            model: &self.model,
            max_tokens: 1024,
            system: SYS_PROMPT,
            messages: vec![ChatMessage {
                role: "user",
                content: vec![ContentBlock {
                    r#type: "text",
                    text: prompt,
                }],
            }],
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
struct MessageRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    system: &'a str,
    messages: Vec<ChatMessage<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
}

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: Vec<ContentBlock<'a>>,
}

#[derive(Serialize)]
struct ContentBlock<'a> {
    #[serde(rename = "type")]
    r#type: &'a str,
    text: &'a str,
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
