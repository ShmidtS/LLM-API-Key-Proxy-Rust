use std::collections::HashMap;

use models::chat::{ChatCompletionRequest, ChatMessage, ChatMessageContent, ToolChoice};
use models::responses::{
    CreateResponseRequest, ResponseContentPart, ResponseInput, ResponseInputContent,
    ResponseInputItem, ResponseNamedToolChoice, ResponseTextConfig, ResponseTool,
    ResponseToolChoice,
};
use serde_json::Value;

use super::error::Result;
use super::input::response_input_item_to_chat_message;
use super::tools::{
    response_text_config_to_chat_response_format, response_tool_choice_to_chat_tool_choice,
    response_tool_to_chat_tool,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResponsesEndpoint {
    NativeResponses,
    ChatCompletionsEmulation,
}

#[derive(Debug, Clone)]
pub struct TranslatedResponsesRequest {
    pub endpoint: ResponsesEndpoint,
    pub upstream_path: String,
    pub chat_request: ChatCompletionRequest,
    pub context: ResponsesRequestContext,
}

#[derive(Debug, Clone)]
pub struct NativeResponsesRequest {
    pub endpoint: ResponsesEndpoint,
    pub upstream_path: String,
    pub body: Value,
}

#[derive(Debug, Clone)]
pub struct ResponsesRequestContext {
    pub original_model: String,
    pub instructions: Option<String>,
    pub max_output_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub tools: Option<Vec<ResponseTool>>,
    pub tool_choice: Option<ResponseToolChoice>,
    pub text: Option<ResponseTextConfig>,
    pub truncation: Option<String>,
    pub previous_response_id: Option<String>,
    pub metadata: Option<Value>,
    pub stream: bool,
}

pub fn chat_request_to_responses_request(
    chat_req: &ChatCompletionRequest,
) -> std::result::Result<CreateResponseRequest, String> {
    let mut input = Vec::new();
    let mut instructions = Vec::new();

    for message in &chat_req.messages {
        let content = message
            .content
            .as_ref()
            .map(chat_message_content_to_response_input_content)
            .transpose()?;

        match message.role.as_str() {
            "system" => {
                let text = response_input_content_to_text(&content);
                if !text.is_empty() {
                    instructions.push(text);
                }
            }
            "user" => input.push(ResponseInputItem::Message {
                type_: "message".to_owned(),
                role: "user".to_owned(),
                content: content.unwrap_or_else(|| ResponseInputContent::Text(String::new())),
            }),
            "assistant" => {
                let tool_calls = message.tool_calls.as_deref().unwrap_or_default();
                if tool_calls.is_empty() {
                    input.push(ResponseInputItem::Message {
                        type_: "message".to_owned(),
                        role: "assistant".to_owned(),
                        content: content
                            .unwrap_or_else(|| ResponseInputContent::Text(String::new())),
                    });
                    continue;
                }
                let text = response_input_content_to_text(&content);
                if !text.is_empty() {
                    input.push(ResponseInputItem::Message {
                        type_: "message".to_owned(),
                        role: "assistant".to_owned(),
                        content: ResponseInputContent::Text(text),
                    });
                }
                for tool_call in tool_calls {
                    input.push(ResponseInputItem::FunctionCall {
                        id: tool_call.id.clone(),
                        call_id: tool_call.id.clone(),
                        name: tool_call.function.name.clone(),
                        arguments: tool_call.function.arguments.clone(),
                        type_: "function_call".to_owned(),
                    });
                }
            }
            "tool" => {
                let text = response_input_content_to_text(&content);
                input.push(ResponseInputItem::FunctionCallOutput {
                    call_id: message.tool_call_id.clone().unwrap_or_default(),
                    output: text,
                    type_: "function_call_output".to_owned(),
                });
            }
            _ => {}
        }
    }

    let bare_model = chat_req
        .model
        .strip_prefix("openai/")
        .unwrap_or(&chat_req.model);
    let is_reasoning = bare_model.starts_with("gpt-5") || bare_model.starts_with("o4");

    Ok(CreateResponseRequest {
        model: chat_req.model.clone(),
        input: ResponseInput::Items(input),
        instructions: (!instructions.is_empty()).then(|| instructions.join("\n")),
        max_output_tokens: chat_req.max_tokens,
        temperature: if is_reasoning {
            None
        } else {
            chat_req.temperature
        },
        top_p: if is_reasoning { None } else { chat_req.top_p },
        tools: chat_req.tools.as_ref().map(|tools| {
            tools
                .iter()
                .map(|tool| ResponseTool {
                    type_: "function".to_owned(),
                    name: Some(tool.function.name.clone()),
                    description: tool.function.description.clone(),
                    parameters: Some(tool.function.parameters.clone()),
                    strict: None,
                    web_search: None,
                    function: None,
                })
                .collect()
        }),
        tool_choice: chat_req
            .tool_choice
            .as_ref()
            .and_then(chat_tool_choice_to_response_tool_choice),
        stream: chat_req.stream,
        text: None,
        truncation: None,
        previous_response_id: None,
        metadata: None,
    })
}

fn chat_message_content_to_response_input_content(
    content: &ChatMessageContent,
) -> std::result::Result<ResponseInputContent, String> {
    match content {
        ChatMessageContent::Text(text) => Ok(ResponseInputContent::Text(text.clone())),
        ChatMessageContent::Blocks(blocks) => {
            let parts: std::result::Result<Vec<_>, _> = blocks
                .iter()
                .map(|block| {
                    let block_type = block.get("type").and_then(Value::as_str);
                    match block_type {
                        Some("text") => block
                            .get("text")
                            .and_then(Value::as_str)
                            .map(|text| ResponseContentPart::Text {
                                text: text.to_owned(),
                            })
                            .ok_or_else(|| "text block missing text field".to_owned()),
                        Some("image_url") => {
                            let url = block
                                .get("image_url")
                                .and_then(|v| v.get("url"))
                                .and_then(Value::as_str)
                                .map(|s| s.to_owned());
                            let detail = block
                                .get("image_url")
                                .and_then(|v| v.get("detail"))
                                .and_then(Value::as_str)
                                .map(|s| s.to_owned());
                            match url {
                                Some(image_url) => {
                                    Ok(ResponseContentPart::Image { image_url, detail })
                                }
                                None => Err("image_url block missing url".to_owned()),
                            }
                        }
                        _ => Err(format!("unsupported content block type: {:?}", block_type)),
                    }
                })
                .collect();
            Ok(ResponseInputContent::Array(parts?))
        }
    }
}

fn response_input_content_to_text(content: &Option<ResponseInputContent>) -> String {
    match content {
        Some(ResponseInputContent::Text(text)) => text.clone(),
        Some(ResponseInputContent::Array(parts)) => parts
            .iter()
            .filter_map(|p| match p {
                ResponseContentPart::Text { text } => Some(text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        None => String::new(),
    }
}

fn chat_tool_choice_to_response_tool_choice(
    tool_choice: &ToolChoice,
) -> Option<ResponseToolChoice> {
    match tool_choice {
        ToolChoice::String(value) => match value.as_str() {
            "auto" => Some(ResponseToolChoice::Auto),
            "none" => Some(ResponseToolChoice::None_),
            "required" => Some(ResponseToolChoice::Required),
            _ => None,
        },
        ToolChoice::Object { r#type, function } if r#type == "function" => {
            Some(ResponseToolChoice::Named(ResponseNamedToolChoice {
                type_: "function".to_owned(),
                name: Some(function.name.clone()),
                function: None,
            }))
        }
        ToolChoice::Object { .. } => None,
    }
}

pub fn responses_request_to_native_request(
    req: &CreateResponseRequest,
) -> Result<NativeResponsesRequest> {
    let mut body = serde_json::to_value(req)?;
    if let Some(model) = body.get("model").and_then(Value::as_str)
        && let Some(stripped) = model.strip_prefix("openai/")
    {
        body["model"] = Value::String(stripped.to_owned());
    }
    Ok(NativeResponsesRequest {
        endpoint: ResponsesEndpoint::NativeResponses,
        upstream_path: "responses".to_owned(),
        body,
    })
}

pub fn responses_request_to_chat_request(
    req: CreateResponseRequest,
) -> Result<TranslatedResponsesRequest> {
    let context = ResponsesRequestContext {
        original_model: req.model.clone(),
        instructions: req.instructions.clone(),
        max_output_tokens: req.max_output_tokens,
        temperature: req.temperature,
        top_p: req.top_p,
        tools: req.tools.clone(),
        tool_choice: req.tool_choice.clone(),
        text: req.text.clone(),
        truncation: req.truncation.clone(),
        previous_response_id: req.previous_response_id.clone(),
        metadata: req.metadata.clone(),
        stream: req.stream.unwrap_or(false),
    };

    let messages = response_input_to_messages(req.instructions.clone(), req.input)?;
    let tools = req
        .tools
        .as_ref()
        .map(|tools| {
            tools
                .iter()
                .map(response_tool_to_chat_tool)
                .collect::<Result<Vec<_>>>()
        })
        .transpose()?
        .filter(|tools| !tools.is_empty());
    let tool_choice = req
        .tool_choice
        .as_ref()
        .map(response_tool_choice_to_chat_tool_choice)
        .transpose()?;
    let mut extra = HashMap::new();
    let response_format =
        response_text_config_to_chat_response_format(req.text.as_ref(), &mut extra);

    Ok(TranslatedResponsesRequest {
        endpoint: ResponsesEndpoint::ChatCompletionsEmulation,
        upstream_path: "chat/completions".to_owned(),
        chat_request: ChatCompletionRequest {
            model: req.model,
            messages,
            temperature: req.temperature,
            max_tokens: req.max_output_tokens,
            top_p: req.top_p,
            stream: req.stream,
            stop: None,
            presence_penalty: None,
            frequency_penalty: None,
            tools,
            tool_choice,
            user: None,
            response_format,
            extra,
        },
        context,
    })
}

pub fn response_input_to_messages(
    instructions: Option<String>,
    input: ResponseInput,
) -> Result<Vec<ChatMessage>> {
    let mut messages = Vec::new();
    if let Some(instructions) = instructions {
        messages.push(ChatMessage {
            role: "system".to_owned(),
            content: Some(ChatMessageContent::Text(instructions)),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        });
    }

    match input {
        ResponseInput::Simple(text) => messages.push(ChatMessage {
            role: "user".to_owned(),
            content: Some(ChatMessageContent::Text(text)),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }),
        ResponseInput::Items(items) => {
            for item in items {
                messages.push(response_input_item_to_chat_message(item)?);
            }
        }
    }

    Ok(messages)
}
