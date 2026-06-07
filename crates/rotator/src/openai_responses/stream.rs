use std::collections::{HashMap, HashSet, VecDeque};
use std::convert::Infallible;

use bytes::Bytes;
use futures::{Stream, StreamExt, stream};
use models::responses::{
    Response as ResponsesResponse, ResponseError, ResponseOutputContent, ResponseOutputItem,
    ResponseStreamEvent, ResponseUsage,
};
use serde_json::Value;

use super::id::{Clock, ResponseIdFactory, SystemClock};
use super::request::ResponsesRequestContext;

#[derive(Debug, Clone)]
pub struct ResponsesStreamTranslator<C = SystemClock> {
    clock: C,
    id_factory: ResponseIdFactory,
}

impl Default for ResponsesStreamTranslator<SystemClock> {
    fn default() -> Self {
        Self {
            clock: SystemClock,
            id_factory: ResponseIdFactory::default(),
        }
    }
}

impl<C: Clock + Clone + Send + Sync + 'static> ResponsesStreamTranslator<C> {
    pub fn new(clock: C, id_factory: ResponseIdFactory) -> Self {
        Self { clock, id_factory }
    }

    pub fn translate<S, E>(
        &self,
        chat_stream: S,
        context: ResponsesRequestContext,
    ) -> impl Stream<Item = Result<Bytes, Infallible>>
    where
        S: Stream<Item = Result<Bytes, E>> + Send + 'static,
        E: std::fmt::Display,
    {
        chat_sse_to_responses_sse(chat_stream, context, self.id_factory, self.clock.clone())
    }
}

pub fn chat_sse_to_responses_sse<S, E, C>(
    chat_stream: S,
    context: ResponsesRequestContext,
    ids: ResponseIdFactory,
    clock: C,
) -> impl Stream<Item = Result<Bytes, Infallible>>
where
    S: Stream<Item = Result<Bytes, E>> + Send + 'static,
    E: std::fmt::Display,
    C: Clock + Send + Sync + 'static,
{
    let initial_state = StreamState::new(context, ids, clock.unix_seconds());
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
                                error: ResponseError {
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

#[allow(dead_code)]
pub struct ChatStreamFrame {
    data: String,
}

#[allow(dead_code)]
pub fn parse_chat_sse_frame(bytes: &[u8]) -> Vec<ChatStreamFrame> {
    let text = std::str::from_utf8(bytes).unwrap_or_default();
    text.split("\n\n")
        .flat_map(|frame| {
            frame
                .lines()
                .filter_map(|line| line.strip_prefix("data:").map(str::trim))
                .map(|data| ChatStreamFrame {
                    data: data.to_owned(),
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

struct StreamState {
    context: ResponsesRequestContext,
    ids: ResponseIdFactory,
    buffer: String,
    pending: VecDeque<String>,
    response_id: String,
    created_at: u64,
    content_added: bool,
    content_part_added: bool,
    content_text: String,
    tool_calls: HashMap<usize, StreamToolCall>,
    added_tools: HashSet<usize>,
    done_tools: HashSet<usize>,
    usage: Option<ResponseUsage>,
    completed: bool,
    done: bool,
}

impl StreamState {
    fn new(context: ResponsesRequestContext, ids: ResponseIdFactory, created_at: u64) -> Self {
        let response_id = format!("resp_{created_at}");
        let mut state = Self {
            context,
            ids,
            buffer: String::new(),
            pending: VecDeque::new(),
            response_id,
            created_at,
            content_added: false,
            content_part_added: false,
            content_text: String::new(),
            tool_calls: HashMap::new(),
            added_tools: HashSet::new(),
            done_tools: HashSet::new(),
            usage: None,
            completed: false,
            done: false,
        };
        emit_response_created(&mut state);
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
            self.response_id = self.ids.response_id_from_chat_id(id);
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
                emit_text_delta(self, content);
            }
            if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
                for tool_call in tool_calls {
                    emit_tool_delta(self, ToolDelta(tool_call.clone()));
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
        if !self.content_added {
            self.content_added = true;
            self.push_event(
                "response.output_item.added",
                ResponseStreamEvent::OutputItemAdded {
                    output_index: 0,
                    item: ResponseOutputItem::Message {
                        id: self.ids.message_id(&self.response_id),
                        type_: "message".to_owned(),
                        role: "assistant".to_owned(),
                        content: vec![],
                        status: Some("in_progress".to_owned()),
                    },
                },
            );
        }
        if !self.content_part_added {
            self.content_part_added = true;
            self.push_event(
                "response.content_part.added",
                ResponseStreamEvent::ContentPartAdded {
                    output_index: 0,
                    content_index: 0,
                    part: ResponseOutputContent::Text {
                        text: String::new(),
                    },
                },
            );
        }
    }

    fn finish_items(&mut self) {
        if self.content_added {
            let part = ResponseOutputContent::Text {
                text: self.content_text.clone(),
            };
            self.push_event(
                "response.content_part.done",
                ResponseStreamEvent::ContentPartDone {
                    output_index: 0,
                    content_index: 0,
                    part: part.clone(),
                },
            );
            self.push_event(
                "response.output_item.done",
                ResponseStreamEvent::OutputItemDone {
                    output_index: 0,
                    item: ResponseOutputItem::Message {
                        id: self.ids.message_id(&self.response_id),
                        type_: "message".to_owned(),
                        role: "assistant".to_owned(),
                        content: vec![part],
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
                let call_id = tool.call_id(index);
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
                            id: self.ids.function_call_id(&call_id),
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
        emit_completed(self);
    }

    fn skeleton_response(&self, status: &str) -> ResponsesResponse {
        ResponsesResponse {
            id: self.response_id.clone(),
            object: "response".to_owned(),
            created_at: self.created_at,
            status: status.to_owned(),
            error: None,
            incomplete_details: None,
            instructions: self.context.instructions.clone(),
            max_output_tokens: self.context.max_output_tokens,
            model: self.context.original_model.clone(),
            output: vec![],
            parallel_tool_calls: None,
            previous_response_id: self.context.previous_response_id.clone(),
            reasoning: None,
            store: None,
            temperature: self.context.temperature,
            text: self.context.text.clone(),
            tool_choice: self.context.tool_choice.clone(),
            tools: self.context.tools.clone(),
            top_p: self.context.top_p,
            truncation: self.context.truncation.clone(),
            usage: Some(self.usage.clone().unwrap_or_default()),
            user: None,
            metadata: self.context.metadata.clone(),
        }
    }

    fn push_event(&mut self, event: &str, data: ResponseStreamEvent) {
        self.pending.push_back(sse_event(event, &data));
    }
}

struct ToolDelta(Value);

struct StreamToolCall {
    id: String,
    name: String,
    arguments: String,
}

impl StreamToolCall {
    fn call_id(&self, index: usize) -> String {
        if self.id.is_empty() {
            format!("call_{index}")
        } else {
            self.id.clone()
        }
    }
}

fn emit_response_created(state: &mut StreamState) {
    let response = state.skeleton_response("in_progress");
    state.push_event(
        "response.created",
        ResponseStreamEvent::ResponseCreated { response },
    );
}

fn emit_text_delta(state: &mut StreamState, delta: &str) {
    state.ensure_content_item();
    state.content_text.push_str(delta);
    state.push_event(
        "response.output_text.delta",
        ResponseStreamEvent::OutputTextDelta {
            output_index: 0,
            content_index: 0,
            delta: delta.to_owned(),
        },
    );
}

fn emit_tool_delta(state: &mut StreamState, delta: ToolDelta) {
    let value = delta.0;
    let index = value.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
    let output_index = index + 1;
    let tool = state
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
    let call_id = tool.call_id(index);
    let name = tool.name.clone();

    if !state.added_tools.contains(&index) {
        state.added_tools.insert(index);
        state.push_event(
            "response.output_item.added",
            ResponseStreamEvent::OutputItemAdded {
                output_index,
                item: ResponseOutputItem::ToolCall {
                    id: state.ids.function_call_id(&call_id),
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
        if let Some(tool) = state.tool_calls.get_mut(&index) {
            tool.arguments.push_str(arguments);
        }
        state.push_event(
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

fn emit_completed(state: &mut StreamState) {
    let response = state.skeleton_response("completed");
    let usage = state.usage.clone().unwrap_or_default();
    state.push_event(
        "response.completed",
        ResponseStreamEvent::ResponseCompleted {
            response,
            usage: Some(usage),
        },
    );
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

pub fn sse_event(event: &str, payload: &ResponseStreamEvent) -> String {
    let payload = serde_json::to_string(payload).unwrap_or_else(|_| "{}".to_owned());
    format!("event: {event}\ndata: {payload}\n\n")
}
