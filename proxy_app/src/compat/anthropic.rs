use serde_json::{Value, json};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn openai_to_anthropic_messages(body: &Value) -> Value {
    translate_anthropic_request(body)
}

pub fn anthropic_to_openai_chat_request(body: &Value) -> Value {
    let mut output = json!({
        "model": body.get("model").cloned().unwrap_or(Value::Null),
        "messages": anthropic_to_openai_messages(body.get("messages").unwrap_or(&Value::Null)),
    });

    copy_optional(body, &mut output, "max_tokens", "max_tokens");
    copy_optional(body, &mut output, "stream", "stream");
    copy_optional(body, &mut output, "temperature", "temperature");
    copy_optional(body, &mut output, "top_p", "top_p");
    copy_optional(body, &mut output, "top_k", "top_k");
    copy_optional(body, &mut output, "stop_sequences", "stop");

    if let Some(system) = anthropic_system_to_openai_message(body.get("system"))
        && let Some(messages) = output.get_mut("messages").and_then(Value::as_array_mut)
    {
        messages.insert(0, system);
    }
    if let Some(tools) = body.get("tools") {
        output["tools"] = anthropic_tools_to_openai_tools(tools);
    }
    if let Some(tool_choice) = body.get("tool_choice") {
        output["tool_choice"] = anthropic_tool_choice_to_openai_tool_choice(tool_choice);
    }
    if body.pointer("/thinking/type").and_then(Value::as_str) == Some("enabled")
        && let Some(budget) = body
            .pointer("/thinking/budget_tokens")
            .and_then(Value::as_u64)
        && budget > 0
    {
        let model = body
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default();
        output["reasoning_effort"] =
            Value::String(budget_to_reasoning_effort(budget, model).to_owned());
    }

    output
}

pub fn anthropic_to_openai_messages(messages: &Value) -> Value {
    Value::Array(
        messages
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(anthropic_message_to_openai_messages)
            .collect(),
    )
}

pub fn anthropic_tools_to_openai_tools(tools: &Value) -> Value {
    Value::Array(
        tools
            .as_array()
            .into_iter()
            .flatten()
            .map(|tool| {
                json!({
                    "type": "function",
                    "function": {
                        "name": tool.get("name").and_then(Value::as_str).unwrap_or_default(),
                        "description": tool.get("description").and_then(Value::as_str).unwrap_or_default(),
                        "parameters": tool.get("input_schema").cloned().unwrap_or_else(|| json!({})),
                    }
                })
            })
            .collect(),
    )
}

pub fn anthropic_tool_choice_to_openai_tool_choice(tool_choice: &Value) -> Value {
    match tool_choice {
        Value::String(choice) => match choice.as_str() {
            "any" => Value::String("required".to_owned()),
            "auto" | "none" => Value::String(choice.clone()),
            _ => Value::Null,
        },
        Value::Object(_) => match tool_choice.get("type").and_then(Value::as_str) {
            Some("any") => Value::String("required".to_owned()),
            Some("auto") => Value::String("auto".to_owned()),
            Some("none") => Value::String("none".to_owned()),
            Some("tool") => json!({
                "type": "function",
                "function": {
                    "name": tool_choice.get("name").and_then(Value::as_str).unwrap_or_default(),
                }
            }),
            _ => Value::Null,
        },
        _ => Value::Null,
    }
}

pub fn openai_chat_to_anthropic_response(response: &Value, model: &str) -> Value {
    let message = response
        .pointer("/choices/0/message")
        .unwrap_or(&Value::Null);
    let mut content = Vec::new();

    if let Some(text) = message.get("content").and_then(Value::as_str)
        && !text.is_empty()
    {
        content.push(json!({"type": "text", "text": text}));
    }
    if let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) {
        for tool_call in tool_calls {
            let input = tool_call
                .pointer("/function/arguments")
                .and_then(Value::as_str)
                .and_then(|arguments| serde_json::from_str::<Value>(arguments).ok())
                .unwrap_or_else(|| json!({}));
            content.push(json!({
                "type": "tool_use",
                "id": tool_call.get("id").and_then(Value::as_str).unwrap_or_default(),
                "name": tool_call.pointer("/function/name").and_then(Value::as_str).unwrap_or_default(),
                "input": input,
            }));
        }
    }

    json!({
        "id": response.get("id").and_then(Value::as_str).unwrap_or("msg_openai"),
        "type": "message",
        "role": "assistant",
        "model": response.get("model").and_then(Value::as_str).unwrap_or(model),
        "content": content,
        "stop_reason": openai_finish_reason_to_anthropic(response.pointer("/choices/0/finish_reason").and_then(Value::as_str)),
        "stop_sequence": Value::Null,
        "usage": {
            "input_tokens": response.pointer("/usage/prompt_tokens").and_then(Value::as_u64).unwrap_or(0),
            "output_tokens": response.pointer("/usage/completion_tokens").and_then(Value::as_u64).unwrap_or(0),
        }
    })
}

pub fn translate_anthropic_request(body: &Value) -> Value {
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
    copy_optional(body, &mut output, "thinking", "thinking");

    let mut system = SystemAccumulator::default();
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
            system.push(content);
            continue;
        }

        let anthropic_role = if role == "assistant" {
            "assistant"
        } else {
            "user"
        };
        let mut anthropic_content = if role == "tool" {
            tool_result_content(message)
        } else {
            openai_content_to_anthropic(content)
        };

        if role == "assistant" {
            anthropic_content = append_tool_calls(anthropic_content, message.get("tool_calls"));
            anthropic_content = reorder_assistant_content(anthropic_content);
        }

        messages.push(json!({
            "role": anthropic_role,
            "content": anthropic_content,
        }));
    }

    output["messages"] = Value::Array(messages);
    if let Some(system) = system.into_value() {
        output["system"] = system;
    }
    if let Some(tools) = openai_tools_to_anthropic(body.get("tools")) {
        output["tools"] = tools;
    }
    if let Some(tool_choice) = openai_tool_choice_to_anthropic(body.get("tool_choice")) {
        output["tool_choice"] = tool_choice;
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
    let content = body.get("content").unwrap_or(&Value::Null);
    let mut message = json!({
        "role": "assistant",
        "content": anthropic_content_text(content),
    });

    if let Some(tool_calls) = anthropic_tool_calls(content) {
        message["tool_calls"] = tool_calls;
    }
    if let Some(reasoning_content) = anthropic_reasoning_content(content) {
        message["reasoning_content"] = reasoning_content;
    }
    if let Some(reasoning_details) = anthropic_reasoning_details(content) {
        message["reasoning_details"] = reasoning_details;
    }

    let mut response = json!({
        "id": body
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("chatcmpl-anthropic"),
        "object": "chat.completion",
        "created": unix_time(),
        "model": model,
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": map_stop_reason(body.get("stop_reason").and_then(Value::as_str)),
        }],
        "usage": {
            "prompt_tokens": prompt_tokens,
            "completion_tokens": completion_tokens,
            "total_tokens": prompt_tokens + completion_tokens,
        },
    });
    if let Some(budget) = body
        .get("thinking")
        .and_then(|t| t.get("budget_tokens"))
        .and_then(Value::as_u64)
    {
        response["reasoning_effort"] = json!(budget_to_reasoning_effort(budget, model));
    }
    preserve_anthropic_extra_fields(body, &mut response);
    response
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

fn anthropic_system_to_openai_message(system: Option<&Value>) -> Option<Value> {
    let content = match system? {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    };

    (!content.is_empty()).then_some(json!({"role": "system", "content": content}))
}

fn anthropic_message_to_openai_messages(message: &Value) -> Vec<Value> {
    let role = message
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("user");
    let content = message.get("content").unwrap_or(&Value::Null);

    match content {
        Value::String(text) => vec![json!({"role": role, "content": text})],
        Value::Array(blocks) => anthropic_blocks_to_openai_messages(role, blocks),
        _ => vec![json!({"role": role, "content": ""})],
    }
}

fn anthropic_blocks_to_openai_messages(role: &str, blocks: &[Value]) -> Vec<Value> {
    let tool_results = anthropic_tool_result_messages(blocks);
    if !tool_results.is_empty() {
        let mut messages = tool_results;
        // Preserve any non-tool_result content (e.g. text the user typed
        // alongside tool results) as a follow-up message so it is not lost.
        let parts = blocks
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) != Some("tool_result"))
            .filter_map(anthropic_content_block_to_openai_part)
            .collect::<Vec<_>>();
        if !parts.is_empty() {
            messages.push(json!({"role": role, "content": openai_content_from_parts(parts)}));
        }
        return messages;
    }

    let parts = blocks
        .iter()
        .filter_map(anthropic_content_block_to_openai_part)
        .collect::<Vec<_>>();
    let tool_calls = anthropic_tool_use_calls(blocks);
    if !tool_calls.is_empty() {
        let content = if parts.is_empty() {
            Value::Null
        } else {
            openai_content_from_parts(parts)
        };
        let mut message = json!({"role": "assistant", "content": content});
        message["tool_calls"] = Value::Array(tool_calls);
        return vec![message];
    }

    vec![json!({"role": role, "content": openai_content_from_parts(parts)})]
}

fn openai_content_from_parts(parts: Vec<Value>) -> Value {
    if parts.is_empty() {
        return Value::Null;
    }

    if parts
        .iter()
        .all(|part| part.get("type").and_then(Value::as_str) == Some("text"))
    {
        Value::String(
            parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(" "),
        )
    } else {
        Value::Array(parts)
    }
}

fn anthropic_content_block_to_openai_part(block: &Value) -> Option<Value> {
    match block.get("type").and_then(Value::as_str) {
        Some("text") => block
            .get("text")
            .and_then(Value::as_str)
            .map(|text| json!({"type": "text", "text": text})),
        Some("image") => anthropic_source_to_openai_image_url(block.get("source")?),
        Some("document") => anthropic_source_to_openai_image_url(block.get("source")?),
        _ => None,
    }
}

fn anthropic_source_to_openai_image_url(source: &Value) -> Option<Value> {
    let url = match source.get("type").and_then(Value::as_str) {
        Some("base64") => format!(
            "data:{};base64,{}",
            source
                .get("media_type")
                .and_then(Value::as_str)
                .unwrap_or("application/octet-stream"),
            source
                .get("data")
                .and_then(Value::as_str)
                .unwrap_or_default()
        ),
        Some("url") => source.get("url").and_then(Value::as_str)?.to_owned(),
        _ => return None,
    };
    Some(json!({"type": "image_url", "image_url": {"url": url}}))
}

fn anthropic_tool_use_calls(blocks: &[Value]) -> Vec<Value> {
    blocks
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_use"))
        .map(|block| {
            json!({
                "id": block.get("id").and_then(Value::as_str).unwrap_or_default(),
                "type": "function",
                "function": {
                    "name": block.get("name").and_then(Value::as_str).unwrap_or_default(),
                    "arguments": block.get("input").map(Value::to_string).unwrap_or_else(|| "{}".to_owned()),
                }
            })
        })
        .collect()
}

fn anthropic_tool_result_messages(blocks: &[Value]) -> Vec<Value> {
    blocks
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"))
        .map(|block| {
            let content = match block.get("content").unwrap_or(&Value::Null) {
                Value::String(text) => text.clone(),
                Value::Array(parts) => parts
                    .iter()
                    .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
                    .filter_map(|part| part.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join(" "),
                Value::Null => String::new(),
                other => other.to_string(),
            };
            json!({
                "role": "tool",
                "tool_call_id": block.get("tool_use_id").and_then(Value::as_str).unwrap_or_default(),
                "content": content,
            })
        })
        .collect()
}

fn openai_finish_reason_to_anthropic(reason: Option<&str>) -> Value {
    match reason {
        Some("length") => Value::String("max_tokens".to_owned()),
        Some("tool_calls") => Value::String("tool_use".to_owned()),
        Some("stop") => Value::String("end_turn".to_owned()),
        Some(other) => Value::String(other.to_owned()),
        None => Value::Null,
    }
}

#[derive(Default)]
struct SystemAccumulator {
    strings: Vec<String>,
    blocks: Vec<Value>,
}

impl SystemAccumulator {
    fn push(&mut self, content: &Value) {
        match content {
            Value::String(text) => self.strings.push(text.clone()),
            Value::Array(parts) => {
                for part in parts {
                    if let Some(block) = system_part_to_anthropic(part) {
                        self.blocks.push(block);
                    }
                }
            }
            _ => {}
        }
    }

    fn into_value(self) -> Option<Value> {
        match (self.strings.is_empty(), self.blocks.is_empty()) {
            (true, true) => None,
            (false, true) => Some(Value::String(self.strings.join("\n"))),
            (true, false) => Some(Value::Array(self.blocks)),
            (false, false) => {
                let mut blocks: Vec<Value> = self
                    .strings
                    .into_iter()
                    .map(|text| json!({ "type": "text", "text": text }))
                    .collect();
                blocks.extend(self.blocks);
                Some(Value::Array(blocks))
            }
        }
    }
}

fn system_part_to_anthropic(part: &Value) -> Option<Value> {
    match part {
        Value::String(text) => Some(json!({ "type": "text", "text": text })),
        Value::Object(_) if part.get("type").and_then(Value::as_str) == Some("text") => part
            .get("text")
            .and_then(Value::as_str)
            .map(|text| json!({ "type": "text", "text": text })),
        Value::Object(_) => Some(part.clone()),
        _ => None,
    }
}

fn openai_content_to_anthropic(content: &Value) -> Value {
    match content {
        Value::String(_) => content.clone(),
        Value::Array(parts) => {
            Value::Array(parts.iter().filter_map(openai_part_to_anthropic).collect())
        }
        Value::Null => Value::String(String::new()),
        _ => content.clone(),
    }
}

fn openai_part_to_anthropic(part: &Value) -> Option<Value> {
    match part.get("type").and_then(Value::as_str) {
        Some("text") => part
            .get("text")
            .and_then(Value::as_str)
            .map(|text| json!({ "type": "text", "text": text })),
        Some("image_url") => openai_image_url_to_anthropic(part),
        Some("thinking") | Some("redacted_thinking") | Some("tool_use") | Some("tool_result") => {
            Some(part.clone())
        }
        _ => None,
    }
}

fn openai_image_url_to_anthropic(part: &Value) -> Option<Value> {
    let url = part.pointer("/image_url/url").and_then(Value::as_str)?;
    let data_url = url.strip_prefix("data:")?;
    let (media_type, data) = data_url.split_once(";base64,")?;
    Some(json!({
        "type": "image",
        "source": {
            "type": "base64",
            "media_type": media_type,
            "data": data,
        }
    }))
}

fn append_tool_calls(content: Value, tool_calls: Option<&Value>) -> Value {
    let Some(tool_calls) = tool_calls.and_then(Value::as_array) else {
        return content;
    };

    let mut blocks = match content {
        Value::Array(blocks) => blocks,
        Value::String(text) if text.is_empty() => Vec::new(),
        Value::String(text) => vec![json!({ "type": "text", "text": text })],
        Value::Null => Vec::new(),
        other => vec![other],
    };

    for tool_call in tool_calls {
        let name = tool_call
            .pointer("/function/name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let input = tool_call
            .pointer("/function/arguments")
            .and_then(Value::as_str)
            .and_then(|arguments| serde_json::from_str::<Value>(arguments).ok())
            .unwrap_or_else(|| json!({}));
        blocks.push(json!({
            "type": "tool_use",
            "id": tool_call.get("id").and_then(Value::as_str).unwrap_or_default(),
            "name": name,
            "input": input,
        }));
    }

    Value::Array(blocks)
}

fn tool_result_content(message: &Value) -> Value {
    let content = openai_content_to_anthropic(message.get("content").unwrap_or(&Value::Null));
    Value::Array(vec![json!({
        "type": "tool_result",
        "tool_use_id": message
            .get("tool_call_id")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        "content": content,
    })])
}

fn reorder_assistant_content(content: Value) -> Value {
    let Value::Array(blocks) = content else {
        return content;
    };
    if blocks.len() <= 1 {
        return Value::Array(blocks);
    }

    let mut thinking_blocks = Vec::new();
    let mut text_blocks = Vec::new();
    let mut tool_use_blocks = Vec::new();
    let mut other_blocks = Vec::new();

    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("thinking") | Some("redacted_thinking") => {
                thinking_blocks.push(sanitize_thinking_block(block));
            }
            Some("tool_use") => tool_use_blocks.push(block),
            Some("text") => {
                if block
                    .get("text")
                    .and_then(Value::as_str)
                    .is_some_and(|text| !text.trim().is_empty())
                {
                    text_blocks.push(block);
                }
            }
            _ => other_blocks.push(block),
        }
    }

    thinking_blocks.extend(other_blocks);
    thinking_blocks.extend(text_blocks);
    thinking_blocks.extend(tool_use_blocks);
    Value::Array(thinking_blocks)
}

fn sanitize_thinking_block(block: Value) -> Value {
    let block_type = block
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("thinking");
    let mut sanitized = json!({
        "type": block_type,
        "thinking": block.get("thinking").and_then(Value::as_str).unwrap_or_default(),
    });
    if let Some(signature) = block.get("signature").and_then(Value::as_str)
        && !signature.is_empty()
    {
        sanitized["signature"] = Value::String(signature.to_owned());
    }
    if let Some(cache_control) = block.get("cache_control") {
        sanitized["cache_control"] = cache_control.clone();
    }
    sanitized
}

fn openai_tools_to_anthropic(tools: Option<&Value>) -> Option<Value> {
    let converted = tools?
        .as_array()?
        .iter()
        .filter_map(|tool| {
            if tool.get("type").and_then(Value::as_str) != Some("function") {
                return None;
            }
            let function = tool.get("function")?;
            Some(json!({
                "name": function.get("name").and_then(Value::as_str).unwrap_or_default(),
                "description": function
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                "input_schema": function.get("parameters").cloned().unwrap_or_else(|| json!({})),
            }))
        })
        .collect::<Vec<_>>();

    (!converted.is_empty()).then_some(Value::Array(converted))
}

fn openai_tool_choice_to_anthropic(tool_choice: Option<&Value>) -> Option<Value> {
    match tool_choice? {
        Value::String(choice) => match choice.as_str() {
            "auto" => Some(json!({ "type": "auto" })),
            "none" => Some(json!({ "type": "none" })),
            "required" => Some(json!({ "type": "any" })),
            _ => None,
        },
        Value::Object(_) => {
            let name = tool_choice?
                .pointer("/function/name")
                .and_then(Value::as_str)?;
            Some(json!({ "type": "tool", "name": name }))
        }
        _ => None,
    }
}

/// Maps Anthropic thinking budget_tokens to OpenAI reasoning_effort levels.
pub(crate) fn budget_to_reasoning_effort(budget_tokens: u64, model: &str) -> &'static str {
    let granular_level = match budget_tokens {
        0..=4096 => "minimal",
        4097..=8192 => "low",
        8193..=12288 => "low_medium",
        12289..=16384 => "medium",
        16385..=24576 => "medium_high",
        _ => "high",
    };

    let provider = model.split_once('/').map(|(provider, _)| provider);
    if provider.is_some_and(|provider| provider.eq_ignore_ascii_case("antigravity")) {
        return granular_level;
    }

    match granular_level {
        "minimal" | "low" => "low",
        "low_medium" | "medium" => "medium",
        "medium_high" | "high" => "high",
        _ => "medium",
    }
}

fn anthropic_content_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| match part {
                Value::String(text) => Some(text.clone()),
                Value::Object(_) if part.get("type").and_then(Value::as_str) == Some("text") => {
                    part.get("text")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

fn anthropic_tool_calls(content: &Value) -> Option<Value> {
    let calls = content
        .as_array()?
        .iter()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("tool_use"))
        .map(|part| {
            json!({
                "id": part.get("id").and_then(Value::as_str).unwrap_or_default(),
                "type": "function",
                "function": {
                    "name": part.get("name").and_then(Value::as_str).unwrap_or_default(),
                    "arguments": part
                        .get("input")
                        .map(Value::to_string)
                        .unwrap_or_else(|| "{}".to_owned()),
                }
            })
        })
        .collect::<Vec<_>>();

    (!calls.is_empty()).then_some(Value::Array(calls))
}

fn anthropic_reasoning_content(content: &Value) -> Option<Value> {
    let text = content
        .as_array()?
        .iter()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("thinking"))
        .filter_map(|part| part.get("thinking").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("");

    (!text.is_empty()).then_some(Value::String(text))
}

fn anthropic_reasoning_details(content: &Value) -> Option<Value> {
    let details = content
        .as_array()?
        .iter()
        .filter_map(|part| match part.get("type").and_then(Value::as_str) {
            Some("thinking") | Some("redacted_thinking") => Some(part.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();

    (!details.is_empty()).then_some(Value::Array(details))
}

fn preserve_anthropic_extra_fields(input: &Value, output: &mut Value) {
    let Some(fields) = input.as_object() else {
        return;
    };

    for (key, value) in fields {
        if !matches!(
            key.as_str(),
            "id" | "type"
                | "role"
                | "content"
                | "model"
                | "stop_reason"
                | "stop_sequence"
                | "usage"
        ) {
            output[key] = value.clone();
        }
    }
}

fn map_stop_reason(reason: Option<&str>) -> Value {
    match reason {
        Some("end_turn") | Some("end_sequence") | Some("stop_sequence") => {
            Value::String("stop".to_owned())
        }
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
    fn uses_max_completion_tokens_when_max_tokens_absent() {
        let input = json!({
            "model": "claude-test",
            "messages": [{"role": "user", "content": "Hi"}],
            "max_completion_tokens": 123,
            "temperature": 0.4,
            "top_p": 0.8
        });

        let output = translate_anthropic_request(&input);

        assert_eq!(output["max_tokens"], 123);
        assert_eq!(output["temperature"], 0.4);
        assert_eq!(output["top_p"], 0.8);
    }

    #[test]
    fn converts_system_list_to_anthropic_system_blocks() {
        let input = json!({
            "model": "claude-test",
            "messages": [{
                "role": "system",
                "content": [
                    {"type": "text", "text": "First"},
                    {"type": "text", "text": "Second"}
                ]
            }, {"role": "user", "content": "Hi"}]
        });

        let output = translate_anthropic_request(&input);

        assert_eq!(
            output["system"],
            json!([
                {"type": "text", "text": "First"},
                {"type": "text", "text": "Second"}
            ])
        );
    }

    #[test]
    fn converts_basic_function_tools_and_tool_choice() {
        let input = json!({
            "model": "claude-test",
            "messages": [{"role": "user", "content": "weather"}],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "lookup",
                    "description": "Lookup weather",
                    "parameters": {"type": "object", "properties": {"city": {"type": "string"}}}
                }
            }],
            "tool_choice": {"type": "function", "function": {"name": "lookup"}}
        });

        let output = translate_anthropic_request(&input);

        assert_eq!(output["tools"][0]["name"], "lookup");
        assert_eq!(output["tools"][0]["description"], "Lookup weather");
        assert_eq!(output["tools"][0]["input_schema"]["type"], "object");
        assert_eq!(
            output["tool_choice"],
            json!({"type": "tool", "name": "lookup"})
        );
    }

    #[test]
    fn converts_basic_string_tool_choices() {
        let input = json!({
            "model": "claude-test",
            "messages": [{"role": "user", "content": "weather"}],
            "tool_choice": "required"
        });

        let output = translate_anthropic_request(&input);

        assert_eq!(output["tool_choice"], json!({"type": "any"}));
    }

    #[test]
    fn converts_tool_calls_and_tool_results() {
        let input = json!({
            "model": "claude-test",
            "messages": [{
                "role": "assistant",
                "content": "I'll check",
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "lookup", "arguments": "{\"city\":\"Paris\"}"}
                }]
            }, {
                "role": "tool",
                "tool_call_id": "call_1",
                "content": "sunny"
            }]
        });

        let output = translate_anthropic_request(&input);

        assert_eq!(output["messages"][0]["role"], "assistant");
        assert_eq!(
            output["messages"][0]["content"][0],
            json!({"type": "text", "text": "I'll check"})
        );
        assert_eq!(output["messages"][0]["content"][1]["type"], "tool_use");
        assert_eq!(
            output["messages"][0]["content"][1]["input"]["city"],
            "Paris"
        );
        assert_eq!(output["messages"][1]["role"], "user");
        assert_eq!(output["messages"][1]["content"][0]["type"], "tool_result");
        assert_eq!(output["messages"][1]["content"][0]["tool_use_id"], "call_1");
    }

    #[test]
    fn reorders_assistant_content_blocks() {
        let input = json!({
            "model": "claude-test",
            "messages": [{
                "role": "assistant",
                "content": [
                    {"type": "tool_use", "id": "call_1", "name": "lookup", "input": {}},
                    {"type": "text", "text": "answer"},
                    {"type": "thinking", "thinking": "reason", "signature": "sig", "cache_control": {}}
                ]
            }]
        });

        let output = translate_anthropic_request(&input);
        let content = output["messages"][0]["content"].as_array().unwrap();

        assert_eq!(content[0]["type"], "thinking");
        assert_eq!(content[0]["cache_control"], json!({}));
        assert_eq!(content[1]["type"], "text");
        assert_eq!(content[2]["type"], "tool_use");
    }

    #[test]
    fn maps_thinking_budget_to_reasoning_effort() {
        assert_eq!(budget_to_reasoning_effort(4096, "openai/o3"), "low");
        assert_eq!(budget_to_reasoning_effort(12288, "openai/o3"), "medium");
        assert_eq!(budget_to_reasoning_effort(24576, "openai/o3"), "high");
        assert_eq!(
            budget_to_reasoning_effort(12288, "antigravity/test"),
            "low_medium"
        );
    }

    #[test]
    fn propagates_reasoning_effort_from_thinking_budget() {
        let input = json!({
            "content": [{"type": "text", "text": "Hello"}],
            "thinking": {"type": "enabled", "budget_tokens": 12288}
        });

        let output = anthropic_to_openai_response(&input, "openai/o3");

        assert_eq!(output["reasoning_effort"], "medium");
    }

    #[test]
    fn preserves_thinking_config() {
        let input = json!({
            "model": "claude-test",
            "messages": [{"role": "user", "content": "think"}],
            "thinking": {"type": "enabled", "budget_tokens": 8192}
        });

        let output = translate_anthropic_request(&input);

        assert_eq!(
            output["thinking"],
            json!({"type": "enabled", "budget_tokens": 8192})
        );
    }

    #[test]
    fn converts_anthropic_request_to_openai_chat_shape() {
        let input = json!({
            "model": "fireworks/llama",
            "system": [{"type": "text", "text": "Be terse"}, {"type": "image", "source": {}}],
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": "Look"},
                    {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "abc"}}
                ]
            }],
            "max_tokens": 99,
            "stream": false,
            "stop_sequences": ["END"],
            "metadata": {"user_id": "drop-me"},
            "thinking": {"type": "enabled", "budget_tokens": 1000}
        });

        let output = anthropic_to_openai_chat_request(&input);

        assert_eq!(output["model"], "fireworks/llama");
        assert_eq!(
            output["messages"][0],
            json!({"role": "system", "content": "Be terse"})
        );
        assert_eq!(output["messages"][1]["role"], "user");
        assert_eq!(
            output["messages"][1]["content"][0],
            json!({"type": "text", "text": "Look"})
        );
        assert_eq!(
            output["messages"][1]["content"][1],
            json!({"type": "image_url", "image_url": {"url": "data:image/png;base64,abc"}})
        );
        assert_eq!(output["stop"], json!(["END"]));
        assert_eq!(output["reasoning_effort"], "low");
        assert_eq!(output.get("metadata"), None);
    }

    #[test]
    fn converts_anthropic_tools_and_tool_choice_to_openai() {
        let tools = json!([{
            "name": "lookup",
            "description": "Lookup weather",
            "input_schema": {"type": "object", "properties": {"city": {"type": "string"}}}
        }]);

        let output = anthropic_tools_to_openai_tools(&tools);

        assert_eq!(output[0]["type"], "function");
        assert_eq!(output[0]["function"]["name"], "lookup");
        assert_eq!(output[0]["function"]["parameters"]["type"], "object");
        assert_eq!(
            anthropic_tool_choice_to_openai_tool_choice(&json!("any")),
            "required"
        );
        assert_eq!(
            anthropic_tool_choice_to_openai_tool_choice(&json!({"type": "tool", "name": "lookup"})),
            json!({"type": "function", "function": {"name": "lookup"}})
        );
    }

    #[test]
    fn converts_anthropic_tool_blocks_to_openai_messages() {
        let input = json!([{
            "role": "assistant",
            "content": [{"type": "tool_use", "id": "toolu_1", "name": "lookup", "input": {"city": "Paris"}}]
        }, {
            "role": "user",
            "content": [{"type": "tool_result", "tool_use_id": "toolu_1", "content": [{"type": "text", "text": "sunny"}]}]
        }]);

        let output = anthropic_to_openai_messages(&input);

        assert_eq!(output[0]["role"], "assistant");
        assert_eq!(output[0]["content"], Value::Null);
        assert_eq!(
            output[0]["tool_calls"][0]["function"]["arguments"],
            "{\"city\":\"Paris\"}"
        );
        assert_eq!(
            output[1],
            json!({"role": "tool", "tool_call_id": "toolu_1", "content": "sunny"})
        );
    }

    #[test]
    fn converts_all_parallel_tool_results_to_tool_messages() {
        // Regression: Claude Code sends parallel tool calls; the user message
        // then carries one tool_result block per tool_use. Dropping all but the
        // first made upstream reject the request with
        // "tool_call_ids did not have response messages: Bash:2".
        let input = json!([{
            "role": "assistant",
            "content": [
                {"type": "tool_use", "id": "Bash:1", "name": "Bash", "input": {"command": "ls"}},
                {"type": "tool_use", "id": "Bash:2", "name": "Bash", "input": {"command": "pwd"}}
            ]
        }, {
            "role": "user",
            "content": [
                {"type": "tool_result", "tool_use_id": "Bash:1", "content": [{"type": "text", "text": "file.txt"}]},
                {"type": "tool_result", "tool_use_id": "Bash:2", "content": [{"type": "text", "text": "/tmp"}]}
            ]
        }]);

        let output = anthropic_to_openai_messages(&input);

        assert_eq!(output[0]["role"], "assistant");
        assert_eq!(output[0]["tool_calls"].as_array().unwrap().len(), 2);
        assert_eq!(
            output[1],
            json!({"role": "tool", "tool_call_id": "Bash:1", "content": "file.txt"})
        );
        assert_eq!(
            output[2],
            json!({"role": "tool", "tool_call_id": "Bash:2", "content": "/tmp"})
        );
    }

    #[test]
    fn keeps_text_blocks_alongside_tool_results() {
        let input = json!([{
            "role": "user",
            "content": [
                {"type": "tool_result", "tool_use_id": "Bash:1", "content": [{"type": "text", "text": "ok"}]},
                {"type": "text", "text": "now continue"}
            ]
        }]);

        let output = anthropic_to_openai_messages(&input);

        assert_eq!(
            output[0],
            json!({"role": "tool", "tool_call_id": "Bash:1", "content": "ok"})
        );
        assert_eq!(
            output[1],
            json!({"role": "user", "content": "now continue"})
        );
    }

    #[test]
    fn converts_openai_chat_response_to_anthropic_shape() {
        let input = json!({
            "id": "chatcmpl_1",
            "model": "fireworks/llama",
            "choices": [{
                "message": {
                    "content": "Hello",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "lookup", "arguments": "{\"q\":\"rust\"}"}
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 3, "completion_tokens": 4}
        });

        let output = openai_chat_to_anthropic_response(&input, "fallback-model");

        assert_eq!(output["id"], "chatcmpl_1");
        assert_eq!(output["model"], "fireworks/llama");
        assert_eq!(
            output["content"][0],
            json!({"type": "text", "text": "Hello"})
        );
        assert_eq!(output["content"][1]["type"], "tool_use");
        assert_eq!(output["content"][1]["input"]["q"], "rust");
        assert_eq!(output["stop_reason"], "tool_use");
        assert_eq!(
            output["usage"],
            json!({"input_tokens": 3, "output_tokens": 4})
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
    fn converts_anthropic_tool_use_blocks_to_openai_tool_calls() {
        let input = json!({
            "id": "msg_tool",
            "content": [{
                "type": "tool_use",
                "id": "toolu_123",
                "name": "lookup",
                "input": {"q": "rust"}
            }],
            "stop_reason": "tool_use"
        });

        let output = anthropic_to_openai_response(&input, "claude-test");

        assert_eq!(output["choices"][0]["message"]["content"], "");
        assert_eq!(output["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(
            output["choices"][0]["message"]["tool_calls"][0],
            json!({
                "id": "toolu_123",
                "type": "function",
                "function": {"name": "lookup", "arguments": "{\"q\":\"rust\"}"}
            })
        );
    }

    #[test]
    fn maps_anthropic_stop_reasons_to_openai_finish_reasons() {
        for (input_reason, expected) in [
            ("end_turn", "stop"),
            ("stop_sequence", "stop"),
            ("max_tokens", "length"),
            ("tool_use", "tool_calls"),
        ] {
            let input = json!({"content": [], "stop_reason": input_reason});
            let output = anthropic_to_openai_response(&input, "claude-test");

            assert_eq!(output["choices"][0]["finish_reason"], expected);
        }
    }

    #[test]
    fn maps_anthropic_usage_to_openai_usage() {
        let input = json!({
            "content": [],
            "usage": {"input_tokens": 11, "output_tokens": 7}
        });

        let output = anthropic_to_openai_response(&input, "claude-test");

        assert_eq!(output["usage"]["prompt_tokens"], 11);
        assert_eq!(output["usage"]["completion_tokens"], 7);
        assert_eq!(output["usage"]["total_tokens"], 18);
    }

    #[test]
    fn preserves_anthropic_thinking_blocks_as_reasoning_fields() {
        let input = json!({
            "content": [
                {"type": "thinking", "thinking": "first ", "signature": "sig_1"},
                {"type": "redacted_thinking", "data": "encrypted"},
                {"type": "text", "text": "answer"}
            ]
        });

        let output = anthropic_to_openai_response(&input, "claude-test");

        assert_eq!(output["choices"][0]["message"]["content"], "answer");
        assert_eq!(
            output["choices"][0]["message"]["reasoning_content"],
            "first "
        );
        assert_eq!(
            output["choices"][0]["message"]["reasoning_details"][0]["type"],
            "thinking"
        );
        assert_eq!(
            output["choices"][0]["message"]["reasoning_details"][1]["type"],
            "redacted_thinking"
        );
    }

    #[test]
    fn preserves_extra_anthropic_response_fields() {
        let input = json!({
            "id": "msg_extra",
            "type": "message",
            "content": [{"type": "text", "text": "Hello"}],
            "container": {"id": "container_123"},
            "service_tier": "standard_only"
        });

        let output = anthropic_to_openai_response(&input, "claude-test");

        assert_eq!(output["container"], json!({"id": "container_123"}));
        assert_eq!(output["service_tier"], "standard_only");
        assert_eq!(output.get("type"), None);
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
