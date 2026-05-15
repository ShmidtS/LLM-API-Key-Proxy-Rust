use serde_json::{Value, json};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn openai_to_anthropic_messages(body: &Value) -> Value {
    let mut output = json!({
        "model": body.get("model").cloned().unwrap_or(Value::Null),
        "max_tokens": body
            .get("max_tokens")
            .or_else(|| body.get("max_completion_tokens"))
            .cloned()
            .unwrap_or_else(|| json!(4096)),
        "messages": [],
    });

    copy_optional(body, &mut output, "temperature", "temperature");
    copy_optional(body, &mut output, "top_p", "top_p");
    copy_optional(body, &mut output, "stream", "stream");
    copy_optional(body, &mut output, "stop", "stop_sequences");

    let mut system_parts = Vec::new();
    let mut messages = Vec::new();

    for message in body
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("user");
        let content = message.get("content").unwrap_or(&Value::Null);

        if role == "system" {
            if let Some(text) = content_text(content) {
                system_parts.push(text);
            }
            continue;
        }

        messages.push(json!({
            "role": if role == "assistant" { "assistant" } else { "user" },
            "content": openai_content_to_anthropic(content),
        }));
    }

    output["messages"] = Value::Array(messages);
    if !system_parts.is_empty() {
        output["system"] = Value::String(system_parts.join("\n"));
    }

    output
}

pub fn anthropic_to_openai_response(body: &Value, model: &str) -> Value {
    let prompt_tokens = body
        .pointer("/usage/input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let completion_tokens = body
        .pointer("/usage/output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);

    json!({
        "id": body
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("chatcmpl-anthropic"),
        "object": "chat.completion",
        "created": unix_time(),
        "model": model,
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": anthropic_content_text(body.get("content").unwrap_or(&Value::Null)),
            },
            "finish_reason": map_stop_reason(body.get("stop_reason").and_then(Value::as_str)),
        }],
        "usage": {
            "prompt_tokens": prompt_tokens,
            "completion_tokens": completion_tokens,
            "total_tokens": prompt_tokens + completion_tokens,
        },
    })
}

pub fn anthropic_stream_to_openai_sse(chunk: &str, model: &str) -> Option<String> {
    let event = chunk
        .lines()
        .find_map(|line| line.strip_prefix("event:").map(str::trim));

    let mut output = String::new();
    for data in chunk
        .lines()
        .filter_map(|line| line.strip_prefix("data:").map(str::trim))
    {
        if data == "[DONE]" {
            output.push_str("data: [DONE]\n\n");
            continue;
        }

        let Ok(value) = serde_json::from_str::<Value>(data) else {
            continue;
        };

        let chunk = match value.get("type").and_then(Value::as_str).or(event) {
            Some("message_start") => Some(openai_stream_chunk(
                value.pointer("/message/id").and_then(Value::as_str),
                model,
                json!({ "role": "assistant" }),
                Value::Null,
            )),
            Some("content_block_delta") => value
                .pointer("/delta/text")
                .and_then(Value::as_str)
                .map(|text| {
                    openai_stream_chunk(None, model, json!({ "content": text }), Value::Null)
                }),
            Some("message_delta") => Some(openai_stream_chunk(
                None,
                model,
                json!({}),
                map_stop_reason(value.pointer("/delta/stop_reason").and_then(Value::as_str)),
            )),
            Some("message_stop") => {
                output.push_str("data: [DONE]\n\n");
                None
            }
            _ => None,
        };

        if let Some(chunk) = chunk {
            output.push_str("data: ");
            output.push_str(&chunk.to_string());
            output.push_str("\n\n");
        }
    }

    if output.is_empty() {
        None
    } else {
        Some(output)
    }
}

fn copy_optional(input: &Value, output: &mut Value, from: &str, to: &str) {
    if let Some(value) = input.get(from) {
        output[to] = value.clone();
    }
}

fn content_text(content: &Value) -> Option<String> {
    match content {
        Value::String(text) => Some(text.clone()),
        Value::Array(_) => {
            let text = anthropic_content_text(&openai_content_to_anthropic(content));
            (!text.is_empty()).then_some(text)
        }
        _ => None,
    }
}

fn openai_content_to_anthropic(content: &Value) -> Value {
    match content {
        Value::String(_) => content.clone(),
        Value::Array(parts) => Value::Array(
            parts
                .iter()
                .filter_map(|part| {
                    if part.get("type").and_then(Value::as_str) == Some("text") {
                        part.get("text")
                            .and_then(Value::as_str)
                            .map(|text| json!({ "type": "text", "text": text }))
                    } else {
                        None
                    }
                })
                .collect(),
        ),
        Value::Null => Value::String(String::new()),
        _ => content.clone(),
    }
}

fn anthropic_content_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| match part {
                Value::String(text) => Some(text.clone()),
                Value::Object(_) => part
                    .get("text")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

fn map_stop_reason(reason: Option<&str>) -> Value {
    match reason {
        Some("end_turn") | Some("stop_sequence") => Value::String("stop".to_owned()),
        Some("max_tokens") => Value::String("length".to_owned()),
        Some("tool_use") => Value::String("tool_calls".to_owned()),
        Some(other) => Value::String(other.to_owned()),
        None => Value::Null,
    }
}

fn openai_stream_chunk(id: Option<&str>, model: &str, delta: Value, finish_reason: Value) -> Value {
    json!({
        "id": id.unwrap_or("chatcmpl-anthropic"),
        "object": "chat.completion.chunk",
        "created": unix_time(),
        "model": model,
        "choices": [{
            "index": 0,
            "delta": delta,
            "finish_reason": finish_reason,
        }],
    })
}

fn unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_openai_messages_to_anthropic_shape() {
        let input = json!({
            "model": "claude-3-5-sonnet-latest",
            "messages": [
                {"role": "system", "content": "Be terse"},
                {"role": "user", "content": "Hi"},
                {"role": "assistant", "content": [{"type": "text", "text": "Hello"}]}
            ],
            "stream": true,
            "stop": ["END"]
        });

        let output = openai_to_anthropic_messages(&input);

        assert_eq!(output["system"], "Be terse");
        assert_eq!(output["max_tokens"], 4096);
        assert_eq!(output["stream"], true);
        assert_eq!(output["stop_sequences"], json!(["END"]));
        assert_eq!(output["messages"][0]["role"], "user");
        assert_eq!(output["messages"][0]["content"], "Hi");
        assert_eq!(output["messages"][1]["role"], "assistant");
        assert_eq!(
            output["messages"][1]["content"],
            json!([{ "type": "text", "text": "Hello" }])
        );
    }

    #[test]
    fn converts_anthropic_response_to_openai_shape() {
        let input = json!({
            "id": "msg_123",
            "content": [{"type": "text", "text": "Hello"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 3, "output_tokens": 2}
        });

        let output = anthropic_to_openai_response(&input, "claude-test");

        assert_eq!(output["id"], "msg_123");
        assert_eq!(output["object"], "chat.completion");
        assert_eq!(output["model"], "claude-test");
        assert_eq!(output["choices"][0]["message"]["content"], "Hello");
        assert_eq!(output["choices"][0]["finish_reason"], "stop");
        assert_eq!(output["usage"]["total_tokens"], 5);
    }

    #[test]
    fn converts_anthropic_stream_delta_to_openai_sse() {
        let input = "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n\n";

        let output = anthropic_stream_to_openai_sse(input, "claude-test").unwrap();

        assert!(output.starts_with("data: "));
        assert!(output.contains("chat.completion.chunk"));
        assert!(output.contains("\"content\":\"Hi\""));
    }

    #[test]
    fn converts_anthropic_message_stop_to_done() {
        let input = "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";

        let output = anthropic_stream_to_openai_sse(input, "claude-test").unwrap();

        assert_eq!(output, "data: [DONE]\n\n");
    }
}
