use crate::config::ContextCompactionConfig;
use crate::error::GuardrailError;
use crate::types::{CompactionResult, ContextBudget, GuardrailRequest};
use serde_json::{Value, json};
use tracing::debug;

pub trait ContextCompactor: Send + Sync {
    fn compact(
        &self,
        request: &GuardrailRequest,
        budget: &ContextBudget,
    ) -> Result<CompactionResult, GuardrailError>;
}

#[derive(Debug, Clone, Default)]
pub struct DefaultContextCompactor;

impl DefaultContextCompactor {
    pub fn compact_with_config(
        &self,
        request: &GuardrailRequest,
        config: &ContextCompactionConfig,
    ) -> Result<CompactionResult, GuardrailError> {
        if !config.enabled {
            return Ok(CompactionResult::Unchanged);
        }
        self.compact(request, &config.budget())
    }
}

impl ContextCompactor for DefaultContextCompactor {
    fn compact(
        &self,
        request: &GuardrailRequest,
        budget: &ContextBudget,
    ) -> Result<CompactionResult, GuardrailError> {
        let Some(messages) = request.body.get("messages").and_then(Value::as_array) else {
            return Ok(CompactionResult::Unchanged);
        };

        let estimated_tokens = estimate_messages_tokens(messages);
        let usable_tokens = budget
            .max_context_tokens
            .saturating_sub(budget.reserve_output_tokens);
        let threshold = (usable_tokens as f32 * budget.compact_above_ratio) as usize;

        if estimated_tokens <= threshold || messages.len() <= budget.min_recent_messages + 1 {
            return Ok(CompactionResult::Unchanged);
        }

        let keep = budget.min_recent_messages.max(1);
        let split_at = messages.len().saturating_sub(keep);
        let removed = &messages[..split_at];
        let kept = &messages[split_at..];
        let summary_text = summarize_messages(removed);
        let summary_message = json!({
            "role": "system",
            "content": summary_text,
        });

        let mut compacted_messages = Vec::with_capacity(kept.len() + 1);
        compacted_messages.push(summary_message.clone());
        compacted_messages.extend(kept.iter().cloned());

        let mut body = request.body.clone();
        body["messages"] = Value::Array(compacted_messages);
        debug!(
            removed_messages = removed.len(),
            "compacted guardrail request context"
        );

        Ok(CompactionResult::Compacted {
            body,
            summary_message,
            removed_messages: removed.len(),
        })
    }
}

fn estimate_messages_tokens(messages: &[Value]) -> usize {
    messages
        .iter()
        .map(|message| message.to_string().chars().count().div_ceil(4).max(1))
        .sum()
}

fn summarize_messages(messages: &[Value]) -> String {
    let mut summary =
        String::from("Summary of earlier conversation removed for context compaction:\n");
    for message in messages {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let content = message_content_text(message);
        if content.is_empty() {
            summary.push_str(&format!("- {role}: [non-text content]\n"));
        } else {
            let clipped = clip_chars(&content, 180);
            summary.push_str(&format!("- {role}: {clipped}\n"));
        }
    }
    summary
}

fn message_content_text(message: &Value) -> String {
    match message.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| {
                part.get("text")
                    .and_then(Value::as_str)
                    .or_else(|| part.get("content").and_then(Value::as_str))
            })
            .collect::<Vec<_>>()
            .join(" "),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

fn clip_chars(input: &str, max: usize) -> String {
    let mut chars = input.chars();
    let clipped = chars.by_ref().take(max).collect::<String>();
    if chars.next().is_some() {
        format!("{clipped}…")
    } else {
        clipped
    }
}
