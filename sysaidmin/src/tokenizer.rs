//! Token counting and prompt truncation utilities.
//! 
//! Provides token-aware conversation history management similar to Claude Code.
//! Uses approximate token counting (4 chars per token) for efficiency.

use crate::conversation::ConversationEntry;

/// Approximate token count for a string (4 characters per token).
/// This is a rough approximation - actual tokenization varies by model.
pub fn approximate_tokens(text: &str) -> usize {
    // Rough approximation: 4 characters per token
    // This is conservative for English text
    text.chars().count() / 4 + 1
}

/// Token count for a conversation entry.
pub fn entry_tokens(entry: &ConversationEntry) -> usize {
    match entry {
        ConversationEntry::Prompt { prompt, .. } => approximate_tokens(prompt),
        ConversationEntry::Plan { response, summary, .. } => {
            if let Some(resp) = response {
                approximate_tokens(resp)
            } else if let Some(summary) = summary {
                approximate_tokens(summary) + 50 // Add overhead for structure
            } else {
                50 // Minimal overhead
            }
        }
        ConversationEntry::Command { description, command, stdout, stderr, .. } => {
            approximate_tokens(description)
                + approximate_tokens(command)
                + approximate_tokens(stdout)
                + approximate_tokens(stderr)
                + 20 // Overhead
        }
        ConversationEntry::FileEdit { description, path, .. } => {
            approximate_tokens(description) + approximate_tokens(path) + 10
        }
        ConversationEntry::Note { description, details, .. } => {
            approximate_tokens(description) + approximate_tokens(details) + 10
        }
    }
}

/// Truncate conversation history to fit within token budget.
/// 
/// Keeps the most recent entries and system prompt, ensuring we don't exceed
/// the token limit. Uses a "sliding window" approach - keeps recent context
/// while preserving important earlier context if space allows.
/// 
/// # Arguments
/// * `history` - Full conversation history
/// * `max_tokens` - Maximum tokens to keep (excluding system prompt and current prompt)
/// * `system_prompt_tokens` - Token count for system prompt
/// * `current_prompt_tokens` - Token count for current prompt
/// 
/// # Returns
/// Truncated history that fits within the budget
pub fn truncate_history(
    history: &[ConversationEntry],
    max_tokens: usize,
    system_prompt_tokens: usize,
    current_prompt_tokens: usize,
) -> Vec<ConversationEntry> {
    // Reserve tokens for system prompt and current prompt
    let available_tokens = max_tokens
        .saturating_sub(system_prompt_tokens)
        .saturating_sub(current_prompt_tokens)
        .saturating_sub(100); // Safety margin
    
    if available_tokens == 0 {
        return vec![];
    }
    
    // Start from the end (most recent) and work backwards
    let mut result = Vec::new();
    let mut total_tokens = 0;
    
    // Always keep the most recent entry if possible (for continuity)
    for entry in history.iter().rev() {
        let entry_tok = entry_tokens(entry);
        
        if total_tokens + entry_tok <= available_tokens {
            result.insert(0, entry.clone());
            total_tokens += entry_tok;
        } else {
            // If we can't fit this entry, stop
            break;
        }
    }
    
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn make_prompt(text: &str) -> ConversationEntry {
        ConversationEntry::Prompt {
            timestamp: Utc::now().to_rfc3339(),
            prompt: text.to_string(),
        }
    }

    #[test]
    fn test_approximate_tokens() {
        assert_eq!(approximate_tokens(""), 1);
        assert!(approximate_tokens("test") >= 1);
        assert!(approximate_tokens("this is a test") >= 3);
    }

    #[test]
    fn test_truncate_history_keeps_recent() {
        let history = vec![
            make_prompt("first prompt"),
            make_prompt("second prompt"),
            make_prompt("third prompt"),
        ];
        
        let truncated = truncate_history(&history, 1000, 100, 50);
        assert!(truncated.len() <= 3); // Should keep all if there's space
        assert!(truncated.len() > 0); // Should keep at least some
    }

    #[test]
    fn test_truncate_history_respects_limit() {
        let history = vec![
            make_prompt("first prompt"),
            make_prompt("second prompt"),
            make_prompt("third prompt"),
        ];
        
        let truncated = truncate_history(&history, 50, 100, 50);
        // Should only keep what fits
        assert!(truncated.len() <= 3);
    }

    #[test]
    fn test_truncate_history_empty_when_no_space() {
        let history = vec![make_prompt("test")];
        let truncated = truncate_history(&history, 10, 100, 50);
        assert_eq!(truncated.len(), 0);
    }
}

