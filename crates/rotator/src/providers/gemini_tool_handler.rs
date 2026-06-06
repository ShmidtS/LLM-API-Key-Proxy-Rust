use models::chat::{ToolChoice, ToolDefinition};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const GEMINI3_TOOL_PREFIX: &str = "gemini3_";

#[allow(dead_code)]
static GEMINI3_TOOL_RENAMES: &[(&str, &str)] = &[];
static GEMINI3_TOOL_RENAMES_REVERSE: &[(&str, &str)] = &[];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GeminiTool {
    #[serde(rename = "functionDeclarations")]
    pub function_declarations: Vec<GeminiFunctionDeclaration>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GeminiFunctionDeclaration {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(
        rename = "parameters",
        skip_serializing_if = "Option::is_none"
    )]
    pub parameters: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GeminiToolConfig {
    #[serde(rename = "functionCallingConfig")]
    pub function_calling_config: GeminiFunctionCallingConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GeminiFunctionCallingConfig {
    pub mode: String,
    #[serde(
        rename = "allowedFunctionNames",
        skip_serializing_if = "Option::is_none"
    )]
    pub allowed_function_names: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GroupedToolResponse {
    pub role: String,
    pub parts: Vec<serde_json::Value>,
}

/// Transform OpenAI tool definitions to Gemini `tools` format.
pub fn transform_tools_to_gemini(tools: &[ToolDefinition]) -> Vec<GeminiTool> {
    let declarations: Vec<GeminiFunctionDeclaration> = tools
        .iter()
        .map(|tool| GeminiFunctionDeclaration {
            name: tool.function.name.clone(),
            description: tool.function.description.clone(),
            parameters: Some(tool.function.parameters.clone()),
        })
        .collect();

    if declarations.is_empty() {
        vec![]
    } else {
        vec![GeminiTool {
            function_declarations: declarations,
        }]
    }
}

/// Translate an OpenAI `tool_choice` into Gemini `toolConfig`.
pub fn transform_tool_choice_to_gemini(tool_choice: &ToolChoice) -> Option<GeminiToolConfig> {
    let config = match tool_choice {
        ToolChoice::String(s) => match s.as_str() {
            "auto" => GeminiFunctionCallingConfig {
                mode: "AUTO".to_owned(),
                allowed_function_names: None,
            },
            "none" => GeminiFunctionCallingConfig {
                mode: "NONE".to_owned(),
                allowed_function_names: None,
            },
            "required" => GeminiFunctionCallingConfig {
                mode: "ANY".to_owned(),
                allowed_function_names: None,
            },
            _ => return None,
        },
        ToolChoice::Object { r#type: _, function } => GeminiFunctionCallingConfig {
            mode: "ANY".to_owned(),
            allowed_function_names: Some(vec![function.name.clone()]),
        },
    };

    Some(GeminiToolConfig {
        function_calling_config: config,
    })
}

/// Group Gemini-format function calls with their responses by ID.
///
/// Converts linear format `(call, response, call, response)` into grouped
/// format `(model with calls, user with all responses)` while preserving
/// ID-based pairing.
pub fn group_tool_responses(
    contents: &[serde_json::Value],
) -> Vec<serde_json::Value> {
    let mut new_contents: Vec<serde_json::Value> = Vec::new();
    #[derive(Debug)]
    struct PendingGroup {
        ids: Vec<String>,
        func_names: Vec<String>,
        insert_after_idx: usize,
    }
    let mut pending_groups: Vec<PendingGroup> = Vec::new();
    let mut collected_responses: HashMap<String, serde_json::Value> = HashMap::new();

    for content in contents {
        let role = content
            .get("role")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let parts = content
            .get("parts")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        let response_parts: Vec<&serde_json::Value> = parts
            .iter()
            .filter(|p| p.get("functionResponse").is_some())
            .collect();

        if !response_parts.is_empty() {
            for resp in response_parts {
                let resp_id = resp
                    .get("functionResponse")
                    .and_then(|f| f.get("id"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_owned();
                if !resp_id.is_empty() && !collected_responses.contains_key(&resp_id) {
                    collected_responses.insert(resp_id, resp.clone());
                }
            }

            for i in (0..pending_groups.len()).rev() {
                let group = &pending_groups[i];
                if group
                    .ids
                    .iter()
                    .all(|gid| collected_responses.contains_key(gid))
                {
                    let group_responses: Vec<serde_json::Value> = group
                        .ids
                        .iter()
                        .map(|gid| collected_responses.remove(gid).unwrap())
                        .collect();
                    new_contents.push(serde_json::json!({
                        "parts": group_responses,
                        "role": "user",
                    }));
                    pending_groups.remove(i);
                    break;
                }
            }
            continue;
        }

        if role == "model" {
            let func_calls: Vec<&serde_json::Value> = parts
                .iter()
                .filter(|p| p.get("functionCall").is_some())
                .collect();
            new_contents.push(content.clone());
            if !func_calls.is_empty() {
                let call_ids: Vec<String> = func_calls
                    .iter()
                    .map(|fc| {
                        fc.get("functionCall")
                            .and_then(|f| f.get("id"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_owned()
                    })
                    .filter(|id| !id.is_empty())
                    .collect();
                let func_names: Vec<String> = func_calls
                    .iter()
                    .map(|fc| {
                        fc.get("functionCall")
                            .and_then(|f| f.get("name"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_owned()
                    })
                    .collect();
                if !call_ids.is_empty() {
                    pending_groups.push(PendingGroup {
                        ids: call_ids,
                        func_names,
                        insert_after_idx: new_contents.len() - 1,
                    });
                }
            }
        } else {
            new_contents.push(content.clone());
        }
    }

    pending_groups.sort_by_key(|b| std::cmp::Reverse(b.insert_after_idx));

    for group in pending_groups {
        let insert_idx = group.insert_after_idx + 1;
        let mut group_responses: Vec<serde_json::Value> = Vec::new();

        for (i, expected_id) in group.ids.iter().enumerate() {
            let expected_name = group
                .func_names
                .get(i)
                .cloned()
                .unwrap_or_default();

            if let Some(resp) = collected_responses.remove(expected_id) {
                group_responses.push(resp);
            } else if !collected_responses.is_empty() {
                let mut matched_orphan_id: Option<String> = None;

                for (orphan_id, orphan_resp) in &collected_responses {
                    let orphan_name = orphan_resp
                        .get("functionResponse")
                        .and_then(|f| f.get("name"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if orphan_name == expected_name && !expected_name.is_empty() {
                        matched_orphan_id = Some(orphan_id.clone());
                        break;
                    }
                }

                if matched_orphan_id.is_none() {
                    for (orphan_id, orphan_resp) in &collected_responses {
                        let orphan_name = orphan_resp
                            .get("functionResponse")
                            .and_then(|f| f.get("name"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        if orphan_name == "unknown_function" {
                            matched_orphan_id = Some(orphan_id.clone());
                            break;
                        }
                    }
                }

                if matched_orphan_id.is_none() {
                    matched_orphan_id = collected_responses.keys().next().cloned();
                }

                if let Some(oid) = matched_orphan_id {
                    let mut orphan_resp = collected_responses.remove(&oid).unwrap();
                    if let Some(obj) = orphan_resp.get_mut("functionResponse")
                        && let Some(map) = obj.as_object_mut() {
                            let old_id = map
                                .get("id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_owned();
                            map.insert("id".to_owned(), serde_json::Value::String(expected_id.clone()));
                            if let Some(name) = map.get("name").and_then(|v| v.as_str())
                                && name == "unknown_function" && !expected_name.is_empty() {
                                    map.insert(
                                        "name".to_owned(),
                                        serde_json::Value::String(expected_name.clone()),
                                    );
                                }
                            if old_id != *expected_id {
                                tracing::warn!(
                                    "Auto-repaired ID mismatch: mapped response '{}' to call '{}' (function: {})",
                                    old_id,
                                    expected_id,
                                    expected_name
                                );
                            }
                        }
                    group_responses.push(orphan_resp);
                }
            } else {
                let placeholder = serde_json::json!({
                    "functionResponse": {
                        "name": if expected_name.is_empty() { "unknown_function" } else { &expected_name },
                        "response": {
                            "result": {
                                "error": "Tool response was lost during context processing. This is a recovered placeholder.",
                                "recovered": true,
                            }
                        },
                        "id": expected_id,
                    }
                });
                group_responses.push(placeholder);
            }
        }

        if !group_responses.is_empty() {
            new_contents.insert(
                insert_idx,
                serde_json::json!({
                    "parts": group_responses,
                    "role": "user",
                }),
            );
        }
    }

    if !collected_responses.is_empty() {
        tracing::warn!(
            "{} unmatched responses remaining: ids={:?}",
            collected_responses.len(),
            collected_responses.keys().collect::<Vec<_>>()
        );
    }

    new_contents
}

/// Strip the Gemini 3 namespace prefix from a tool name.
/// Also reverses any tool renames that were applied to avoid Gemini conflicts.
pub fn strip_gemini3_prefix(name: &str) -> String {
    if let Some(stripped) = name.strip_prefix(GEMINI3_TOOL_PREFIX) {
        let mut result = stripped.to_owned();
        for (renamed, original) in GEMINI3_TOOL_RENAMES_REVERSE {
            if result == *renamed {
                result = (*original).to_owned();
                break;
            }
        }
        return result;
    }
    name.to_owned()
}

/// Append a structured parameter signature to a tool's description.
///
/// The `template` should contain a `{params}` placeholder which will be
/// replaced with the formatted parameter list.
pub fn inject_tool_signature(
    declaration: &mut GeminiFunctionDeclaration,
    template: &str,
) {
    let schema = declaration
        .parameters
        .clone()
        .unwrap_or(serde_json::json!({}));
    let properties = schema
        .get("properties")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    let required = schema
        .get("required")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let required_set: std::collections::HashSet<String> = required
        .iter()
        .filter_map(|v| v.as_str().map(ToOwned::to_owned))
        .collect();

    if properties.is_empty() {
        return;
    }

    let mut param_list: Vec<String> = Vec::new();
    for (prop_name, prop_data) in &properties {
        let type_hint = format_type_hint(prop_data);
        let is_required = required_set.contains(prop_name);
        param_list.push(format!(
            "{} ({}{})",
            prop_name,
            type_hint,
            if is_required { ", REQUIRED" } else { "" }
        ));
    }

    if !param_list.is_empty() {
        let sig_str = template.replace("{params}", &param_list.join(", "));
        let existing = declaration.description.clone().unwrap_or_default();
        declaration.description = Some(format!("{}{}", existing, sig_str));
    }
}

fn format_type_hint(prop_data: &serde_json::Value) -> String {
    let prop_obj = match prop_data.as_object() {
        Some(o) => o,
        None => return "unknown".to_owned(),
    };

    if let Some(enum_vals) = prop_obj.get("enum").and_then(|v| v.as_array()) {
        if enum_vals.len() <= 5 {
            let vals: Vec<String> = enum_vals
                .iter()
                .map(|v| match v.as_str() {
                    Some(s) => format!("'{}'", s),
                    None => v.to_string(),
                })
                .collect();
            return format!("string ENUM[{}]", vals.join(", "));
        }
        return format!("string ENUM[{} options]", enum_vals.len());
    }

    if let Some(const_val) = prop_obj.get("const") {
        let formatted = match const_val.as_str() {
            Some(s) => format!("'{}'", s),
            None => const_val.to_string(),
        };
        return format!("string CONST={}", formatted);
    }

    let type_hint = prop_obj
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    if type_hint == "array" {
        if let Some(items) = prop_obj.get("items").and_then(|v| v.as_object()) {
            let item_type = items
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            if item_type == "object" {
                let nested_props = items
                    .get("properties")
                    .and_then(|v| v.as_object())
                    .cloned()
                    .unwrap_or_default();
                if !nested_props.is_empty() {
                    let nested_list: Vec<String> = nested_props
                        .iter()
                        .map(|(n, d)| {
                            let t = format_type_hint(d);
                            let req = items
                                .get("required")
                                .and_then(|r| r.as_array())
                                .map(|reqs| reqs.contains(&serde_json::json!(n)))
                                .unwrap_or(false);
                            format!("{}: {}{}", n, t, if req { " REQUIRED" } else { "" })
                        })
                        .collect();
                    return format!("ARRAY_OF_OBJECTS[{}]", nested_list.join(", "));
                }
                return "ARRAY_OF_OBJECTS".to_owned();
            }
            return format!("ARRAY_OF_{}", item_type.to_uppercase());
        }
        return "ARRAY".to_owned();
    }

    if type_hint == "object" {
        let nested_props = prop_obj
            .get("properties")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        let nested_req = prop_obj
            .get("required")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        if !nested_props.is_empty() {
            let nested_list: Vec<String> = nested_props
                .iter()
                .map(|(n, d)| {
                    let t = d
                        .get("type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    let req = nested_req.contains(&serde_json::json!(n));
                    format!("{}: {}{}", n, t, if req { " REQUIRED" } else { "" })
                })
                .collect();
            return format!("object{{{}}}", nested_list.join(", "));
        }
    }

    type_hint.to_owned()
}

/// Enforce strict JSON schema by adding `additionalProperties: false` to
/// object schemas that have `properties`, while preserving truthy
/// `additionalProperties` values already present in the schema.
pub fn enforce_strict_schema(schema: &mut serde_json::Value) {
    if let Some(obj) = schema.as_object_mut() {
        let mut preserved_additional_props: Option<serde_json::Value> = None;
        let mut keys_to_remove: Vec<String> = Vec::new();

        for (key, value) in obj.iter_mut() {
            if key == "additionalProperties" {
                if let Some(v) = value.as_bool() {
                    if !v {
                        keys_to_remove.push(key.clone());
                    } else {
                        preserved_additional_props = Some(value.clone());
                    }
                } else {
                    preserved_additional_props = Some(value.clone());
                }
                continue;
            }
            enforce_strict_schema(value);
        }

        for key in keys_to_remove {
            obj.remove(&key);
        }

        if obj.get("type").and_then(|v| v.as_str()) == Some("object")
            && obj.contains_key("properties")
        {
            if let Some(val) = preserved_additional_props {
                obj.insert("additionalProperties".to_owned(), val);
            } else {
                obj.insert(
                    "additionalProperties".to_owned(),
                    serde_json::Value::Bool(false),
                );
            }
        }
    } else if let Some(arr) = schema.as_array_mut() {
        for item in arr {
            enforce_strict_schema(item);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use models::chat::FunctionDefinition;
    use serde_json::json;

    fn make_tool(name: &str, description: &str, parameters: serde_json::Value) -> ToolDefinition {
        ToolDefinition {
            r#type: "function".to_owned(),
            function: FunctionDefinition {
                name: name.to_owned(),
                description: Some(description.to_owned()),
                parameters,
            },
        }
    }

    #[test]
    fn transform_tools_to_gemini_basic() {
        let tools = vec![make_tool(
            "get_weather",
            "Get the weather",
            json!({
                "type": "object",
                "properties": {
                    "location": {"type": "string"}
                },
                "required": ["location"]
            }),
        )];

        let gemini = transform_tools_to_gemini(&tools);
        assert_eq!(gemini.len(), 1);
        assert_eq!(gemini[0].function_declarations.len(), 1);
        assert_eq!(gemini[0].function_declarations[0].name, "get_weather");
        assert_eq!(
            gemini[0].function_declarations[0].description,
            Some("Get the weather".to_owned())
        );
        assert!(gemini[0].function_declarations[0].parameters.is_some());
    }

    #[test]
    fn transform_tool_choice_auto() {
        let tc = ToolChoice::String("auto".to_owned());
        let config = transform_tool_choice_to_gemini(&tc).unwrap();
        assert_eq!(config.function_calling_config.mode, "AUTO");
        assert!(config.function_calling_config.allowed_function_names.is_none());
    }

    #[test]
    fn transform_tool_choice_none() {
        let tc = ToolChoice::String("none".to_owned());
        let config = transform_tool_choice_to_gemini(&tc).unwrap();
        assert_eq!(config.function_calling_config.mode, "NONE");
    }

    #[test]
    fn transform_tool_choice_required() {
        let tc = ToolChoice::String("required".to_owned());
        let config = transform_tool_choice_to_gemini(&tc).unwrap();
        assert_eq!(config.function_calling_config.mode, "ANY");
    }

    #[test]
    fn transform_tool_choice_function() {
        let tc = ToolChoice::Object {
            r#type: "function".to_owned(),
            function: FunctionDefinition {
                name: "get_weather".to_owned(),
                description: None,
                parameters: json!({}),
            },
        };
        let config = transform_tool_choice_to_gemini(&tc).unwrap();
        assert_eq!(config.function_calling_config.mode, "ANY");
        assert_eq!(
            config.function_calling_config.allowed_function_names,
            Some(vec!["get_weather".to_owned()])
        );
    }

    #[test]
    fn group_tool_responses_basic() {
        let contents = vec![
            json!({
                "role": "model",
                "parts": [{"functionCall": {"id": "call_1", "name": "get_weather"}}]
            }),
            json!({
                "role": "user",
                "parts": [{"functionResponse": {"id": "call_1", "name": "get_weather", "response": {"result": "sunny"}}}]
            }),
        ];

        let grouped = group_tool_responses(&contents);
        assert_eq!(grouped.len(), 2);
        assert_eq!(grouped[0]["role"], "model");
        assert_eq!(grouped[1]["role"], "user");
        let parts = grouped[1]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["functionResponse"]["id"], "call_1");
    }

    #[test]
    fn group_tool_responses_with_recovery() {
        let contents = vec![
            json!({
                "role": "model",
                "parts": [{"functionCall": {"id": "call_1", "name": "get_weather"}}]
            }),
            json!({
                "role": "user",
                "parts": [{"functionResponse": {"id": "mismatch", "name": "get_weather", "response": {"result": "sunny"}}}]
            }),
        ];

        let grouped = group_tool_responses(&contents);
        assert_eq!(grouped.len(), 2);
        let parts = grouped[1]["parts"].as_array().unwrap();
        assert_eq!(parts[0]["functionResponse"]["id"], "call_1");
    }

    #[test]
    fn group_tool_responses_placeholder_for_missing() {
        let contents = vec![
            json!({
                "role": "model",
                "parts": [{"functionCall": {"id": "call_1", "name": "get_weather"}}]
            }),
        ];

        let grouped = group_tool_responses(&contents);
        assert_eq!(grouped.len(), 2);
        let parts = grouped[1]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["functionResponse"]["id"], "call_1");
        assert_eq!(parts[0]["functionResponse"]["name"], "get_weather");
        assert!(
            parts[0]["functionResponse"]["response"]["result"]["recovered"]
                .as_bool()
                .unwrap()
        );
    }

    #[test]
    fn strip_gemini3_prefix_no_prefix() {
        assert_eq!(strip_gemini3_prefix("get_weather"), "get_weather");
    }

    #[test]
    fn strip_gemini3_prefix_strips() {
        assert_eq!(strip_gemini3_prefix("gemini3_get_weather"), "get_weather");
    }

    #[test]
    fn inject_tool_signature_appends() {
        let mut decl = GeminiFunctionDeclaration {
            name: "get_weather".to_owned(),
            description: Some("Get weather.".to_owned()),
            parameters: Some(json!({
                "type": "object",
                "properties": {
                    "location": {"type": "string"},
                    "unit": {"type": "string", "enum": ["celsius", "fahrenheit"]}
                },
                "required": ["location"]
            })),
        };

        inject_tool_signature(&mut decl, "\nParameters: {params}");
        let desc = decl.description.unwrap();
        assert!(desc.contains("Get weather."));
        assert!(desc.contains("location"));
        assert!(desc.contains("REQUIRED"));
        assert!(desc.contains("ENUM"));
    }

    #[test]
    fn enforce_strict_schema_adds_false() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "name": {"type": "string"}
            }
        });
        enforce_strict_schema(&mut schema);
        assert_eq!(schema["additionalProperties"], false);
    }

    #[test]
    fn enforce_strict_schema_preserves_truthy() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "name": {"type": "string"}
            },
            "additionalProperties": true
        });
        enforce_strict_schema(&mut schema);
        assert_eq!(schema["additionalProperties"], true);
    }

    #[test]
    fn enforce_strict_schema_preserves_object_additional_props() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "name": {"type": "string"}
            },
            "additionalProperties": {"type": "string"}
        });
        enforce_strict_schema(&mut schema);
        assert_eq!(schema["additionalProperties"], json!({"type": "string"}));
    }

    #[test]
    fn format_type_hint_enum() {
        let prop = json!({"type": "string", "enum": ["a", "b", "c"]});
        assert_eq!(
            format_type_hint(&prop),
            "string ENUM['a', 'b', 'c']"
        );
    }

    #[test]
    fn format_type_hint_const() {
        let prop = json!({"const": "fixed"});
        assert_eq!(format_type_hint(&prop), "string CONST='fixed'");
    }

    #[test]
    fn format_type_hint_array_of_objects() {
        let prop = json!({
            "type": "array",
            "items": {
                "type": "object",
                "properties": {
                    "x": {"type": "number"}
                },
                "required": ["x"]
            }
        });
        assert_eq!(format_type_hint(&prop), "ARRAY_OF_OBJECTS[x: number REQUIRED]");
    }

    #[test]
    fn format_type_hint_nested_object() {
        let prop = json!({
            "type": "object",
            "properties": {
                "inner": {"type": "string"}
            },
            "required": ["inner"]
        });
        assert_eq!(format_type_hint(&prop), "object{inner: string REQUIRED}");
    }
}
