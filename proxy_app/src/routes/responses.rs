use crate::errors::{AppError, invalid_request_error};
use crate::routes::utils::{
    normalize_model_in_body, resolve_provider_for_model, upstream_response,
};
use crate::state::AppState;
use axum::body::{Body, Bytes};
use axum::extract::{OriginalUri, Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router, routing::get, routing::post};
use futures::{Stream, StreamExt, stream};
use models::chat::{
    ChatCompletionRequest, ChatCompletionResponse, ChatMessage, ChatMessageContent, FunctionCall,
    FunctionDefinition, ResponseFormat, ToolCall, ToolChoice, ToolDefinition, Usage,
};
use models::responses::{
    CreateResponseRequest, Response as ResponsesResponse, ResponseContentPart, ResponseInput,
    ResponseInputContent, ResponseInputItem, ResponseOutputContent, ResponseOutputItem,
    ResponseStreamEvent, ResponseTextFormat, ResponseTool, ResponseToolChoice, ResponseUsage,
};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet, VecDeque};
use std::convert::Infallible;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/responses", post(create_response))
        .route("/responses/{response_id}", get(get_response))
        .route("/v1/responses", post(create_response))
        .route("/v1/responses/{response_id}", get(get_response))
}

async fn get_response(
    State(state): State<AppState>,
    Path(response_id): Path<String>,
    OriginalUri(_uri): OriginalUri,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    let query_vec = params.into_iter().collect::<Vec<_>>();
    let path = format!("responses/{response_id}");
    let upstream = state
        .rotator
        .get_with_query("openai", &path, &query_vec)
        .await?;
    upstream_response(upstream).await
}

async fn create_response(
    State(state): State<AppState>,
    Json(original_request): Json<CreateResponseRequest>,
) -> Result<Response, AppError> {
    if !state.registry.is_model_allowed(&original_request.model) {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Model not allowed"})),
        )
            .into_response());
    }

    if let Some(tools) = &original_request.tools {
        for tool in tools {
            if tool.type_ != "function" {
                return Ok(
                    invalid_request_error(format!("Unsupported tool type: {}", tool.type_))
                        .into_response(),
                );
            }
        }
    }

    let chat_request = responses_request_to_chat_request(original_request.clone())?;
    let provider = resolve_provider_for_model(&state, &chat_request.model);
    let mut upstream_body = serde_json::to_value(&chat_request)?;
    normalize_model_in_body(&mut upstream_body, &provider);
    tracing::info!(
        method = "POST",
        provider = %provider,
        model = %chat_request.model,
        upstream_path = "chat/completions",
        "forwarding responses request"
    );
    let upstream = state
        .rotator
        .request(&provider, "chat/completions", upstream_body)
        .await?;
    tracing::info!(
        provider = %provider,
        status = %upstream.status(),
        "upstream responses response"
    );

    if original_request.stream == Some(true) {
        let status = upstream.status();
        let stream = chat_sse_to_responses_sse(upstream.bytes_stream(), original_request);
        return Response::builder()
            .status(status)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .header(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"))
            .body(Body::from_stream(stream))
            .map_err(|e| AppError::Internal(e.to_string()));
    }

    let status = upstream.status();
    if !status.is_success() {
        return upstream_response(upstream).await;
    }
    let chat: ChatCompletionResponse = upstream
        .json()
        .await
        .map_err(|e| rotator::RotatorError::Http(e.to_string()))?;
    let response = chat_completion_to_response(chat, &original_request);
    Ok((
        StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::OK),
        Json(response),
    )
        .into_response())
}

fn responses_request_to_chat_request(
    req: CreateResponseRequest,
) -> Result<ChatCompletionRequest, AppError> {
    let mut messages = Vec::new();
    if let Some(instructions) = req.instructions.clone() {
        messages.push(ChatMessage {
            role: "system".to_owned(),
            content: Some(ChatMessageContent::Text(instructions)),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        });
    }

    match req.input {
        ResponseInput::Simple(text) => messages.push(ChatMessage {
            role: "user".to_owned(),
            content: Some(ChatMessageContent::Text(text)),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }),
        ResponseInput::Items(items) => {
            messages.extend(items.into_iter().map(response_input_item_to_chat_message));
        }
    }

    let tools = req
        .tools
        .as_ref()
        .map(|tools| {
            tools
                .iter()
                .map(response_tool_to_chat_tool)
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?;
    let tools = tools.filter(|tools| !tools.is_empty());
    let tool_choice = req
        .tool_choice
        .as_ref()
        .map(response_tool_choice_to_chat_tool_choice);
    let mut extra = HashMap::new();
    let response_format = response_format_from_text(req.text.as_ref(), &mut extra);
    if let Some(metadata) = req.metadata.clone() {
        extra.insert("metadata".to_owned(), metadata);
    }
    if let Some(previous_response_id) = req.previous_response_id.clone() {
        extra.insert(
            "previous_response_id".to_owned(),
            Value::String(previous_response_id),
        );
    }
    if let Some(truncation) = req.truncation.clone() {
        extra.insert("truncation".to_owned(), Value::String(truncation));
    }

    Ok(ChatCompletionRequest {
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
    })
}

fn response_input_item_to_chat_message(item: ResponseInputItem) -> ChatMessage {
    match item {
        ResponseInputItem::Message { role, content } => ChatMessage {
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
        } => ChatMessage {
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
        } => ChatMessage {
            role: "tool".to_owned(),
            content: Some(ChatMessageContent::Text(output)),
            tool_calls: None,
            tool_call_id: Some(call_id),
            name: None,
        },
        ResponseInputItem::Output { role, content, .. } => ChatMessage {
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
    }
}

fn response_input_content_to_chat_content(content: ResponseInputContent) -> ChatMessageContent {
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

fn response_content_part_to_chat_value(part: ResponseContentPart) -> Option<Value> {
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

fn response_output_content_to_input_part(
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

fn response_tool_to_chat_tool(tool: &ResponseTool) -> Result<ToolDefinition, AppError> {
    if tool.type_ != "function" {
        return Err(AppError::BadRequest(format!(
            "Unsupported tool type: {}",
            tool.type_
        )));
    }
    let function = tool.function.clone().ok_or_else(|| {
        AppError::BadRequest("Function tool missing function definition".to_owned())
    })?;
    Ok(ToolDefinition {
        r#type: "function".to_owned(),
        function,
    })
}

fn response_tool_choice_to_chat_tool_choice(choice: &ResponseToolChoice) -> ToolChoice {
    match choice {
        ResponseToolChoice::Auto => ToolChoice::String("auto".to_owned()),
        ResponseToolChoice::Required => ToolChoice::String("required".to_owned()),
        ResponseToolChoice::None_ => ToolChoice::String("none".to_owned()),
        ResponseToolChoice::Named(choice) => ToolChoice::Object {
            r#type: "function".to_owned(),
            function: FunctionDefinition {
                name: choice.function.name.clone(),
                description: None,
                parameters: json!({}),
            },
        },
    }
}

fn response_format_from_text(
    text: Option<&models::responses::ResponseTextConfig>,
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

fn chat_completion_to_response(
    chat: ChatCompletionResponse,
    original_request: &CreateResponseRequest,
) -> ResponsesResponse {
    let mut output = Vec::new();
    if let Some(choice) = chat.choices.first() {
        if let Some(content) = choice.message.content.clone() {
            output.push(ResponseOutputItem::Message {
                id: format!("msg_{}", chat.id),
                type_: "message".to_owned(),
                role: "assistant".to_owned(),
                content: chat_content_to_response_content(content),
                status: Some("completed".to_owned()),
            });
        }
        if let Some(tool_calls) = choice.message.tool_calls.clone() {
            output.extend(tool_calls.into_iter().map(tool_call_to_response_item));
        }
    }

    ResponsesResponse {
        id: response_id_from_chat_id(&chat.id),
        object: "response".to_owned(),
        created_at: unix_time(),
        status: "completed".to_owned(),
        error: None,
        incomplete_details: None,
        instructions: original_request.instructions.clone(),
        max_output_tokens: original_request.max_output_tokens,
        model: original_request.model.clone(),
        output,
        parallel_tool_calls: None,
        previous_response_id: original_request.previous_response_id.clone(),
        reasoning: None,
        store: None,
        temperature: original_request.temperature,
        text: original_request.text.clone(),
        tool_choice: original_request.tool_choice.clone(),
        tools: original_request.tools.clone(),
        top_p: original_request.top_p,
        truncation: original_request.truncation.clone(),
        usage: Some(usage_to_response_usage(chat.usage)),
        user: None,
        metadata: original_request.metadata.clone(),
    }
}

fn chat_content_to_response_content(content: ChatMessageContent) -> Vec<ResponseOutputContent> {
    match content {
        ChatMessageContent::Text(text) => vec![ResponseOutputContent::Text { text }],
        ChatMessageContent::Blocks(parts) => parts
            .into_iter()
            .filter_map(|part| {
                let part_type = part.get("type").and_then(Value::as_str);
                match part_type {
                    Some("text") | Some("output_text") => part
                        .get("text")
                        .and_then(Value::as_str)
                        .map(|text| ResponseOutputContent::Text {
                            text: text.to_owned(),
                        }),
                    Some("refusal") => part.get("refusal").and_then(Value::as_str).map(|refusal| {
                        ResponseOutputContent::Refusal {
                            refusal: refusal.to_owned(),
                        }
                    }),
                    _ => None,
                }
            })
            .collect(),
    }
}

fn tool_call_to_response_item(tool_call: ToolCall) -> ResponseOutputItem {
    ResponseOutputItem::ToolCall {
        id: format!("fc_{}", tool_call.id),
        type_: "function_call".to_owned(),
        call_id: tool_call.id,
        name: tool_call.function.name,
        arguments: tool_call.function.arguments,
        status: Some("completed".to_owned()),
    }
}

fn usage_to_response_usage(usage: Usage) -> ResponseUsage {
    ResponseUsage {
        input_tokens: usage.prompt_tokens,
        output_tokens: usage.completion_tokens,
        total_tokens: usage.total_tokens,
        input_tokens_details: None,
        output_tokens_details: None,
    }
}

fn chat_sse_to_responses_sse<S, E>(
    chat_stream: S,
    original_request: CreateResponseRequest,
) -> impl Stream<Item = Result<Bytes, Infallible>>
where
    S: Stream<Item = Result<Bytes, E>> + Send + 'static,
    E: std::fmt::Display,
{
    let initial_state = StreamState::new(original_request);
    stream::unfold(
        (chat_stream.boxed(), initial_state),
        |(mut chat_stream, mut state)| async move {
            loop {
                if let Some(event) = state.pending.pop_front() {
                    return Some((Ok(Bytes::from(event)), (chat_stream, state)));
                }
                if state.done {
                    return None;
                }

                match chat_stream.next().await {
                    Some(Ok(bytes)) => state.feed(&bytes),
                    Some(Err(err)) => {
                        state.pending.push_back(sse_event(
                            "error",
                            &ResponseStreamEvent::Error {
                                error: models::responses::ResponseError {
                                    message: err.to_string(),
                                    code: None,
                                    type_: "api_error".to_owned(),
                                },
                            },
                        ));
                        state.done = true;
                    }
                    None => {
                        state.complete_if_needed();
                        state.done = true;
                    }
                }
            }
        },
    )
}

struct StreamState {
    original_request: CreateResponseRequest,
    buffer: String,
    pending: VecDeque<String>,
    response_id: String,
    created_at: u64,
    content_added: bool,
    content_text: String,
    tool_calls: HashMap<usize, StreamToolCall>,
    added_tools: HashSet<usize>,
    done_tools: HashSet<usize>,
    usage: Option<ResponseUsage>,
    completed: bool,
    done: bool,
}

impl StreamState {
    fn new(original_request: CreateResponseRequest) -> Self {
        let response_id = format!("resp_{}", unix_time());
        let created_at = unix_time();
        let mut state = Self {
            original_request,
            buffer: String::new(),
            pending: VecDeque::new(),
            response_id,
            created_at,
            content_added: false,
            content_text: String::new(),
            tool_calls: HashMap::new(),
            added_tools: HashSet::new(),
            done_tools: HashSet::new(),
            usage: None,
            completed: false,
            done: false,
        };
        let response = state.skeleton_response("in_progress");
        state.push_event(
            "response.created",
            ResponseStreamEvent::ResponseCreated { response },
        );
        state
    }

    fn feed(&mut self, bytes: &[u8]) {
        self.buffer
            .push_str(std::str::from_utf8(bytes).unwrap_or_default());
        while let Some(pos) = self.buffer.find("\n\n") {
            let frame = self.buffer[..pos].to_owned();
            self.buffer = self.buffer[pos + 2..].to_owned();
            self.process_frame(&frame);
        }
    }

    fn process_frame(&mut self, frame: &str) {
        for data in frame
            .lines()
            .filter_map(|line| line.strip_prefix("data:").map(str::trim))
        {
            if data == "[DONE]" {
                self.complete_if_needed();
                self.done = true;
                continue;
            }
            let Ok(chunk) = serde_json::from_str::<Value>(data) else {
                continue;
            };
            self.process_chunk(&chunk);
        }
    }

    fn process_chunk(&mut self, chunk: &Value) {
        if let Some(id) = chunk.get("id").and_then(Value::as_str) {
            self.response_id = response_id_from_chat_id(id);
        }
        if let Some(usage) = chunk.get("usage") {
            self.usage = usage_from_value(usage);
        }

        for choice in chunk
            .get("choices")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let delta = choice.get("delta").unwrap_or(&Value::Null);
            if let Some(content) = delta.get("content").and_then(Value::as_str) {
                self.ensure_content_item();
                self.content_text.push_str(content);
                self.push_event(
                    "response.output_text.delta",
                    ResponseStreamEvent::OutputTextDelta {
                        output_index: 0,
                        content_index: 0,
                        delta: content.to_owned(),
                    },
                );
            }
            if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
                for tool_call in tool_calls {
                    self.process_tool_call_delta(tool_call);
                }
            }
            if choice
                .get("finish_reason")
                .and_then(Value::as_str)
                .is_some()
            {
                self.finish_items();
            }
        }
    }

    fn ensure_content_item(&mut self) {
        if self.content_added {
            return;
        }
        self.content_added = true;
        self.push_event(
            "response.output_item.added",
            ResponseStreamEvent::OutputItemAdded {
                output_index: 0,
                item: ResponseOutputItem::Message {
                    id: format!("msg_{}", self.response_id),
                    type_: "message".to_owned(),
                    role: "assistant".to_owned(),
                    content: vec![],
                    status: Some("in_progress".to_owned()),
                },
            },
        );
    }

    fn process_tool_call_delta(&mut self, value: &Value) {
        let index = value.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
        let output_index = index + 1;
        let tool = self
            .tool_calls
            .entry(index)
            .or_insert_with(|| StreamToolCall {
                id: String::new(),
                name: String::new(),
                arguments: String::new(),
            });
        if let Some(id) = value.get("id").and_then(Value::as_str) {
            tool.id = id.to_owned();
        }
        if let Some(name) = value.pointer("/function/name").and_then(Value::as_str) {
            tool.name = name.to_owned();
        }
        let call_id = if tool.id.is_empty() {
            format!("call_{index}")
        } else {
            tool.id.clone()
        };
        let name = tool.name.clone();

        if !self.added_tools.contains(&index) {
            self.added_tools.insert(index);
            self.push_event(
                "response.output_item.added",
                ResponseStreamEvent::OutputItemAdded {
                    output_index,
                    item: ResponseOutputItem::ToolCall {
                        id: format!("fc_{call_id}"),
                        type_: "function_call".to_owned(),
                        call_id: call_id.clone(),
                        name: name.clone(),
                        arguments: String::new(),
                        status: Some("in_progress".to_owned()),
                    },
                },
            );
        }

        if let Some(arguments) = value.pointer("/function/arguments").and_then(Value::as_str) {
            if let Some(tool) = self.tool_calls.get_mut(&index) {
                tool.arguments.push_str(arguments);
            }
            self.push_event(
                "response.tool_call_arguments.delta",
                ResponseStreamEvent::ToolCallArgumentsDelta {
                    output_index,
                    content_index: 0,
                    call_id,
                    delta: arguments.to_owned(),
                },
            );
        }
    }

    fn finish_items(&mut self) {
        if self.content_added {
            self.push_event(
                "response.output_item.done",
                ResponseStreamEvent::OutputItemDone {
                    output_index: 0,
                    item: ResponseOutputItem::Message {
                        id: format!("msg_{}", self.response_id),
                        type_: "message".to_owned(),
                        role: "assistant".to_owned(),
                        content: vec![ResponseOutputContent::Text {
                            text: self.content_text.clone(),
                        }],
                        status: Some("completed".to_owned()),
                    },
                },
            );
            self.content_added = false;
        }

        let mut indexes = self.tool_calls.keys().copied().collect::<Vec<_>>();
        indexes.sort_unstable();
        for index in indexes {
            if self.done_tools.contains(&index) {
                continue;
            }
            self.done_tools.insert(index);
            if let Some(tool) = self.tool_calls.get(&index) {
                let call_id = if tool.id.is_empty() {
                    format!("call_{index}")
                } else {
                    tool.id.clone()
                };
                let name = tool.name.clone();
                let arguments = tool.arguments.clone();
                self.push_event(
                    "response.tool_call.done",
                    ResponseStreamEvent::ToolCallDone {
                        output_index: index + 1,
                        content_index: 0,
                        call_id: call_id.clone(),
                    },
                );
                self.push_event(
                    "response.output_item.done",
                    ResponseStreamEvent::OutputItemDone {
                        output_index: index + 1,
                        item: ResponseOutputItem::ToolCall {
                            id: format!("fc_{call_id}"),
                            type_: "function_call".to_owned(),
                            call_id,
                            name,
                            arguments,
                            status: Some("completed".to_owned()),
                        },
                    },
                );
            }
        }
    }

    fn complete_if_needed(&mut self) {
        if self.completed {
            return;
        }
        self.finish_items();
        self.completed = true;
        let response = self.skeleton_response("completed");
        self.push_event(
            "response.completed",
            ResponseStreamEvent::ResponseCompleted {
                response,
                usage: self.usage.clone(),
            },
        );
    }

    fn skeleton_response(&self, status: &str) -> ResponsesResponse {
        ResponsesResponse {
            id: self.response_id.clone(),
            object: "response".to_owned(),
            created_at: self.created_at,
            status: status.to_owned(),
            error: None,
            incomplete_details: None,
            instructions: self.original_request.instructions.clone(),
            max_output_tokens: self.original_request.max_output_tokens,
            model: self.original_request.model.clone(),
            output: vec![],
            parallel_tool_calls: None,
            previous_response_id: self.original_request.previous_response_id.clone(),
            reasoning: None,
            store: None,
            temperature: self.original_request.temperature,
            text: self.original_request.text.clone(),
            tool_choice: self.original_request.tool_choice.clone(),
            tools: self.original_request.tools.clone(),
            top_p: self.original_request.top_p,
            truncation: self.original_request.truncation.clone(),
            usage: self.usage.clone(),
            user: None,
            metadata: self.original_request.metadata.clone(),
        }
    }

    fn push_event(&mut self, event: &str, data: ResponseStreamEvent) {
        self.pending.push_back(sse_event(event, &data));
    }
}

struct StreamToolCall {
    id: String,
    name: String,
    arguments: String,
}

fn usage_from_value(value: &Value) -> Option<ResponseUsage> {
    let input_tokens = value.get("prompt_tokens")?.as_u64()? as u32;
    let output_tokens = value.get("completion_tokens")?.as_u64()? as u32;
    let total_tokens = value.get("total_tokens")?.as_u64()? as u32;
    Some(ResponseUsage {
        input_tokens,
        output_tokens,
        total_tokens,
        input_tokens_details: None,
        output_tokens_details: None,
    })
}

fn sse_event(event: &str, data: &ResponseStreamEvent) -> String {
    let payload = serde_json::to_string(data).unwrap_or_else(|_| "{}".to_owned());
    format!("event: {event}\ndata: {payload}\n\n")
}

fn response_id_from_chat_id(id: &str) -> String {
    if id.starts_with("resp_") {
        id.to_owned()
    } else {
        format!("resp_{id}")
    }
}

fn unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}
