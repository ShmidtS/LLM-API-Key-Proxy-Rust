use crate::config::ContextCompactionConfig;
use crate::error::GuardrailError;
use crate::types::{CompactionResult, GuardrailRequest};
use serde_json::{Value, json};
use tracing::debug;

pub trait ContextCompactor: Send + Sync {
    fn compact(
        &self,
        request: &GuardrailRequest,
        config: &ContextCompactionConfig,
    ) -> Result<CompactionResult, GuardrailError>;
}

#[derive(Debug, Clone, Default)]
pub struct DefaultContextCompactor;

impl ContextCompactor for DefaultContextCompactor {
    fn compact(
        &self,
        request: &GuardrailRequest,
        config: &ContextCompactionConfig,
    ) -> Result<CompactionResult, GuardrailError> {
        if !config.enabled {
            return Ok(CompactionResult::Unchanged);
        }

        let Some(messages) = request.body.get("messages").and_then(Value::as_array) else {
            return Ok(CompactionResult::Unchanged);
        };

        let estimated_tokens = estimate_messages_tokens(messages);
        let usable_tokens = config
            .token_budget
            .max_context_tokens
            .saturating_sub(config.token_budget.reserve_output_tokens);
        let threshold = (usable_tokens as f32 * config.token_budget.compact_above_ratio) as usize;

        if estimated_tokens <= threshold || messages.len() <= config.min_messages_to_keep + 1 {
            return Ok(CompactionResult::Unchanged);
        }

        let keep = config.min_messages_to_keep.max(1);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{RouteKind, TokenBudget};

    fn request(messages: Vec<Value>) -> GuardrailRequest {
        GuardrailRequest {
            route: RouteKind::ChatCompletions,
            provider: "openai".into(),
            upstream_path: "/v1/chat/completions".into(),
            model: "gpt".into(),
            body: json!({"messages": messages}),
            stream: false,
            schema_hint: None,
            step_policy: None,
        }
    }

    #[test]
    fn leaves_disabled_config_unchanged() {
        let result = DefaultContextCompactor
            .compact(
                &request(vec![json!({"role":"user","content":"hi"})]),
                &ContextCompactionConfig::default(),
            )
            .unwrap();
        assert_eq!(result, CompactionResult::Unchanged);
    }

    #[test]
    fn compacts_when_threshold_exceeded() {
        let mut config = ContextCompactionConfig {
            enabled: true,
            token_budget: TokenBudget {
                max_context_tokens: 40,
                compact_above_ratio: 0.5,
                reserve_output_tokens: 0,
            },
            min_messages_to_keep: 2,
        };
        config.enabled = true;
        let messages = (0..8)
            .map(|i| json!({"role":"user","content": format!("message {i} with enough text to count")}))
            .collect();
        let result = DefaultContextCompactor
            .compact(&request(messages), &config)
            .unwrap();
        match result {
            CompactionResult::Compacted {
                removed_messages,
                body,
                ..
            } => {
                assert_eq!(removed_messages, 6);
                assert_eq!(body["messages"].as_array().unwrap().len(), 3);
            }
            CompactionResult::Unchanged => panic!("expected compaction"),
        }
    }
}
