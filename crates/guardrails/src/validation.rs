use crate::error::GuardrailError;
use crate::types::{
    GuardrailRequest, SchemaHint, ValidationIssue, ValidationOptions, ValidationReport,
};
use models::chat::{ChatCompletionResponse, ChatMessage, ChatMessageContent, ToolCall};
use serde_json::Value;

pub trait ResponseValidator: Send + Sync {
    fn validate(
        &self,
        request: &GuardrailRequest,
        response: &Value,
        options: &ValidationOptions,
    ) -> Result<ValidationReport, GuardrailError>;
}

#[derive(Debug, Clone, Default)]
pub struct DefaultResponseValidator;

impl ResponseValidator for DefaultResponseValidator {
    fn validate(
        &self,
        request: &GuardrailRequest,
        response: &Value,
        options: &ValidationOptions,
    ) -> Result<ValidationReport, GuardrailError> {
        let mut violations = Vec::new();

        let chat_response = serde_json::from_value::<ChatCompletionResponse>(response.clone()).ok();

        if options.validate_tool_calls || matches!(request.schema_hint, Some(SchemaHint::ToolCalls))
        {
            match &chat_response {
                Some(parsed) => validate_tool_calls(parsed, &mut violations),
                None => violations.push(issue(
                    "response",
                    "response is not a valid chat completion response",
                    "error",
                )),
            }
        }

        if options.validate_json_mode || matches!(request.schema_hint, Some(SchemaHint::JsonMode)) {
            match &chat_response {
                Some(parsed) => validate_json_mode(parsed, &mut violations),
                None => validate_raw_json_mode(response, &mut violations),
            }
        }

        if let Some(SchemaHint::JsonSchema(schema)) = &request.schema_hint {
            validate_json_schema_hint(response, schema, &mut violations);
        } else if options.validate_schema {
            validate_raw_json_mode(response, &mut violations);
        }

        if let Some(SchemaHint::StepCompletion(required, before)) = &request.schema_hint {
            validate_step_completion(response, required, before, &mut violations);
        }

        if options.validate_steps {
            if let Some(policy) = &request.step_policy {
                validate_step_completion(
                    response,
                    &policy.required_steps,
                    &policy.before_steps,
                    &mut violations,
                );
            } else if !matches!(request.schema_hint, Some(SchemaHint::StepCompletion(_, _))) {
                validate_step_completion(response, &[], &[], &mut violations);
            }
        } else if let Some(policy) = &request.step_policy {
            validate_step_completion(
                response,
                &policy.required_steps,
                &policy.before_steps,
                &mut violations,
            );
        }

        Ok(ValidationReport {
            ok: violations.is_empty(),
            violations,
        })
    }
}

fn validate_tool_calls(response: &ChatCompletionResponse, violations: &mut Vec<ValidationIssue>) {
    for (choice_index, choice) in response.choices.iter().enumerate() {
        if let Some(tool_calls) = &choice.message.tool_calls {
            for (tool_index, tool_call) in tool_calls.iter().enumerate() {
                validate_tool_call(choice_index, tool_index, tool_call, violations);
            }
        }
    }
}

fn validate_tool_call(
    choice_index: usize,
    tool_index: usize,
    tool_call: &ToolCall,
    violations: &mut Vec<ValidationIssue>,
) {
    let prefix = format!("choices.{choice_index}.message.tool_calls.{tool_index}");
    if tool_call.id.trim().is_empty() {
        violations.push(issue(
            format!("{prefix}.id"),
            "tool call id is required",
            "error",
        ));
    }
    if tool_call.r#type != "function" {
        violations.push(issue(
            format!("{prefix}.type"),
            "tool call type must be function",
            "error",
        ));
    }
    if tool_call.function.name.trim().is_empty() {
        violations.push(issue(
            format!("{prefix}.function.name"),
            "tool function name is required",
            "error",
        ));
    }
    if serde_json::from_str::<Value>(&tool_call.function.arguments).is_err() {
        violations.push(issue(
            format!("{prefix}.function.arguments"),
            "tool function arguments must be valid JSON",
            "error",
        ));
    }
}

fn validate_json_mode(response: &ChatCompletionResponse, violations: &mut Vec<ValidationIssue>) {
    for (choice_index, choice) in response.choices.iter().enumerate() {
        let Some(content) = message_text(&choice.message) else {
            if choice
                .message
                .tool_calls
                .as_ref()
                .is_some_and(|tool_calls| !tool_calls.is_empty())
            {
                continue;
            }
            violations.push(issue(
                format!("choices.{choice_index}.message.content"),
                "JSON mode response content is missing",
                "error",
            ));
            continue;
        };

        if serde_json::from_str::<Value>(&content).is_err() {
            violations.push(issue(
                format!("choices.{choice_index}.message.content"),
                "JSON mode response content must be valid JSON",
                "error",
            ));
        }
    }
}

fn validate_raw_json_mode(response: &Value, violations: &mut Vec<ValidationIssue>) {
    if !response.is_object() && !response.is_array() {
        violations.push(issue("response", "response must be valid JSON", "error"));
    }
}

fn validate_json_schema_hint(
    response: &Value,
    schema: &Value,
    violations: &mut Vec<ValidationIssue>,
) {
    let extracted = extract_response_json(response);
    let target = extracted.as_ref().unwrap_or(response);

    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        for field in required.iter().filter_map(Value::as_str) {
            if target.get(field).is_none() {
                violations.push(issue(
                    field,
                    "response did not include required schema field",
                    "error",
                ));
            }
        }
    }

    if let Some(expected_type) = schema.get("type").and_then(Value::as_str) {
        let type_matches = match expected_type {
            "object" => target.is_object(),
            "array" => target.is_array(),
            "string" => target.is_string(),
            "number" => target.is_number(),
            "boolean" => target.is_boolean(),
            "null" => target.is_null(),
            _ => true,
        };
        if !type_matches {
            violations.push(issue(
                "response",
                "response did not match schema type",
                "error",
            ));
        }
    }
}

fn validate_step_completion(
    response: &Value,
    required_steps: &[String],
    before_steps: &[String],
    violations: &mut Vec<ValidationIssue>,
) {
    let text = response_text(response).to_lowercase();
    for step in required_steps {
        if !text.contains(&step.to_lowercase()) {
            violations.push(issue(
                "steps",
                format!("required step `{step}` was not completed"),
                "error",
            ));
        }
    }

    if before_steps.len() >= 2 {
        for pair in before_steps.windows(2) {
            let first = pair[0].to_lowercase();
            let second = pair[1].to_lowercase();
            let first_pos = text.find(&first);
            let second_pos = text.find(&second);
            if let (Some(first_pos), Some(second_pos)) = (first_pos, second_pos)
                && first_pos > second_pos
            {
                violations.push(issue(
                    "steps",
                    format!("step `{}` must be completed before `{}`", pair[0], pair[1]),
                    "error",
                ));
            }
        }
    }
}

fn extract_response_json(response: &Value) -> Option<Value> {
    response
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
}

fn response_text(response: &Value) -> String {
    if let Ok(chat_response) = serde_json::from_value::<ChatCompletionResponse>(response.clone()) {
        chat_response
            .choices
            .iter()
            .filter_map(|choice| message_text(&choice.message))
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        response.to_string()
    }
}

fn message_text(message: &ChatMessage) -> Option<String> {
    match &message.content {
        Some(ChatMessageContent::Text(text)) => Some(text.clone()),
        Some(ChatMessageContent::Blocks(blocks)) => Some(
            blocks
                .iter()
                .filter_map(|block| {
                    block
                        .get("text")
                        .and_then(Value::as_str)
                        .or_else(|| block.get("content").and_then(Value::as_str))
                })
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        None => None,
    }
}

fn issue(
    field: impl Into<String>,
    reason: impl Into<String>,
    severity: impl Into<String>,
) -> ValidationIssue {
    ValidationIssue {
        field: field.into(),
        reason: reason.into(),
        severity: severity.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{GuardrailRequest, RouteKind, SchemaHint};
    use serde_json::json;

    fn request(schema_hint: SchemaHint) -> GuardrailRequest {
        GuardrailRequest {
            route: RouteKind::ChatCompletions,
            provider: "openai".into(),
            upstream_path: "/v1/chat/completions".into(),
            model: "gpt".into(),
            body: json!({"messages": []}),
            stream: false,
            schema_hint: Some(schema_hint),
            step_policy: None,
        }
    }

    fn chat_response(content: Value) -> Value {
        json!({
            "id": "cmpl_1",
            "object": "chat.completion",
            "created": 1,
            "model": "gpt",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": content}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        })
    }

    fn options() -> ValidationOptions {
        ValidationOptions::default()
    }

    #[test]
    fn validates_json_mode_content() {
        let report = DefaultResponseValidator::default()
            .validate(
                &request(SchemaHint::JsonMode),
                &chat_response(json!("not-json")),
                &options(),
            )
            .unwrap();
        assert!(!report.ok);
    }

    #[test]
    fn accepts_valid_tool_call() {
        let response = json!({
            "id": "cmpl_1",
            "object": "chat.completion",
            "created": 1,
            "model": "gpt",
            "choices": [{"index": 0, "message": {"role": "assistant", "tool_calls": [{"id":"call_1","type":"function","function":{"name":"lookup","arguments":"{\"q\":\"x\"}"}}]}, "finish_reason": "tool_calls"}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        });
        let report = DefaultResponseValidator::default()
            .validate(&request(SchemaHint::ToolCalls), &response, &options())
            .unwrap();
        assert!(report.ok);
    }

    #[test]
    fn flags_missing_schema_field() {
        let report = DefaultResponseValidator::default()
            .validate(
                &request(SchemaHint::JsonSchema(
                    json!({"type":"object","required":["answer"]}),
                )),
                &json!({"other": true}),
                &options(),
            )
            .unwrap();
        assert!(!report.ok);
    }

    #[test]
    fn validates_step_completion() {
        let report = DefaultResponseValidator::default()
            .validate(
                &request(SchemaHint::StepCompletion(vec!["plan".into()], vec![])),
                &chat_response(json!("final only")),
                &options(),
            )
            .unwrap();
        assert!(!report.ok);
    }

    #[test]
    fn config_option_enables_json_validation_without_hint() {
        let mut req = request(SchemaHint::ToolCalls);
        req.schema_hint = None;
        let report = DefaultResponseValidator::default()
            .validate(
                &req,
                &chat_response(json!("not-json")),
                &ValidationOptions {
                    validate_json_mode: true,
                    ..ValidationOptions::default()
                },
            )
            .unwrap();
        assert!(!report.ok);
    }
}
