use models::chat::{ChatCompletionResponse, ChatMessageContent, ToolCall, Usage};
use models::responses::{Response, ResponseOutputContent, ResponseOutputItem, ResponseUsage};
use serde_json::json;

use super::id::{Clock, ResponseIdFactory};
use super::request::ResponsesRequestContext;

pub fn chat_completion_to_response(
    chat: ChatCompletionResponse,
    context: &ResponsesRequestContext,
    ids: &ResponseIdFactory,
    clock: &dyn Clock,
) -> Response {
    let response_id = ids.response_id_from_chat_id(&chat.id);
    let mut output = Vec::new();
    let mut status = "completed".to_owned();
    let mut incomplete_details = None;

    if let Some(choice) = chat.choices.first() {
        if let Some(content) = choice.message.content.clone() {
            output.push(ResponseOutputItem::Message {
                id: ids.message_id(&response_id),
                type_: "message".to_owned(),
                role: "assistant".to_owned(),
                content: chat_content_to_response_content(content),
                status: Some("completed".to_owned()),
            });
        }
        if let Some(tool_calls) = choice.message.tool_calls.clone() {
            output.extend(
                tool_calls
                    .into_iter()
                    .map(|tool_call| tool_call_to_response_item(tool_call, ids)),
            );
        }
        if choice.finish_reason.as_deref() == Some("length") {
            status = "incomplete".to_owned();
            incomplete_details = Some(json!({ "reason": "max_output_tokens" }));
        }
    }

    Response {
        id: response_id,
        object: "response".to_owned(),
        created_at: clock.unix_seconds(),
        status,
        error: None,
        incomplete_details,
        instructions: context.instructions.clone(),
        max_output_tokens: context.max_output_tokens,
        model: context.original_model.clone(),
        output,
        parallel_tool_calls: None,
        previous_response_id: context.previous_response_id.clone(),
        reasoning: None,
        store: None,
        temperature: context.temperature,
        text: context.text.clone(),
        tool_choice: context.tool_choice.clone(),
        tools: context.tools.clone(),
        top_p: context.top_p,
        truncation: context.truncation.clone(),
        usage: Some(usage_to_response_usage(chat.usage)),
        user: None,
        metadata: context.metadata.clone(),
    }
}

pub fn chat_content_to_response_content(content: ChatMessageContent) -> Vec<ResponseOutputContent> {
    match content {
        ChatMessageContent::Text(text) => vec![ResponseOutputContent::Text { text }],
        ChatMessageContent::Blocks(parts) => parts
            .into_iter()
            .filter_map(|part| {
                let part_type = part.get("type").and_then(serde_json::Value::as_str);
                match part_type {
                    Some("text") | Some("output_text") => part
                        .get("text")
                        .and_then(serde_json::Value::as_str)
                        .map(|text| ResponseOutputContent::Text {
                            text: text.to_owned(),
                        }),
                    Some("refusal") => part.get("refusal").and_then(serde_json::Value::as_str).map(
                        |refusal| ResponseOutputContent::Refusal {
                            refusal: refusal.to_owned(),
                        },
                    ),
                    _ => None,
                }
            })
            .collect(),
    }
}

pub fn tool_call_to_response_item(
    tool_call: ToolCall,
    ids: &ResponseIdFactory,
) -> ResponseOutputItem {
    ResponseOutputItem::ToolCall {
        id: ids.function_call_id(&tool_call.id),
        type_: "function_call".to_owned(),
        call_id: tool_call.id,
        name: tool_call.function.name,
        arguments: tool_call.function.arguments,
        status: Some("completed".to_owned()),
    }
}

pub fn usage_to_response_usage(usage: Usage) -> ResponseUsage {
    ResponseUsage {
        input_tokens: usage.prompt_tokens,
        output_tokens: usage.completion_tokens,
        total_tokens: usage.total_tokens,
        input_tokens_details: None,
        output_tokens_details: None,
    }
}
