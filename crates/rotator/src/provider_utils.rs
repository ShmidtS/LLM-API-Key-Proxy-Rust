pub fn extract_usage(body: &serde_json::Value) -> (usize, usize) {
    let Some(usage) = body.get("usage") else {
        return (0, 0);
    };

    let input_tokens = usage
        .get("prompt_tokens")
        .or_else(|| usage.get("input_tokens"))
        .and_then(serde_json::Value::as_u64)
        .map(|value| value as usize)
        .unwrap_or(0);
    let output_tokens = usage
        .get("completion_tokens")
        .or_else(|| usage.get("output_tokens"))
        .and_then(serde_json::Value::as_u64)
        .map(|value| value as usize)
        .unwrap_or(0);

    (input_tokens, output_tokens)
}

pub fn transform_tool_schema(tool: &mut serde_json::Value) {
    let Some(object) = tool.as_object_mut() else {
        return;
    };

    if object.get("type").and_then(serde_json::Value::as_str) == Some("function") {
        object.insert(
            "type".to_string(),
            serde_json::Value::String("tool".to_string()),
        );
    }

    let Some(function) = object.remove("function") else {
        return;
    };
    let Some(function_object) = function.as_object() else {
        object.insert("tool".to_string(), function);
        return;
    };

    for field in ["name", "description"] {
        if let Some(value) = function_object.get(field) {
            object.insert(field.to_string(), value.clone());
        }
    }

    if let Some(parameters) = function_object.get("parameters") {
        object.insert("input_schema".to_string(), parameters.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_openai_usage() {
        let usage = extract_usage(&serde_json::json!({
            "usage": {"prompt_tokens": 11, "completion_tokens": 7}
        }));

        assert_eq!(usage, (11, 7));
    }

    #[test]
    fn extracts_anthropic_usage() {
        let usage = extract_usage(&serde_json::json!({
            "usage": {"input_tokens": 13, "output_tokens": 5}
        }));

        assert_eq!(usage, (13, 5));
    }

    #[test]
    fn missing_usage_returns_zeroes() {
        assert_eq!(extract_usage(&serde_json::json!({})), (0, 0));
    }

    #[test]
    fn transforms_openai_function_tool_to_anthropic_tool() {
        let mut tool = serde_json::json!({
            "type": "function",
            "function": {
                "name": "lookup",
                "description": "Lookup data",
                "parameters": {
                    "type": "object",
                    "properties": {"query": {"type": "string"}}
                }
            }
        });

        transform_tool_schema(&mut tool);

        assert_eq!(tool["type"], "tool");
        assert_eq!(tool["name"], "lookup");
        assert_eq!(tool["description"], "Lookup data");
        assert!(tool.get("function").is_none());
        assert_eq!(tool["input_schema"]["type"], "object");
    }
}
