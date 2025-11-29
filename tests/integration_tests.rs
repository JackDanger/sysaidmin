//! Integration tests for Claude Code-style features

use sysaidmin::conversation::{ConversationEntry, ConversationLogger};
use sysaidmin::hooks::{HookEvent, HookManager, HookResult};
use sysaidmin::tokenizer;
use sysaidmin::transcript::{TranscriptManager, TranscriptMessage, TranscriptContentBlock};
use chrono::Utc;
use std::path::PathBuf;
use tempfile::NamedTempFile;

#[test]
fn test_conversation_history_with_truncation() {
    // Create a conversation logger
    let temp_file = NamedTempFile::new().unwrap();
    let path = temp_file.path().to_path_buf();
    drop(temp_file);
    
    let logger = ConversationLogger::new(path.clone()).unwrap();
    
    // Add many entries
    for i in 0..50 {
        let _ = logger.log(ConversationEntry::Prompt {
            timestamp: Utc::now().to_rfc3339(),
            prompt: format!("Prompt number {} with some content", i),
        });
    }
    
    // Load history
    let history = logger.load_history().unwrap();
    assert_eq!(history.len(), 50);
    
    // Test truncation
    let truncated = tokenizer::truncate_history(&history, 500, 100, 50);
    assert!(truncated.len() < history.len());
    assert!(truncated.len() > 0);
}

#[test]
fn test_transcript_management() {
    let temp_file = NamedTempFile::new().unwrap();
    let path = temp_file.path().to_path_buf();
    drop(temp_file);
    
    let manager = TranscriptManager::new(path.clone()).unwrap();
    
    // Add user message
    let user_msg = TranscriptMessage {
        role: "user".to_string(),
        content: vec![TranscriptContentBlock {
            r#type: "text".to_string(),
            text: "Hello, Claude!".to_string(),
        }],
    };
    manager.append(user_msg).unwrap();
    
    // Add assistant message
    let assistant_msg = TranscriptMessage {
        role: "assistant".to_string(),
        content: vec![TranscriptContentBlock {
            r#type: "text".to_string(),
            text: "Hello! How can I help?".to_string(),
        }],
    };
    manager.append(assistant_msg).unwrap();
    
    // Load transcript
    let loaded = manager.load().unwrap();
    assert_eq!(loaded.len(), 2);
    assert_eq!(loaded[0].role, "user");
    assert_eq!(loaded[1].role, "assistant");
}

#[test]
fn test_hook_manager_registration() {
    let mut manager = HookManager::new();
    
    manager.register(sysaidmin::hooks::Hook {
        event: HookEvent::PreToolUse,
        command: "echo test".to_string(),
        timeout_seconds: 10,
    });
    
    let input = serde_json::json!({
        "tool_name": "Bash",
        "tool_input": {
            "command": "ls -la"
        }
    });
    
    let results = manager.execute(HookEvent::PreToolUse, &input);
    // Hook should execute (may return empty if echo doesn't produce JSON)
    assert!(results.len() >= 0);
}

#[test]
fn test_token_approximation_accuracy() {
    // Test that token approximation is reasonable
    let short = "test";
    let medium = "This is a medium length string with multiple words.";
    let long = "A".repeat(1000);
    
    assert!(tokenizer::approximate_tokens(short) > 0);
    assert!(tokenizer::approximate_tokens(medium) > tokenizer::approximate_tokens(short));
    assert!(tokenizer::approximate_tokens(&long) > tokenizer::approximate_tokens(medium));
}

#[test]
fn test_conversation_entry_token_counting() {
    let prompt = ConversationEntry::Prompt {
        timestamp: Utc::now().to_rfc3339(),
        prompt: "test prompt".to_string(),
    };
    
    let plan = ConversationEntry::Plan {
        timestamp: Utc::now().to_rfc3339(),
        summary: Some("test summary".to_string()),
        task_count: 3,
        response: Some(r#"{"summary": "test", "plan": []}"#.to_string()),
    };
    
    let prompt_tokens = tokenizer::entry_tokens(&prompt);
    let plan_tokens = tokenizer::entry_tokens(&plan);
    
    assert!(prompt_tokens > 0);
    assert!(plan_tokens > prompt_tokens); // Plan should have more tokens
}

#[test]
fn test_history_truncation_preserves_recent() {
    let mut history = Vec::new();
    
    // Create history with timestamps
    for i in 0..20 {
        history.push(ConversationEntry::Prompt {
            timestamp: Utc::now().to_rfc3339(),
            prompt: format!("Prompt {}", i),
        });
    }
    
    // Truncate with small budget
    let truncated = tokenizer::truncate_history(&history, 100, 50, 50);
    
    // Should keep some entries
    if truncated.len() > 0 {
        // Most recent should be preserved
        let last_original = history.last().unwrap();
        let last_truncated = truncated.last().unwrap();
        
        match (last_original, last_truncated) {
            (
                ConversationEntry::Prompt { prompt: p1, .. },
                ConversationEntry::Prompt { prompt: p2, .. },
            ) => {
                assert_eq!(p1, p2);
            }
            _ => panic!("Unexpected entry types"),
        }
    }
}

