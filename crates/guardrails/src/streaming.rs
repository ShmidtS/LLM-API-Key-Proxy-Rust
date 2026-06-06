use crate::error::GuardrailError;
use crate::types::{GuardrailTrace, ValidationIssue};
use serde_json::Value;
use std::collections::BTreeMap;

pub trait StreamValidator: Send + Sync {
    fn validate_frame(
        &self,
        frame: &Value,
        trace: &mut GuardrailTrace,
    ) -> Result<(), GuardrailError>;
    fn finish(&self, trace: &mut GuardrailTrace) -> Result<Value, GuardrailError>;
}

#[derive(Debug, Clone, Default)]
pub struct NoOpStreamValidator;

impl StreamValidator for NoOpStreamValidator {
    fn validate_frame(
        &self,
        _frame: &Value,
        _trace: &mut GuardrailTrace,
    ) -> Result<(), GuardrailError> {
        Ok(())
    }

    fn finish(&self, _trace: &mut GuardrailTrace) -> Result<Value, GuardrailError> {
        Ok(Value::Null)
    }
}

#[derive(Debug, Default)]
pub struct BufferingStreamValidator {
    frames: std::sync::Mutex<Vec<Value>>,
}

impl StreamValidator for BufferingStreamValidator {
    fn validate_frame(
        &self,
        frame: &Value,
        _trace: &mut GuardrailTrace,
    ) -> Result<(), GuardrailError> {
        self.frames.lock().unwrap().push(frame.clone());
        Ok(())
    }

    fn finish(&self, _trace: &mut GuardrailTrace) -> Result<Value, GuardrailError> {
        let mut guard = self.frames.lock().unwrap();
        let frames = std::mem::take(&mut *guard);
        Ok(accumulate_chat_completion_chunks(&frames))
    }
}

pub fn accumulate_chat_completion_chunks(frames: &[Value]) -> Value {
    let mut id = String::new();
    let mut created: i64 = 0;
    let mut model = String::new();
    let mut role = String::new();
    let mut content = String::new();
    let mut tool_calls: BTreeMap<u64, serde_json::Map<String, Value>> = BTreeMap::new();
    let mut finish_reason = String::new();
    let mut usage = Value::Null;

    for frame in frames {
        if id.is_empty() {
            if let Some(v) = frame.get("id").and_then(Value::as_str) {
                id = v.to_owned();
            }
            if let Some(v) = frame.get("created").and_then(Value::as_i64) {
                created = v;
            }
            if let Some(v) = frame.get("model").and_then(Value::as_str) {
                model = v.to_owned();
            }
        }
        if let Some(choices) = frame.get("choices").and_then(Value::as_array) {
            for choice in choices {
                if let Some(delta) = choice.get("delta") {
                    if let Some(r) = delta.get("role").and_then(Value::as_str) {
                        role = r.to_owned();
                    }
                    if let Some(c) = delta.get("content").and_then(Value::as_str) {
                        content.push_str(c);
                    }
                    if let Some(tcs) = delta.get("tool_calls").and_then(Value::as_array) {
                        for tc in tcs {
                            let index = tc.get("index").and_then(Value::as_u64).unwrap_or(0);
                            let entry = tool_calls.entry(index).or_default();
                            if let Some(id_val) = tc.get("id").and_then(Value::as_str) {
                                entry.insert("id".to_string(), Value::String(id_val.to_owned()));
                            }
                            if let Some(t) = tc.get("type").and_then(Value::as_str) {
                                entry.insert("type".to_string(), Value::String(t.to_owned()));
                            }
                            if let Some(func) = tc.get("function") {
                                let func_map = entry
                                    .entry("function")
                                    .or_insert_with(|| Value::Object(serde_json::Map::new()));
                                if let Value::Object(ref mut fmap) = *func_map {
                                    if let Some(name) = func.get("name").and_then(Value::as_str) {
                                        fmap.insert(
                                            "name".to_string(),
                                            Value::String(name.to_owned()),
                                        );
                                    }
                                    if let Some(args) =
                                        func.get("arguments").and_then(Value::as_str)
                                    {
                                        let existing = fmap
                                            .get("arguments")
                                            .and_then(Value::as_str)
                                            .unwrap_or("");
                                        fmap.insert(
                                            "arguments".to_string(),
                                            Value::String(format!("{}{}", existing, args)),
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
                if let Some(fr) = choice.get("finish_reason").and_then(Value::as_str) {
                    finish_reason = fr.to_owned();
                }
            }
        }
        if let Some(u) = frame.get("usage") {
            usage = u.clone();
        }
    }

    if id.is_empty() {
        id = "chatcmpl-guardrails".to_string();
    }
    if model.is_empty() {
        model = "unknown".to_string();
    }
    if role.is_empty() {
        role = "assistant".to_string();
    }
    if finish_reason.is_empty() {
        finish_reason = "stop".to_string();
    }

    let mut message = serde_json::Map::new();
    message.insert("role".to_string(), Value::String(role));
    if !content.is_empty() {
        message.insert("content".to_string(), Value::String(content));
    }
    let tool_calls_vec: Vec<Value> = tool_calls.into_values().map(Value::Object).collect();
    if !tool_calls_vec.is_empty() {
        message.insert("tool_calls".to_string(), Value::Array(tool_calls_vec));
    }

    let mut choice = serde_json::Map::new();
    choice.insert("index".to_string(), Value::Number(0.into()));
    choice.insert("message".to_string(), Value::Object(message));
    choice.insert("finish_reason".to_string(), Value::String(finish_reason));

    let mut body = serde_json::Map::new();
    body.insert("id".to_string(), Value::String(id));
    body.insert(
        "object".to_string(),
        Value::String("chat.completion".to_string()),
    );
    body.insert("created".to_string(), Value::Number(created.into()));
    body.insert("model".to_string(), Value::String(model));
    body.insert(
        "choices".to_string(),
        Value::Array(vec![Value::Object(choice)]),
    );
    if usage.is_null() {
        usage = serde_json::json!({"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0});
    }
    body.insert("usage".to_string(), usage);

    Value::Object(body)
}

pub fn chat_completion_to_sse_bytes(value: &Value) -> Vec<u8> {
    let mut chunks: Vec<String> = Vec::new();
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("chatcmpl-guardrails");
    let created = value.get("created").and_then(Value::as_i64).unwrap_or(0);
    let model = value
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let empty: Vec<Value> = Vec::new();
    let choices = value
        .get("choices")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    let choice = choices.first().and_then(Value::as_object);
    let message = choice
        .and_then(|c| c.get("message"))
        .and_then(Value::as_object);
    let finish_reason = choice.and_then(|c| c.get("finish_reason").and_then(Value::as_str));

    if let Some(role) = message.and_then(|m| m.get("role").and_then(Value::as_str)) {
        let chunk = serde_json::json!({
            "id": id,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "choices": [{"index": 0, "delta": {"role": role}, "finish_reason": null}]
        });
        chunks.push(format!("data: {chunk}\n\n"));
    }

    if let Some(content) = message.and_then(|m| m.get("content").and_then(Value::as_str))
        && !content.is_empty()
    {
        let chunk = serde_json::json!({
            "id": id,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "choices": [{"index": 0, "delta": {"content": content}, "finish_reason": null}]
        });
        chunks.push(format!("data: {chunk}\n\n"));
    }

    if let Some(tool_calls) = message
        .and_then(|m| m.get("tool_calls"))
        .and_then(Value::as_array)
    {
        for (i, tc) in tool_calls.iter().enumerate() {
            if let Some(tc_obj) = tc.as_object() {
                let mut tool_call = serde_json::Map::new();
                tool_call.insert("index".to_string(), Value::Number(i.into()));
                if let Some(id_val) = tc_obj.get("id").and_then(Value::as_str) {
                    tool_call.insert("id".to_string(), Value::String(id_val.to_owned()));
                }
                if let Some(t) = tc_obj.get("type").and_then(Value::as_str) {
                    tool_call.insert("type".to_string(), Value::String(t.to_owned()));
                }
                if let Some(func) = tc_obj.get("function").and_then(Value::as_object)
                    && let Some(name) = func.get("name").and_then(Value::as_str)
                {
                    let mut inner = serde_json::Map::new();
                    inner.insert("name".to_string(), Value::String(name.to_owned()));
                    tool_call.insert("function".to_string(), Value::Object(inner));
                }
                let delta = serde_json::json!({"tool_calls": [Value::Object(tool_call)]});
                let chunk = serde_json::json!({
                    "id": id,
                    "object": "chat.completion.chunk",
                    "created": created,
                    "model": model,
                    "choices": [{"index": 0, "delta": delta, "finish_reason": null}]
                });
                chunks.push(format!("data: {chunk}\n\n"));

                if let Some(func) = tc_obj.get("function").and_then(Value::as_object)
                    && let Some(args) = func.get("arguments").and_then(Value::as_str)
                {
                    let mut inner = serde_json::Map::new();
                    inner.insert("arguments".to_string(), Value::String(args.to_owned()));
                    let mut tool_call2 = serde_json::Map::new();
                    tool_call2.insert("index".to_string(), Value::Number(i.into()));
                    tool_call2.insert("function".to_string(), Value::Object(inner));
                    let delta2 = serde_json::json!({"tool_calls": [Value::Object(tool_call2)]});
                    let chunk2 = serde_json::json!({
                        "id": id,
                        "object": "chat.completion.chunk",
                        "created": created,
                        "model": model,
                        "choices": [{"index": 0, "delta": delta2, "finish_reason": null}]
                    });
                    chunks.push(format!("data: {chunk2}\n\n"));
                }
            }
        }
    }

    let final_delta = serde_json::Map::new();
    let chunk = serde_json::json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": [{"index": 0, "delta": final_delta, "finish_reason": finish_reason.unwrap_or("stop")}]
    });
    chunks.push(format!("data: {chunk}\n\n"));
    chunks.push("data: [DONE]\n\n".to_string());

    chunks.join("").into_bytes()
}

#[allow(dead_code)]
fn frame_issue(field: impl Into<String>, reason: impl Into<String>) -> ValidationIssue {
    ValidationIssue {
        field: field.into(),
        reason: reason.into(),
        severity: "warning".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn noop_accepts_any_frame() {
        let mut trace = GuardrailTrace::default();
        NoOpStreamValidator
            .validate_frame(&json!({"event":"delta"}), &mut trace)
            .unwrap();
        let result = NoOpStreamValidator.finish(&mut trace).unwrap();
        assert!(trace.issues.is_empty());
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn buffering_accumulates_content_chunks() {
        let validator = BufferingStreamValidator::default();
        let mut trace = GuardrailTrace::default();
        validator
            .validate_frame(
                &json!({
                    "id":"c1","object":"chat.completion.chunk","created":1,"model":"m",
                    "choices":[{"index":0,"delta":{"role":"assistant"},"finish_reason":null}]
                }),
                &mut trace,
            )
            .unwrap();
        validator
            .validate_frame(
                &json!({
                    "id":"c1","object":"chat.completion.chunk","created":1,"model":"m",
                    "choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}]
                }),
                &mut trace,
            )
            .unwrap();
        validator
            .validate_frame(
                &json!({
                    "id":"c1","object":"chat.completion.chunk","created":1,"model":"m",
                    "choices":[{"index":0,"delta":{},"finish_reason":"stop"}]
                }),
                &mut trace,
            )
            .unwrap();

        let result = validator.finish(&mut trace).unwrap();
        assert_eq!(result["id"], "c1");
        assert_eq!(result["object"], "chat.completion");
        assert_eq!(result["choices"][0]["message"]["role"], "assistant");
        assert_eq!(result["choices"][0]["message"]["content"], "Hello");
        assert_eq!(result["choices"][0]["finish_reason"], "stop");
    }

    #[test]
    fn buffering_accumulates_tool_call_chunks() {
        let validator = BufferingStreamValidator::default();
        let mut trace = GuardrailTrace::default();
        validator
            .validate_frame(
                &json!({
                    "id":"c1","object":"chat.completion.chunk","created":1,"model":"m",
                    "choices":[{"index":0,"delta":{"role":"assistant"},"finish_reason":null}]
                }),
                &mut trace,
            )
            .unwrap();
        validator.validate_frame(&json!({
            "id":"c1","object":"chat.completion.chunk","created":1,"model":"m",
            "choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"lookup"}}]},"finish_reason":null}]
        }), &mut trace).unwrap();
        validator.validate_frame(&json!({
            "id":"c1","object":"chat.completion.chunk","created":1,"model":"m",
            "choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{q: 'rust',}"}}]},"finish_reason":null}]
        }), &mut trace).unwrap();
        validator
            .validate_frame(
                &json!({
                    "id":"c1","object":"chat.completion.chunk","created":1,"model":"m",
                    "choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]
                }),
                &mut trace,
            )
            .unwrap();

        let result = validator.finish(&mut trace).unwrap();
        let tool_call = &result["choices"][0]["message"]["tool_calls"][0];
        assert_eq!(tool_call["id"], "call_1");
        assert_eq!(tool_call["type"], "function");
        assert_eq!(tool_call["function"]["name"], "lookup");
        assert_eq!(tool_call["function"]["arguments"], "{q: 'rust',}");
        assert_eq!(result["choices"][0]["finish_reason"], "tool_calls");
    }

    #[test]
    fn roundtrip_content_sse() {
        let body = json!({
            "id":"rt1","object":"chat.completion","created":2,"model":"m",
            "choices":[{"index":0,"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}]
        });
        let sse = chat_completion_to_sse_bytes(&body);
        let text = String::from_utf8(sse).unwrap();
        let records: Vec<&str> = text
            .lines()
            .filter(|l| l.starts_with("data: "))
            .map(|l| &l[6..])
            .collect();
        let frames: Vec<Value> = records
            .iter()
            .filter(|r| **r != "[DONE]")
            .map(|r| serde_json::from_str(r).unwrap())
            .collect();
        let accumulated = accumulate_chat_completion_chunks(&frames);
        assert_eq!(accumulated["choices"][0]["message"]["content"], "hi");
    }

    #[test]
    fn roundtrip_tool_call_sse() {
        let body = json!({
            "id":"rt2","object":"chat.completion","created":3,"model":"m",
            "choices":[{"index":0,"message":{"role":"assistant","tool_calls":[{"id":"c1","type":"function","function":{"name":"n","arguments":"{}"}}]},"finish_reason":"tool_calls"}]
        });
        let sse = chat_completion_to_sse_bytes(&body);
        let text = String::from_utf8(sse).unwrap();
        let records: Vec<&str> = text
            .lines()
            .filter(|l| l.starts_with("data: "))
            .map(|l| &l[6..])
            .collect();
        let frames: Vec<Value> = records
            .iter()
            .filter(|r| **r != "[DONE]")
            .map(|r| serde_json::from_str(r).unwrap())
            .collect();
        let accumulated = accumulate_chat_completion_chunks(&frames);
        let tc = &accumulated["choices"][0]["message"]["tool_calls"][0];
        assert_eq!(tc["function"]["name"], "n");
        assert_eq!(tc["function"]["arguments"], "{}");
    }
}
