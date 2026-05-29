use crate::error::GuardrailError;
use crate::types::{RescueCandidate, ValidationIssue};
use serde_json::Value;

const MAX_TOOL_CALLS_TO_REPAIR: usize = 16;
const MAX_TOOL_ARGUMENT_BYTES: usize = 64 * 1024;

pub trait ToolCallRescuer: Send + Sync {
    fn rescue(&self, response: &Value) -> Result<Option<RescueCandidate>, GuardrailError>;
}

#[derive(Debug, Clone, Default)]
pub struct DefaultToolCallRescuer;

impl ToolCallRescuer for DefaultToolCallRescuer {
    fn rescue(&self, response: &Value) -> Result<Option<RescueCandidate>, GuardrailError> {
        let mut body = response.clone();
        let mut repaired_fields = Vec::new();
        let mut remaining_issues = Vec::new();

        let Some(choices) = body.get_mut("choices").and_then(Value::as_array_mut) else {
            return Ok(None);
        };

        let mut processed_tool_calls = 0usize;
        for (choice_index, choice) in choices.iter_mut().enumerate() {
            let Some(tool_calls) = choice
                .get_mut("message")
                .and_then(|message| message.get_mut("tool_calls"))
                .and_then(Value::as_array_mut)
            else {
                continue;
            };

            for (tool_index, tool_call) in tool_calls.iter_mut().enumerate() {
                let field = format!(
                    "choices.{choice_index}.message.tool_calls.{tool_index}.function.arguments"
                );
                processed_tool_calls += 1;
                if processed_tool_calls > MAX_TOOL_CALLS_TO_REPAIR {
                    remaining_issues.push(ValidationIssue {
                        field,
                        reason: "too many tool calls to repair safely".into(),
                        severity: "error".into(),
                    });
                    continue;
                }

                let Some(arguments) = tool_call
                    .get_mut("function")
                    .and_then(|function| function.get_mut("arguments"))
                    .and_then(|arguments| arguments.as_str())
                    .map(str::to_owned)
                else {
                    remaining_issues.push(ValidationIssue {
                        field,
                        reason: "tool call arguments must be a JSON string".into(),
                        severity: "error".into(),
                    });
                    continue;
                };

                if arguments.len() > MAX_TOOL_ARGUMENT_BYTES {
                    remaining_issues.push(ValidationIssue {
                        field,
                        reason: "tool call arguments are too large to repair safely".into(),
                        severity: "error".into(),
                    });
                    continue;
                }

                if serde_json::from_str::<Value>(&arguments).is_ok() {
                    continue;
                }

                match repair_json_object_text(&arguments) {
                    Some(repaired) => {
                        if let Some(arguments_value) = tool_call
                            .get_mut("function")
                            .and_then(|function| function.get_mut("arguments"))
                        {
                            *arguments_value = Value::String(repaired);
                            repaired_fields.push(field);
                        }
                    }
                    None => remaining_issues.push(ValidationIssue {
                        field,
                        reason: "tool call arguments are malformed and could not be repaired"
                            .into(),
                        severity: "error".into(),
                    }),
                }
            }
        }

        if repaired_fields.is_empty() && remaining_issues.is_empty() {
            Ok(None)
        } else {
            Ok(Some(RescueCandidate {
                body,
                repaired_fields,
                remaining_issues,
            }))
        }
    }
}

fn repair_json_object_text(input: &str) -> Option<String> {
    candidate_repairs(input)
        .into_iter()
        .find(|candidate| serde_json::from_str::<Value>(candidate).is_ok())
}

fn candidate_repairs(input: &str) -> Vec<String> {
    let trimmed = input.trim();
    let unwrapped = unwrap_json_string(trimmed).unwrap_or_else(|| trimmed.to_owned());
    let without_trailing_commas = remove_trailing_commas(&unwrapped);
    let with_double_quotes = without_trailing_commas.replace('\'', "\"");
    let quoted_keys = quote_unquoted_keys(&with_double_quotes);

    vec![
        unwrapped,
        without_trailing_commas,
        with_double_quotes,
        quoted_keys,
    ]
}

fn unwrap_json_string(input: &str) -> Option<String> {
    serde_json::from_str::<String>(input).ok()
}

fn remove_trailing_commas(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;

    while let Some(ch) = chars.next() {
        if in_string {
            output.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        if ch == '"' {
            in_string = true;
            output.push(ch);
            continue;
        }

        if ch == ',' {
            let mut lookahead = chars.clone();
            while matches!(lookahead.peek(), Some(next) if next.is_whitespace()) {
                lookahead.next();
            }
            if matches!(lookahead.peek(), Some('}') | Some(']')) {
                continue;
            }
        }

        output.push(ch);
    }

    output
}

fn quote_unquoted_keys(input: &str) -> String {
    let mut output = String::with_capacity(input.len() + 8);
    let mut chars = input.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;
    let mut expect_key = false;

    while let Some(ch) = chars.next() {
        if in_string {
            output.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        match ch {
            '"' => {
                in_string = true;
                output.push(ch);
            }
            '{' | ',' => {
                expect_key = true;
                output.push(ch);
            }
            c if expect_key && c.is_whitespace() => output.push(c),
            c if expect_key && is_identifier_start(c) => {
                let mut key = String::from(c);
                while let Some(next) = chars.peek().copied() {
                    if is_identifier_continue(next) {
                        key.push(next);
                        chars.next();
                    } else {
                        break;
                    }
                }
                let mut lookahead = chars.clone();
                while matches!(lookahead.peek(), Some(next) if next.is_whitespace()) {
                    lookahead.next();
                }
                if matches!(lookahead.peek(), Some(':')) {
                    output.push('"');
                    output.push_str(&key);
                    output.push('"');
                } else {
                    output.push_str(&key);
                }
                expect_key = false;
            }
            ':' => {
                expect_key = false;
                output.push(ch);
            }
            _ => {
                expect_key = false;
                output.push(ch);
            }
        }
    }

    output
}

fn is_identifier_start(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphabetic()
}

fn is_identifier_continue(ch: char) -> bool {
    ch == '_' || ch == '-' || ch.is_ascii_alphanumeric()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn repairs_single_quotes_trailing_commas_and_keys() {
        let repaired = repair_json_object_text("{foo: 'bar', count: 1,}").unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&repaired).unwrap()["foo"],
            "bar"
        );
    }

    #[test]
    fn repairs_escaped_json_string() {
        let repaired = repair_json_object_text("\"{\\\"foo\\\":1,}\"").unwrap();
        assert_eq!(serde_json::from_str::<Value>(&repaired).unwrap()["foo"], 1);
    }

    #[test]
    fn rescues_response_tool_arguments() {
        let response = json!({
            "choices": [{
                "message": {
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "lookup", "arguments": "{foo: 'bar',}"}
                    }]
                }
            }]
        });
        let candidate = DefaultToolCallRescuer.rescue(&response).unwrap().unwrap();
        assert_eq!(candidate.repaired_fields.len(), 1);
        assert!(candidate.remaining_issues.is_empty());
    }

    #[test]
    fn skips_repair_when_tool_call_limit_exceeded() {
        let tool_calls = (0..17)
            .map(|index| {
                json!({
                    "id": format!("call_{index}"),
                    "type": "function",
                    "function": {"name": "lookup", "arguments": "{foo: 'bar',}"}
                })
            })
            .collect::<Vec<_>>();
        let response = json!({"choices": [{"message": {"tool_calls": tool_calls}}]});
        let candidate = DefaultToolCallRescuer.rescue(&response).unwrap().unwrap();
        assert_eq!(candidate.repaired_fields.len(), 16);
        assert_eq!(candidate.remaining_issues.len(), 1);
    }

    #[test]
    fn skips_repair_when_arguments_too_large() {
        let response = json!({
            "choices": [{"message": {"tool_calls": [{
                "id": "call_1",
                "type": "function",
                "function": {"name": "lookup", "arguments": "x".repeat(MAX_TOOL_ARGUMENT_BYTES + 1)}
            }]}}]
        });
        let candidate = DefaultToolCallRescuer.rescue(&response).unwrap().unwrap();
        assert!(candidate.repaired_fields.is_empty());
        assert_eq!(candidate.remaining_issues.len(), 1);
    }
}
