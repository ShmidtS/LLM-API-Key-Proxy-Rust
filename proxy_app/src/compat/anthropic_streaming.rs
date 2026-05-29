use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseRecord {
    pub event: Option<String>,
    pub data: String,
}

#[derive(Debug, Default)]
pub struct ChunkBatcher {
    buffer: Vec<u8>,
}

impl ChunkBatcher {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, chunk: impl AsRef<[u8]>) -> Vec<SseRecord> {
        self.buffer.extend_from_slice(chunk.as_ref());
        let mut records = Vec::new();

        while let Some((record_end, delimiter_len)) = find_record_delimiter(&self.buffer) {
            let record = self.buffer[..record_end].to_vec();
            self.buffer.drain(..record_end + delimiter_len);

            if let Some(record) = parse_sse_record(&record) {
                records.push(record);
            }
        }

        records
    }

    pub fn buffered_len(&self) -> usize {
        self.buffer.len()
    }
}

fn find_record_delimiter(buffer: &[u8]) -> Option<(usize, usize)> {
    for (index, window) in buffer.windows(2).enumerate() {
        if window == b"\n\n" {
            return Some((index, 2));
        }
    }

    for (index, window) in buffer.windows(4).enumerate() {
        if window == b"\r\n\r\n" {
            return Some((index, 4));
        }
    }

    None
}

fn parse_sse_record(bytes: &[u8]) -> Option<SseRecord> {
    let text = String::from_utf8_lossy(bytes);
    let mut event = None;
    let mut data = Vec::new();

    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() || line.starts_with(':') {
            continue;
        }

        if let Some(value) = line.strip_prefix("event:") {
            event = Some(value.trim_start().to_owned());
        } else if let Some(value) = line.strip_prefix("data:") {
            data.push(value.trim_start().to_owned());
        }
    }

    if data.is_empty() {
        None
    } else {
        Some(SseRecord {
            event,
            data: data.join("\n"),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(tag = "type")]
pub enum AnthropicSseEvent {
    #[serde(rename = "message_start")]
    MessageStart { message: AnthropicMessage },
    #[serde(rename = "content_block_start")]
    ContentBlockStart {
        index: u64,
        content_block: AnthropicContentBlock,
    },
    #[serde(rename = "content_block_delta")]
    ContentBlockDelta {
        index: Option<u64>,
        delta: AnthropicContentDelta,
    },
    #[serde(rename = "content_block_stop")]
    ContentBlockStop { index: u64 },
    #[serde(rename = "message_delta")]
    MessageDelta {
        delta: AnthropicMessageDelta,
        usage: Option<AnthropicUsage>,
    },
    #[serde(rename = "message_stop")]
    MessageStop,
    #[serde(rename = "ping")]
    Ping,
    #[serde(rename = "error")]
    Error { error: AnthropicStreamError },
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct AnthropicMessage {
    pub id: Option<String>,
    pub model: Option<String>,
    pub role: Option<String>,
    pub usage: Option<AnthropicUsage>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct AnthropicContentBlock {
    #[serde(rename = "type")]
    pub block_type: String,
    pub text: Option<String>,
    pub id: Option<String>,
    pub name: Option<String>,
    pub input: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(tag = "type")]
pub enum AnthropicContentDelta {
    #[serde(rename = "text_delta")]
    TextDelta { text: String },
    #[serde(rename = "input_json_delta")]
    InputJsonDelta { partial_json: String },
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct AnthropicMessageDelta {
    pub stop_reason: Option<String>,
    pub stop_sequence: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct AnthropicUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct AnthropicStreamError {
    #[serde(rename = "type")]
    pub error_type: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AnthropicStreamItem {
    Event(AnthropicSseEvent),
    Done,
}

impl SseRecord {
    pub fn parse_anthropic(&self) -> Option<AnthropicStreamItem> {
        if self.data == "[DONE]" {
            return Some(AnthropicStreamItem::Done);
        }

        serde_json::from_str::<AnthropicSseEvent>(&self.data)
            .ok()
            .map(AnthropicStreamItem::Event)
    }
}

pub fn anthropic_message_start_event(
    id: Option<&str>,
    model: Option<&str>,
    usage: Option<AnthropicUsage>,
) -> AnthropicSseEvent {
    AnthropicSseEvent::MessageStart {
        message: AnthropicMessage {
            id: id.map(str::to_owned),
            model: model.map(str::to_owned),
            role: Some("assistant".to_owned()),
            usage,
        },
    }
}

pub fn anthropic_content_block_start_event(index: u64, text: Option<&str>) -> AnthropicSseEvent {
    AnthropicSseEvent::ContentBlockStart {
        index,
        content_block: AnthropicContentBlock {
            block_type: "text".to_owned(),
            text: text.map(str::to_owned),
            id: None,
            name: None,
            input: None,
        },
    }
}

pub fn anthropic_text_delta_event(index: Option<u64>, text: &str) -> AnthropicSseEvent {
    AnthropicSseEvent::ContentBlockDelta {
        index,
        delta: AnthropicContentDelta::TextDelta {
            text: text.to_owned(),
        },
    }
}

pub fn anthropic_content_block_stop_event(index: u64) -> AnthropicSseEvent {
    AnthropicSseEvent::ContentBlockStop { index }
}

pub fn anthropic_message_delta_event(stop_reason: Option<&str>) -> AnthropicSseEvent {
    AnthropicSseEvent::MessageDelta {
        delta: AnthropicMessageDelta {
            stop_reason: stop_reason.map(str::to_owned),
            stop_sequence: None,
        },
        usage: None,
    }
}

pub fn anthropic_message_stop_event() -> AnthropicSseEvent {
    AnthropicSseEvent::MessageStop
}

pub fn anthropic_ping_event() -> AnthropicSseEvent {
    AnthropicSseEvent::Ping
}

pub fn anthropic_error_event(message: &str) -> AnthropicSseEvent {
    AnthropicSseEvent::Error {
        error: AnthropicStreamError {
            error_type: "invalid_request_error".to_owned(),
            message: message.to_owned(),
        },
    }
}

pub fn anthropic_sse_event(event: &AnthropicSseEvent) -> String {
    format!(
        "event: {}\ndata: {}\n\n",
        anthropic_event_name(event),
        serde_json::to_string(event).unwrap_or_default()
    )
}

pub fn anthropic_done_event() -> String {
    "data: [DONE]\n\n".to_owned()
}

fn anthropic_event_name(event: &AnthropicSseEvent) -> &'static str {
    match event {
        AnthropicSseEvent::MessageStart { .. } => "message_start",
        AnthropicSseEvent::ContentBlockStart { .. } => "content_block_start",
        AnthropicSseEvent::ContentBlockDelta { .. } => "content_block_delta",
        AnthropicSseEvent::ContentBlockStop { .. } => "content_block_stop",
        AnthropicSseEvent::MessageDelta { .. } => "message_delta",
        AnthropicSseEvent::MessageStop => "message_stop",
        AnthropicSseEvent::Ping => "ping",
        AnthropicSseEvent::Error { .. } => "error",
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenAiStreamChunk {
    pub id: String,
    pub object: String,
    pub created: u64,
    pub model: String,
    pub choices: Vec<OpenAiStreamChoice>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenAiStreamChoice {
    pub index: u64,
    pub delta: Value,
    pub finish_reason: Value,
}

pub fn openai_message_start_chunk(id: Option<&str>, model: &str) -> OpenAiStreamChunk {
    openai_stream_chunk(
        id,
        model,
        serde_json::json!({ "role": "assistant" }),
        Value::Null,
    )
}

pub fn openai_text_delta_chunk(model: &str, text: &str) -> OpenAiStreamChunk {
    openai_stream_chunk(
        None,
        model,
        serde_json::json!({ "content": text }),
        Value::Null,
    )
}

pub fn openai_message_delta_chunk(model: &str, stop_reason: Option<&str>) -> OpenAiStreamChunk {
    openai_stream_chunk(
        None,
        model,
        serde_json::json!({}),
        map_stop_reason(stop_reason),
    )
}

pub fn openai_tool_call_start_chunk(
    model: &str,
    index: u64,
    id: Option<&str>,
    name: Option<&str>,
    arguments: String,
) -> OpenAiStreamChunk {
    openai_stream_chunk(
        None,
        model,
        serde_json::json!({
            "tool_calls": [{
                "index": index,
                "id": id.unwrap_or_default(),
                "type": "function",
                "function": {
                    "name": name.unwrap_or_default(),
                    "arguments": arguments,
                }
            }]
        }),
        Value::Null,
    )
}

pub fn openai_tool_call_delta_chunk(
    model: &str,
    index: u64,
    arguments: String,
) -> OpenAiStreamChunk {
    openai_stream_chunk(
        None,
        model,
        serde_json::json!({
            "tool_calls": [{
                "index": index,
                "function": {"arguments": arguments}
            }]
        }),
        Value::Null,
    )
}

pub fn openai_sse_event(chunk: &OpenAiStreamChunk) -> String {
    format!(
        "data: {}\n\n",
        serde_json::to_string(chunk).unwrap_or_default()
    )
}

pub fn openai_done_event() -> String {
    "data: [DONE]\n\n".to_owned()
}

#[derive(Debug, Clone)]
pub struct OpenAiToAnthropicStreamTranslator {
    model: String,
    current_id: Option<String>,
    started: bool,
    stopped: bool,
    next_content_index: u64,
    text_index: Option<u64>,
    tool_indices: HashMap<u64, u64>,
    open_blocks: HashSet<u64>,
}

impl OpenAiToAnthropicStreamTranslator {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            current_id: None,
            started: false,
            stopped: false,
            next_content_index: 0,
            text_index: None,
            tool_indices: HashMap::new(),
            open_blocks: HashSet::new(),
        }
    }

    pub fn translate_sse_record_to_sse(&mut self, record: &SseRecord) -> Vec<String> {
        if record.data == "[DONE]" {
            return self.stop_events();
        }

        let Ok(value) = serde_json::from_str::<Value>(&record.data) else {
            return Vec::new();
        };
        self.translate_chunk_to_sse(&value)
    }

    fn translate_chunk_to_sse(&mut self, chunk: &Value) -> Vec<String> {
        let mut events = Vec::new();
        if !self.started {
            self.started = true;
            self.current_id = chunk.get("id").and_then(Value::as_str).map(str::to_owned);
            events.push(anthropic_sse_event(&anthropic_message_start_event(
                self.current_id.as_deref(),
                Some(&self.model),
                None,
            )));
        }

        let delta = chunk.pointer("/choices/0/delta").unwrap_or(&Value::Null);
        if let Some(text) = delta.get("content").and_then(Value::as_str)
            && !text.is_empty()
        {
            let index = self.ensure_text_block(&mut events);
            events.push(anthropic_sse_event(&anthropic_text_delta_event(
                Some(index),
                text,
            )));
        }

        if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for tool_call in tool_calls {
                self.translate_tool_call_delta(tool_call, &mut events);
            }
        }

        if let Some(finish_reason) = chunk
            .pointer("/choices/0/finish_reason")
            .and_then(Value::as_str)
        {
            events.extend(self.finish_events(finish_reason));
        }

        events
    }

    fn ensure_text_block(&mut self, events: &mut Vec<String>) -> u64 {
        if let Some(index) = self.text_index {
            return index;
        }
        let index = self.next_index();
        self.text_index = Some(index);
        self.open_blocks.insert(index);
        events.push(anthropic_sse_event(&anthropic_content_block_start_event(
            index, None,
        )));
        index
    }

    fn translate_tool_call_delta(&mut self, tool_call: &Value, events: &mut Vec<String>) {
        let openai_index = tool_call.get("index").and_then(Value::as_u64).unwrap_or(0);
        let index = if let Some(index) = self.tool_indices.get(&openai_index).copied() {
            index
        } else {
            let index = self.next_index();
            self.tool_indices.insert(openai_index, index);
            self.open_blocks.insert(index);
            events.push(anthropic_sse_event(&AnthropicSseEvent::ContentBlockStart {
                index,
                content_block: AnthropicContentBlock {
                    block_type: "tool_use".to_owned(),
                    text: None,
                    id: tool_call
                        .get("id")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    name: tool_call
                        .pointer("/function/name")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    input: Some(serde_json::json!({})),
                },
            }));
            index
        };

        if let Some(arguments) = tool_call
            .pointer("/function/arguments")
            .and_then(Value::as_str)
            && !arguments.is_empty()
        {
            events.push(anthropic_sse_event(&AnthropicSseEvent::ContentBlockDelta {
                index: Some(index),
                delta: AnthropicContentDelta::InputJsonDelta {
                    partial_json: arguments.to_owned(),
                },
            }));
        }
    }

    fn finish_events(&mut self, finish_reason: &str) -> Vec<String> {
        let mut events = Vec::new();
        let mut open_blocks = self.open_blocks.iter().copied().collect::<Vec<_>>();
        open_blocks.sort_unstable();
        for index in open_blocks {
            events.push(anthropic_sse_event(&anthropic_content_block_stop_event(
                index,
            )));
        }
        self.open_blocks.clear();
        events.push(anthropic_sse_event(&anthropic_message_delta_event(Some(
            openai_finish_reason_to_anthropic(finish_reason),
        ))));
        events
    }

    fn stop_events(&mut self) -> Vec<String> {
        if self.stopped {
            return Vec::new();
        }
        self.stopped = true;
        vec![anthropic_sse_event(&anthropic_message_stop_event())]
    }

    fn next_index(&mut self) -> u64 {
        let index = self.next_content_index;
        self.next_content_index += 1;
        index
    }
}

fn openai_finish_reason_to_anthropic(reason: &str) -> &str {
    match reason {
        "length" => "max_tokens",
        "tool_calls" => "tool_use",
        "stop" => "end_turn",
        other => other,
    }
}

#[derive(Debug, Clone)]
pub struct AnthropicStreamTranslator {
    fallback_model: String,
    current_id: Option<String>,
    current_model: Option<String>,
    stopped: bool,
}

impl AnthropicStreamTranslator {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            fallback_model: model.into(),
            current_id: None,
            current_model: None,
            stopped: false,
        }
    }

    pub fn translate_item(&mut self, item: AnthropicStreamItem) -> Vec<OpenAiStreamChunk> {
        match item {
            AnthropicStreamItem::Done => {
                self.stopped = true;
                Vec::new()
            }
            AnthropicStreamItem::Event(event) => self.translate_event(event),
        }
    }

    pub fn translate_sse_record(&mut self, record: &SseRecord) -> Vec<OpenAiStreamChunk> {
        match record.parse_anthropic() {
            Some(item) => self.translate_item(item),
            None => self.translate_event(anthropic_error_event("malformed Anthropic SSE chunk")),
        }
    }

    pub fn translate_item_to_sse(&mut self, item: AnthropicStreamItem) -> Vec<String> {
        match item {
            AnthropicStreamItem::Done => {
                if self.stopped {
                    Vec::new()
                } else {
                    self.stopped = true;
                    vec![openai_done_event()]
                }
            }
            AnthropicStreamItem::Event(event) => self.translate_event_to_sse(event),
        }
    }

    pub fn translate_sse_record_to_sse(&mut self, record: &SseRecord) -> Vec<String> {
        match record.parse_anthropic() {
            Some(item) => self.translate_item_to_sse(item),
            None => {
                self.translate_event_to_sse(anthropic_error_event("malformed Anthropic SSE chunk"))
            }
        }
    }

    pub fn translate_all_to_sse<I>(&mut self, items: I) -> Vec<String>
    where
        I: IntoIterator<Item = AnthropicStreamItem>,
    {
        items
            .into_iter()
            .flat_map(|item| self.translate_item_to_sse(item))
            .collect()
    }

    pub fn is_stopped(&self) -> bool {
        self.stopped
    }

    fn translate_event(&mut self, event: AnthropicSseEvent) -> Vec<OpenAiStreamChunk> {
        match event {
            AnthropicSseEvent::MessageStart { message } => {
                self.current_id = message.id;
                self.current_model = message.model;
                vec![openai_message_start_chunk(
                    self.current_id.as_deref(),
                    self.model(),
                )]
            }
            AnthropicSseEvent::ContentBlockStart {
                index,
                content_block,
            } => match content_block.block_type.as_str() {
                "text" => content_block.text.map_or_else(Vec::new, |text| {
                    vec![openai_text_delta_chunk(self.model(), &text)]
                }),
                "tool_use" => vec![openai_tool_call_start_chunk(
                    self.model(),
                    index,
                    content_block.id.as_deref(),
                    content_block.name.as_deref(),
                    content_block
                        .input
                        .map(|input| input.to_string())
                        .unwrap_or_default(),
                )],
                _ => Vec::new(),
            },
            AnthropicSseEvent::ContentBlockDelta {
                delta: AnthropicContentDelta::TextDelta { text },
                ..
            } => vec![openai_text_delta_chunk(self.model(), &text)],
            AnthropicSseEvent::ContentBlockDelta {
                index,
                delta: AnthropicContentDelta::InputJsonDelta { partial_json },
            } => vec![openai_tool_call_delta_chunk(
                self.model(),
                index.unwrap_or(0),
                partial_json,
            )],
            AnthropicSseEvent::ContentBlockDelta {
                delta: AnthropicContentDelta::Other,
                ..
            } => Vec::new(),
            AnthropicSseEvent::MessageDelta { delta, .. } => vec![openai_message_delta_chunk(
                self.model(),
                delta.stop_reason.as_deref(),
            )],
            AnthropicSseEvent::MessageStop => {
                self.stopped = true;
                Vec::new()
            }
            AnthropicSseEvent::ContentBlockStop { .. }
            | AnthropicSseEvent::Ping
            | AnthropicSseEvent::Error { .. } => Vec::new(),
        }
    }

    fn translate_event_to_sse(&mut self, event: AnthropicSseEvent) -> Vec<String> {
        let stopped = matches!(event, AnthropicSseEvent::MessageStop);
        let mut events: Vec<String> = self
            .translate_event(event)
            .iter()
            .map(openai_sse_event)
            .collect();

        if stopped {
            events.push(openai_done_event());
        }

        events
    }

    fn model(&self) -> &str {
        self.current_model
            .as_deref()
            .unwrap_or(&self.fallback_model)
    }
}

pub fn anthropic_event_to_openai_sse(event: &AnthropicStreamItem, model: &str) -> Option<String> {
    let mut translator = AnthropicStreamTranslator::new(model);
    translator
        .translate_item_to_sse(event.clone())
        .into_iter()
        .next()
}

fn openai_stream_chunk(
    id: Option<&str>,
    model: &str,
    delta: Value,
    finish_reason: Value,
) -> OpenAiStreamChunk {
    OpenAiStreamChunk {
        id: id.unwrap_or("chatcmpl-anthropic").to_owned(),
        object: "chat.completion.chunk".to_owned(),
        created: unix_time(),
        model: model.to_owned(),
        choices: vec![OpenAiStreamChoice {
            index: 0,
            delta,
            finish_reason,
        }],
    }
}

fn map_stop_reason(reason: Option<&str>) -> Value {
    match reason {
        Some("end_turn") | Some("stop_sequence") => Value::String("stop".to_owned()),
        Some("max_tokens") => Value::String("length".to_owned()),
        Some("tool_use") => Value::String("tool_calls".to_owned()),
        Some(other) => Value::String(other.to_owned()),
        None => Value::Null,
    }
}

fn unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_complete_sse_record_across_single_chunk() {
        let mut batcher = ChunkBatcher::new();

        let records = batcher.push(
            b"event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n\n",
        );

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].event.as_deref(), Some("content_block_delta"));
        assert!(records[0].data.contains("text_delta"));
        assert_eq!(batcher.buffered_len(), 0);
    }

    #[test]
    fn parses_split_sse_record_across_multiple_chunks() {
        let mut batcher = ChunkBatcher::new();

        assert!(
            batcher
                .push(b"event: content_block_delta\ndata: {\"type\":")
                .is_empty()
        );
        let records = batcher.push(
            b"\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n\n",
        );

        assert_eq!(records.len(), 1);
        let item = records[0].parse_anthropic().unwrap();
        assert_eq!(
            item,
            AnthropicStreamItem::Event(AnthropicSseEvent::ContentBlockDelta {
                index: None,
                delta: AnthropicContentDelta::TextDelta {
                    text: "Hi".to_owned(),
                },
            })
        );
    }

    #[test]
    fn recovers_after_malformed_chunk() {
        let mut batcher = ChunkBatcher::new();

        assert!(batcher.push(b"not an sse record\n\n").is_empty());
        let records = batcher.push(b"event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n");

        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0].parse_anthropic(),
            Some(AnthropicStreamItem::Event(AnthropicSseEvent::MessageStop))
        );
    }

    #[test]
    fn serializes_openai_sse_event_roundtrip() {
        let chunk = openai_text_delta_chunk("claude-test", "Hi");
        let event = openai_sse_event(&chunk);

        let payload = event
            .strip_prefix("data: ")
            .and_then(|value| value.strip_suffix("\n\n"))
            .unwrap();
        let decoded: OpenAiStreamChunk = serde_json::from_str(payload).unwrap();

        assert_eq!(decoded.model, "claude-test");
        assert_eq!(decoded.object, "chat.completion.chunk");
        assert_eq!(decoded.choices[0].delta, json!({ "content": "Hi" }));
    }

    #[test]
    fn parses_done_sentinel() {
        let mut batcher = ChunkBatcher::new();

        let records = batcher.push(b"data: [DONE]\n\n");

        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0].parse_anthropic(),
            Some(AnthropicStreamItem::Done)
        );
    }

    #[test]
    fn serializes_anthropic_builder_event_roundtrip() {
        let event = anthropic_text_delta_event(Some(0), "Hi");
        let sse = anthropic_sse_event(&event);

        assert!(sse.starts_with("event: content_block_delta\n"));
        let record = ChunkBatcher::new().push(sse.as_bytes()).remove(0);

        assert_eq!(
            record.parse_anthropic(),
            Some(AnthropicStreamItem::Event(anthropic_text_delta_event(
                Some(0),
                "Hi"
            )))
        );
    }

    #[test]
    fn translates_full_anthropic_stream_to_openai_sse() {
        let mut translator = AnthropicStreamTranslator::new("fallback-model");
        let events = vec![
            AnthropicStreamItem::Event(anthropic_message_start_event(
                Some("msg_1"),
                Some("claude-test"),
                None,
            )),
            AnthropicStreamItem::Event(anthropic_content_block_start_event(0, None)),
            AnthropicStreamItem::Event(anthropic_text_delta_event(Some(0), "Hello")),
            AnthropicStreamItem::Event(anthropic_content_block_stop_event(0)),
            AnthropicStreamItem::Event(anthropic_message_delta_event(Some("end_turn"))),
            AnthropicStreamItem::Event(anthropic_message_stop_event()),
            AnthropicStreamItem::Done,
        ];

        let sse_events = translator.translate_all_to_sse(events);

        assert_eq!(sse_events.len(), 4);
        let chunks = decode_openai_sse_chunks(&sse_events[..3]);
        assert_eq!(chunks[0].id, "msg_1");
        assert_eq!(chunks[0].model, "claude-test");
        assert_eq!(chunks[0].choices[0].delta, json!({ "role": "assistant" }));
        assert_eq!(chunks[1].choices[0].delta, json!({ "content": "Hello" }));
        assert_eq!(chunks[2].choices[0].finish_reason, json!("stop"));
        assert_eq!(sse_events[3], openai_done_event());
        assert!(translator.is_stopped());
    }

    #[test]
    fn translates_multiple_text_deltas_in_order() {
        let mut translator = AnthropicStreamTranslator::new("claude-test");
        let events = vec![
            AnthropicStreamItem::Event(anthropic_text_delta_event(Some(0), "Hel")),
            AnthropicStreamItem::Event(anthropic_text_delta_event(Some(0), "lo")),
        ];

        let sse_events = translator.translate_all_to_sse(events);
        let chunks = decode_openai_sse_chunks(&sse_events);
        let text: String = chunks
            .iter()
            .filter_map(|chunk| {
                chunk.choices[0]
                    .delta
                    .get("content")
                    .and_then(Value::as_str)
            })
            .collect();

        assert_eq!(text, "Hello");
    }

    #[test]
    fn translates_empty_stream_to_no_events() {
        let mut translator = AnthropicStreamTranslator::new("claude-test");

        let sse_events = translator.translate_all_to_sse(Vec::new());

        assert!(sse_events.is_empty());
        assert!(!translator.is_stopped());
    }

    #[test]
    fn handles_error_and_malformed_events_without_openai_chunks() {
        let mut translator = AnthropicStreamTranslator::new("claude-test");
        let malformed_record = SseRecord {
            event: Some("content_block_delta".to_owned()),
            data: "not json".to_owned(),
        };

        assert!(
            translator
                .translate_item_to_sse(AnthropicStreamItem::Event(anthropic_error_event(
                    "bad chunk"
                )))
                .is_empty()
        );
        assert!(
            translator
                .translate_sse_record_to_sse(&malformed_record)
                .is_empty()
        );
    }

    fn decode_openai_sse_chunks(events: &[String]) -> Vec<OpenAiStreamChunk> {
        events
            .iter()
            .map(|event| {
                let payload = event
                    .strip_prefix("data: ")
                    .and_then(|value| value.strip_suffix("\n\n"))
                    .unwrap();
                serde_json::from_str(payload).unwrap()
            })
            .collect()
    }
}
