use serde_json::{Value, json};

pub const RESPOND_TOOL_NAME: &str = "respond";

pub fn inject_respond_tool(request: &mut Value) {
    let Some(tools) = request.get_mut("tools").and_then(Value::as_array_mut) else {
        return;
    };
    if tools.is_empty() || tools.iter().any(is_respond_tool) {
        return;
    }

    tools.push(respond_tool_spec());
}

/// Убирает синтетические вызовы `respond` из ответа модели.
///
/// Возвращает `true`, если хотя бы один choice был сведён к чистому
/// текстовому ответу из единственного вызова `respond` (conversational-ответ
/// Forge), что освобождает его от JSON-mode валидации у вызывающей стороны.
pub fn strip_respond_tool_calls(response: &mut Value) -> bool {
    let Some(choices) = response.get_mut("choices").and_then(Value::as_array_mut) else {
        return false;
    };

    let mut converted_to_text = false;
    for choice in choices {
        let Some(message) = choice.get_mut("message").and_then(Value::as_object_mut) else {
            continue;
        };
        let Some(tool_calls) = message.get_mut("tool_calls").and_then(Value::as_array_mut) else {
            continue;
        };

        let mut respond_message = None;
        let mut real_tool_calls = Vec::new();
        for tool_call in tool_calls.iter() {
            if tool_call["function"]["name"] == RESPOND_TOOL_NAME {
                if respond_message.is_none() {
                    respond_message = Some(extract_respond_message(tool_call));
                }
            } else {
                real_tool_calls.push(tool_call.clone());
            }
        }

        if real_tool_calls.is_empty() {
            if let Some(content) = respond_message {
                message.insert("content".to_owned(), Value::String(content));
                message.remove("tool_calls");
                choice["finish_reason"] = Value::String("stop".to_owned());
                converted_to_text = true;
            }
        } else if real_tool_calls.len() != tool_calls.len() {
            *tool_calls = real_tool_calls;
        }
    }
    converted_to_text
}

fn is_respond_tool(tool: &Value) -> bool {
    tool["function"]["name"] == RESPOND_TOOL_NAME || tool["name"] == RESPOND_TOOL_NAME
}

fn respond_tool_spec() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": RESPOND_TOOL_NAME,
            "description": "Respond to the user with a message. Use this when the user is chatting, asking a question, when you need to ask a clarifying question before proceeding, or when no other tool action is needed. Also use this after completing the user's request to report the result.",
            "parameters": {
                "type": "object",
                "properties": {
                    "message": {
                        "type": "string",
                        "description": "The message to send to the user."
                    }
                },
                "required": ["message"]
            }
        }
    })
}

fn extract_respond_message(tool_call: &Value) -> String {
    let Some(arguments) = tool_call["function"]["arguments"].as_str() else {
        return String::new();
    };
    serde_json::from_str::<Value>(arguments)
        .ok()
        .and_then(|value| {
            value
                .get("message")
                .or_else(|| value.get("content"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_call(name: &str, arguments: &str) -> Value {
        json!({
            "id": "call_1",
            "type": "function",
            "function": {"name": name, "arguments": arguments}
        })
    }

    #[test]
    fn inject_adds_respond_when_tools_present() {
        let mut request = json!({
            "model": "gpt-4o",
            "tools": [{"type": "function", "function": {"name": "search"}}]
        });

        inject_respond_tool(&mut request);

        let tools = request["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 2);
        let respond = tools
            .iter()
            .find(|tool| tool["function"]["name"] == RESPOND_TOOL_NAME)
            .expect("respond tool injected");
        assert_eq!(
            respond["function"]["parameters"]["properties"]["message"]["type"],
            "string"
        );
    }

    #[test]
    fn inject_skips_when_no_tools() {
        let mut request = json!({"model": "gpt-4o"});
        inject_respond_tool(&mut request);
        assert!(request.get("tools").is_none());

        let mut empty = json!({"model": "gpt-4o", "tools": []});
        inject_respond_tool(&mut empty);
        assert_eq!(empty["tools"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn inject_does_not_duplicate_respond() {
        let mut request = json!({
            "model": "gpt-4o",
            "tools": [{"type": "function", "function": {"name": "search"}}]
        });
        inject_respond_tool(&mut request);
        inject_respond_tool(&mut request);
        let count = request["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|tool| tool["function"]["name"] == RESPOND_TOOL_NAME)
            .count();
        assert_eq!(count, 1);
    }

    #[test]
    fn strip_converts_sole_respond_call_to_text() {
        let mut response = json!({
            "choices": [{
                "index": 0,
                "finish_reason": "tool_calls",
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [tool_call(RESPOND_TOOL_NAME, "{\"message\":\"hi\"}")]
                }
            }]
        });

        strip_respond_tool_calls(&mut response);

        let choice = &response["choices"][0];
        assert_eq!(choice["message"]["content"], "hi");
        assert!(choice["message"].get("tool_calls").is_none());
        assert_eq!(choice["finish_reason"], "stop");
    }

    #[test]
    fn strip_leaves_real_tool_calls_untouched() {
        let mut response = json!({
            "choices": [{
                "index": 0,
                "finish_reason": "tool_calls",
                "message": {
                    "role": "assistant",
                    "tool_calls": [tool_call("search", "{\"q\":\"rust\"}")]
                }
            }]
        });
        let original = response.clone();

        strip_respond_tool_calls(&mut response);

        assert_eq!(response, original);
    }

    #[test]
    fn strip_removes_respond_but_keeps_real_call_in_mixed() {
        let mut response = json!({
            "choices": [{
                "index": 0,
                "finish_reason": "tool_calls",
                "message": {
                    "role": "assistant",
                    "tool_calls": [
                        tool_call(RESPOND_TOOL_NAME, "{\"message\":\"hi\"}"),
                        tool_call("search", "{\"q\":\"rust\"}")
                    ]
                }
            }]
        });

        strip_respond_tool_calls(&mut response);

        let tool_calls = response["choices"][0]["message"]["tool_calls"]
            .as_array()
            .unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0]["function"]["name"], "search");
    }
}
