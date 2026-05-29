use std::collections::HashMap;

use models::chat::{FunctionDefinition, ResponseFormat, ToolChoice, ToolDefinition};
use models::responses::{ResponseTextConfig, ResponseTextFormat, ResponseTool, ResponseToolChoice};
use serde_json::{Value, json};

use super::error::{ResponsesBridgeError, Result};

pub fn response_tool_to_chat_tool(tool: &ResponseTool) -> Result<ToolDefinition> {
    if tool.type_ != "function" {
        return Err(ResponsesBridgeError::UnsupportedToolType {
            tool_type: tool.type_.clone(),
        });
    }
    let function = tool
        .function
        .clone()
        .ok_or(ResponsesBridgeError::MissingFunctionDefinition)?;
    Ok(ToolDefinition {
        r#type: "function".to_owned(),
        function,
    })
}

pub fn response_tool_choice_to_chat_tool_choice(choice: &ResponseToolChoice) -> Result<ToolChoice> {
    Ok(match choice {
        ResponseToolChoice::Auto => ToolChoice::String("auto".to_owned()),
        ResponseToolChoice::Required => ToolChoice::String("required".to_owned()),
        ResponseToolChoice::None_ => ToolChoice::String("none".to_owned()),
        ResponseToolChoice::Named(choice) => {
            if choice.type_ != "function" {
                return Err(ResponsesBridgeError::InvalidToolChoice {
                    reason: format!("unsupported named tool choice type: {}", choice.type_),
                });
            }
            ToolChoice::Object {
                r#type: "function".to_owned(),
                function: FunctionDefinition {
                    name: choice.function.name.clone(),
                    description: None,
                    parameters: json!({}),
                },
            }
        }
    })
}

pub fn response_text_config_to_chat_response_format(
    text: Option<&ResponseTextConfig>,
    extra: &mut HashMap<String, Value>,
) -> Option<ResponseFormat> {
    match text.and_then(|text| text.format.as_ref()) {
        Some(ResponseTextFormat::PlainText) | None => None,
        Some(ResponseTextFormat::JsonObject { schema }) => {
            if let Some(schema) = schema.clone() {
                extra.insert(
                    "response_format".to_owned(),
                    json!({ "type": "json_schema", "json_schema": schema }),
                );
                None
            } else {
                Some(ResponseFormat {
                    r#type: "json_object".to_owned(),
                })
            }
        }
    }
}
