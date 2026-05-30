use models::chat::{ChatMessageContent, FunctionCall, ToolCall};
use models::responses::{
    ResponseContentPart, ResponseInputContent, ResponseInputItem, ResponseOutputContent,
};
use serde_json::{Value, json};

use super::error::{ResponsesBridgeError, Result};

pub fn response_input_item_to_chat_message(
    item: ResponseInputItem,
) -> Result<models::chat::ChatMessage> {
    Ok(match item {
        ResponseInputItem::Message { role, content, .. } => models::chat::ChatMessage {
            role,
            content: Some(response_input_content_to_chat_content(content)),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        },
        ResponseInputItem::FunctionCall {
            call_id,
            name,
            arguments,
            ..
        } => models::chat::ChatMessage {
            role: "assistant".to_owned(),
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: call_id,
                r#type: "function".to_owned(),
                function: FunctionCall { name, arguments },
            }]),
            tool_call_id: None,
            name: None,
        },
        ResponseInputItem::FunctionCallOutput {
            call_id, output, ..
        } => models::chat::ChatMessage {
            role: "tool".to_owned(),
            content: Some(ChatMessageContent::Text(output)),
            tool_calls: None,
            tool_call_id: Some(call_id),
            name: None,
        },
        ResponseInputItem::Output { role, content, .. } => models::chat::ChatMessage {
            role: role.unwrap_or_else(|| "assistant".to_owned()),
            content: Some(response_input_content_to_chat_content(
                ResponseInputContent::Array(
                    content
                        .into_iter()
                        .filter_map(response_output_content_to_input_part)
                        .collect(),
                ),
            )),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        },
    })
}

pub fn response_input_content_to_chat_content(content: ResponseInputContent) -> ChatMessageContent {
    match content {
        ResponseInputContent::Text(text) => ChatMessageContent::Text(text),
        ResponseInputContent::Array(parts) => ChatMessageContent::Blocks(
            parts
                .into_iter()
                .filter_map(response_content_part_to_chat_value)
                .collect(),
        ),
    }
}

pub fn response_content_part_to_chat_value(part: ResponseContentPart) -> Option<Value> {
    match part {
        ResponseContentPart::Text { text } => Some(json!({ "type": "text", "text": text })),
        ResponseContentPart::Image { image_url, detail } => {
            let mut image = json!({ "url": image_url });
            if let Some(detail) = detail {
                image["detail"] = Value::String(detail);
            }
            Some(json!({ "type": "image_url", "image_url": image }))
        }
        ResponseContentPart::File { .. } => None,
    }
}

pub fn response_output_content_to_input_part(
    content: ResponseOutputContent,
) -> Option<ResponseContentPart> {
    match content {
        ResponseOutputContent::Text { text } => Some(ResponseContentPart::Text { text }),
        ResponseOutputContent::Refusal { refusal } => {
            Some(ResponseContentPart::Text { text: refusal })
        }
        ResponseOutputContent::Thinking { thinking, .. } => {
            Some(ResponseContentPart::Text { text: thinking })
        }
        ResponseOutputContent::ToolCall { .. } => None,
    }
}

#[allow(dead_code)]
pub fn unsupported_input_part(part_type: impl Into<String>) -> ResponsesBridgeError {
    ResponsesBridgeError::UnsupportedInputPart {
        part_type: part_type.into(),
    }
}
