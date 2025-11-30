use anyhow::{Result, anyhow};
use serde::Deserialize;

use crate::task::{CommandTask, FileEditTask, Task, TaskDetail};

#[derive(Debug)]
pub struct ParsedPlan {
    pub summary: Option<String>,
    pub tasks: Vec<Task>,
}

pub fn parse_plan(raw: &str, default_shell: &str) -> Result<ParsedPlan> {
    let cleaned = strip_code_fence(raw);
    let cleaned = cleaned.trim();
    let payload =
        extract_json_segment(&cleaned).or_else(|| extract_json_segment(&cleaned.replace('\n', "")));
    let segment = payload.unwrap_or_else(|| cleaned.to_string());

    let llm_plan: LlmPlan = serde_json::from_str(segment.trim()).map_err(|err| {
        // Check if this looks like a truncated response
        let is_truncated = err.to_string().contains("EOF") || 
                          segment.trim().ends_with(',') ||
                          !segment.contains('}') ||
                          (segment.matches('{').count() > segment.matches('}').count());
        
        let preview = cleaned
            .lines()
            .take(6)
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(500)
            .collect::<String>();
        
        let error_msg = if is_truncated {
            format!(
                "failed parsing plan JSON (response appears truncated - may need higher max_tokens): {err}. Snippet: {}",
                preview
            )
        } else {
            format!(
                "failed parsing plan JSON from SYSAIDMIN response: {err}. Snippet: {}",
                preview
            )
        };
        
        anyhow!(error_msg)
    })?;

    let mut tasks = Vec::new();
    for entry in llm_plan.plan {
        match entry.kind.as_deref().unwrap_or("note") {
            "command" => {
                let description = entry
                    .description
                    .clone()
                    .unwrap_or_else(|| "Command task".into());
                let command = entry
                    .command
                    .clone()
                    .ok_or_else(|| anyhow!("command task missing 'command' field"))?;
                let detail = TaskDetail::Command(CommandTask {
                    shell: entry
                        .shell
                        .clone()
                        .unwrap_or_else(|| default_shell.to_string()),
                    command,
                    cwd: entry.cwd.clone(),
                    requires_root: entry.requires_root.unwrap_or(false),
                });
                tasks.push(Task::new(description, detail));
            }
            "file_edit" => {
                let description = entry
                    .description
                    .clone()
                    .unwrap_or_else(|| "File edit".into());
                let path = entry.path.clone();
                let new_text = entry
                    .new_text
                    .clone()
                    .ok_or_else(|| anyhow!("file_edit task missing 'new_text'"))?;
                let detail = TaskDetail::FileEdit(FileEditTask {
                    path,
                    new_text,
                    description: entry.details.clone(),
                });
                tasks.push(Task::new(description, detail));
            }
            "note" | _ => {
                let details = entry
                    .details
                    .clone()
                    .or(entry.description.clone())
                    .unwrap_or_else(|| "Note".into());
                let detail = TaskDetail::Note {
                    details: details.clone(),
                };
                // Use details as description if description is missing or just "Note"
                let description = entry
                    .description
                    .filter(|d| d != "Note" && !d.is_empty())
                    .unwrap_or_else(|| {
                        // Use first line of details, truncated to 60 chars
                        details
                            .lines()
                            .next()
                            .map(|line| {
                                if line.len() > 60 {
                                    format!("{}…", &line[..60])
                                } else {
                                    line.to_string()
                                }
                            })
                            .unwrap_or_else(|| "Note".into())
                    });
                tasks.push(Task::new(description, detail));
            }
        }
    }

    if tasks.is_empty() {
        return Err(anyhow!("SYSAIDMIN response did not include any plan items"));
    }

    Ok(ParsedPlan {
        summary: llm_plan.summary,
        tasks,
    })
}

fn strip_code_fence(raw: &str) -> String {
    let trimmed = raw.trim();
    // Handle ```json\n{...}\n``` format
    if let Some(start) = trimmed.find("```json") {
        let after_start = &trimmed[start + 7..]; // Skip "```json"
        let after_start = after_start.trim_start();
        // Find the closing ```
        if let Some(end) = after_start.rfind("```") {
            return after_start[..end].trim().to_string();
        }
        // If no closing ```, just return everything after ```json
        return after_start.trim().to_string();
    }
    // Handle ```JSON (uppercase)
    if let Some(start) = trimmed.find("```JSON") {
        let after_start = &trimmed[start + 7..];
        let after_start = after_start.trim_start();
        if let Some(end) = after_start.rfind("```") {
            return after_start[..end].trim().to_string();
        }
        return after_start.trim().to_string();
    }
    // Handle plain ``` (generic code fence)
    if let Some(start) = trimmed.find("```") {
        let after_start = &trimmed[start + 3..];
        let after_start = after_start.trim_start();
        if let Some(end) = after_start.rfind("```") {
            return after_start[..end].trim().to_string();
        }
    }
    trimmed.to_string()
}

fn extract_json_segment(raw: &str) -> Option<String> {
    let mut depth = 0usize;
    let mut start_idx = None;
    for (idx, ch) in raw.char_indices() {
        match ch {
            '{' => {
                if depth == 0 {
                    start_idx = Some(idx);
                }
                depth += 1;
            }
            '}' => {
                if depth > 0 {
                    depth -= 1;
                    if depth == 0 {
                        if let Some(start) = start_idx {
                            return Some(raw[start..=idx].to_string());
                        }
                    }
                }
            }
            _ => {}
        }
    }
    // If we never found balanced braces, fall back to curly substring if exists
    if let Some(pos) = raw.find('{') {
        let slice = &raw[pos..];
        if slice.ends_with('}') {
            return Some(slice.to_string());
        }
    }
    None
}

#[derive(Debug, Deserialize)]
struct LlmPlan {
    summary: Option<String>,
    plan: Vec<LlmPlanItem>,
}

#[derive(Debug, Deserialize)]
struct LlmPlanItem {
    #[serde(rename = "id")]
    _id: Option<String>,
    kind: Option<String>,
    description: Option<String>,
    command: Option<String>,
    shell: Option<String>,
    requires_root: Option<bool>,
    cwd: Option<String>,
    path: Option<String>,
    new_text: Option<String>,
    details: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_plan() {
        let input = r#"{
            "summary": "Check disk pressure",
            "plan": [
                {
                    "id": "1",
                    "kind": "command",
                    "description": "Inspect disk usage",
                    "command": "df -h",
                    "shell": "/bin/bash"
                },
                {
                    "id": "2",
                    "kind": "note",
                    "description": "Review high usage partitions"
                }
            ]
        }"#;

        let parsed = parse_plan(input, "/bin/bash").expect("plan parses");
        assert_eq!(parsed.summary.unwrap(), "Check disk pressure");
        assert_eq!(parsed.tasks.len(), 2);
    }

    #[test]
    fn parses_code_fenced_plan() {
        let input = r#"```json
{
  "summary": "Demo",
  "plan": [
    {"kind": "note", "description": "hi"}
  ]
}
```"#;
        let parsed = parse_plan(input, "/bin/bash").expect("plan parses");
        assert_eq!(parsed.tasks.len(), 1);
    }

    #[test]
    fn extract_json_segment_handles_text_prefix() {
        let raw = "Model output:\n\n{\n  \"summary\": \"ok\",\n  \"plan\": []\n}\nThanks!";
        let segment = extract_json_segment(raw).expect("segment");
        assert!(segment.contains("\"summary\""));
    }
}
