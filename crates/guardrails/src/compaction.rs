use crate::config::ContextCompactionConfig;
use crate::error::GuardrailError;
use crate::types::{CompactionResult, ContextBudget, GuardrailRequest};
use serde_json::{Value, json};
use std::sync::Arc;
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

        let mut body = Arc::clone(&request.body);
        Arc::make_mut(&mut body)["messages"] = Value::Array(compacted_messages);
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

pub trait CompactionStrategy: Send + Sync {
    fn compact(
        &self,
        messages: &[serde_json::Value],
        budget_tokens: usize,
        step_hint: &str,
    ) -> (Vec<serde_json::Value>, usize);
}

#[derive(Debug, Clone, Default)]
pub struct NoCompact;

impl CompactionStrategy for NoCompact {
    fn compact(
        &self,
        messages: &[serde_json::Value],
        _budget_tokens: usize,
        _step_hint: &str,
    ) -> (Vec<serde_json::Value>, usize) {
        (messages.to_vec(), 0)
    }
}

#[derive(Debug, Clone)]
pub struct SlidingWindowCompact {
    keep_recent: usize,
    compact_threshold: f32,
}

impl SlidingWindowCompact {
    pub fn new(keep_recent: usize, compact_threshold: f32) -> Self {
        Self {
            keep_recent,
            compact_threshold,
        }
    }
}

impl CompactionStrategy for SlidingWindowCompact {
    fn compact(
        &self,
        messages: &[serde_json::Value],
        budget_tokens: usize,
        _step_hint: &str,
    ) -> (Vec<serde_json::Value>, usize) {
        let estimated = estimate_value_messages_tokens(messages);
        let trigger = (budget_tokens as f32 * self.compact_threshold) as usize;
        if estimated < trigger || messages.len() <= 2 {
            return (messages.to_vec(), 0);
        }
        let keep = self.keep_recent.max(1);
        let split_at = messages.len().saturating_sub(keep);
        if split_at <= 2 {
            return (messages.to_vec(), 1);
        }
        let mut result = Vec::with_capacity(2 + keep);
        result.push(messages[0].clone());
        result.push(messages[1].clone());
        result.extend_from_slice(&messages[split_at..]);
        (result, 1)
    }
}

#[derive(Debug, Clone)]
pub struct TieredCompact {
    keep_recent: usize,
    phase_thresholds: (f32, f32, f32),
}

impl TieredCompact {
    pub fn new(keep_recent: usize, compact_threshold: f32) -> Self {
        Self {
            keep_recent,
            phase_thresholds: (compact_threshold, compact_threshold, compact_threshold),
        }
    }

    pub fn with_phase_thresholds(keep_recent: usize, phase_thresholds: (f32, f32, f32)) -> Self {
        Self {
            keep_recent,
            phase_thresholds,
        }
    }
}

impl CompactionStrategy for TieredCompact {
    fn compact(
        &self,
        messages: &[serde_json::Value],
        budget_tokens: usize,
        _step_hint: &str,
    ) -> (Vec<serde_json::Value>, usize) {
        let estimated = estimate_value_messages_tokens(messages);
        let t1 = (budget_tokens as f32 * self.phase_thresholds.0) as usize;
        let t2 = (budget_tokens as f32 * self.phase_thresholds.1) as usize;
        let t3 = (budget_tokens as f32 * self.phase_thresholds.2) as usize;

        if estimated < t1 {
            return (messages.to_vec(), 0);
        }

        // Phase 1: truncate tool results to ~200 chars
        let mut phase1 = messages.to_vec();
        for msg in phase1.iter_mut() {
            if msg.get("role").and_then(|v| v.as_str()) == Some("tool")
                && let Some(content) = msg.get_mut("content")
                && let Some(s) = content.as_str()
                && s.len() > 200
            {
                *content = serde_json::Value::String(format!("{}…", &s[..200]));
            }
        }
        let est1 = estimate_value_messages_tokens(&phase1);
        if est1 < t1 {
            return (phase1, 1);
        }

        // Phase 2: drop tool results entirely (except in keep_recent window)
        let keep = self.keep_recent.max(1);
        let split_at = phase1.len().saturating_sub(keep);
        // Сохраняем первые `header` сообщений (system/user-заголовок, максимум 2,
        // но не больше границы split_at), фильтруем середину, последние `keep` целиком.
        let header = split_at.min(2);
        let phase2: Vec<serde_json::Value> = phase1[..header]
            .iter()
            .chain(
                phase1[header..split_at]
                    .iter()
                    .filter(|m| m.get("role").and_then(|v| v.as_str()) != Some("tool")),
            )
            .chain(&phase1[split_at..])
            .cloned()
            .collect();
        let est2 = estimate_value_messages_tokens(&phase2);
        if est2 < t2 {
            return (phase2, 2);
        }

        // Phase 3: drop assistant reasoning (keep system + user + tool_call skeletons)
        let split_at3 = phase2.len().saturating_sub(keep);
        let header3 = split_at3.min(2);
        let phase3: Vec<serde_json::Value> = phase2[..header3]
            .iter()
            .chain(phase2[header3..split_at3].iter().filter(|m| {
                let role = m.get("role").and_then(|v| v.as_str());
                role != Some("assistant") && role != Some("tool")
            }))
            .chain(&phase2[split_at3..])
            .cloned()
            .collect();
        let est3 = estimate_value_messages_tokens(&phase3);
        if est3 < t3 || phase3.len() < phase2.len() {
            return (phase3, 3);
        }
        (phase3, 3)
    }
}

fn estimate_value_messages_tokens(messages: &[serde_json::Value]) -> usize {
    messages
        .iter()
        .map(|message| {
            message
                .get("content")
                .and_then(serde_json::Value::as_str)
                .map(|s| s.chars().count().div_ceil(4).max(1))
                .unwrap_or(1)
        })
        .sum()
}
