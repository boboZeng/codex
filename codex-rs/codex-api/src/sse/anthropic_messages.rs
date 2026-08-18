use crate::common::ResponseEvent;
use crate::common::ResponseStream;
use crate::error::ApiError;
use crate::telemetry::SseTelemetry;
use codex_client::StreamResponse;
use codex_protocol::ResponseItemId;
use codex_protocol::ToolName;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ReasoningItemContent;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::TokenUsage;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio::time::timeout;
use tracing::debug;
use tracing::trace;

const REQUEST_ID_HEADER: &str = "request-id";

#[derive(Clone, Debug, Default)]
pub(crate) struct AnthropicToolNameMap {
    anthropic_to_tool_name: HashMap<String, ToolName>,
    anthropic_to_tool_kind: HashMap<String, AnthropicToolKind>,
    tool_name_to_anthropic: HashMap<ToolName, String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum AnthropicToolKind {
    #[default]
    Function,
    Custom,
}

impl AnthropicToolNameMap {
    pub(crate) fn insert(
        &mut self,
        anthropic_name: String,
        tool_name: ToolName,
        tool_kind: AnthropicToolKind,
    ) -> Result<(), ApiError> {
        if let Some(existing) = self.anthropic_to_tool_name.get(&anthropic_name)
            && existing != &tool_name
        {
            return Err(ApiError::Stream(format!(
                "Anthropic tool name collision for {anthropic_name}"
            )));
        }
        if let Some(existing) = self.tool_name_to_anthropic.get(&tool_name)
            && existing != &anthropic_name
        {
            return Err(ApiError::Stream(format!(
                "multiple Anthropic tool names map to {tool_name}"
            )));
        }

        self.anthropic_to_tool_name
            .insert(anthropic_name.clone(), tool_name.clone());
        self.anthropic_to_tool_kind
            .insert(anthropic_name.clone(), tool_kind);
        self.tool_name_to_anthropic
            .insert(tool_name, anthropic_name);
        Ok(())
    }

    pub(crate) fn tool_name(&self, anthropic_name: &str) -> ToolName {
        self.anthropic_to_tool_name
            .get(anthropic_name)
            .cloned()
            .unwrap_or_else(|| ToolName::plain(anthropic_name))
    }

    pub(crate) fn anthropic_name(&self, tool_name: &ToolName) -> Option<&str> {
        self.tool_name_to_anthropic
            .get(tool_name)
            .map(String::as_str)
    }

    pub(crate) fn tool_kind(&self, anthropic_name: &str) -> AnthropicToolKind {
        self.anthropic_to_tool_kind
            .get(anthropic_name)
            .copied()
            .unwrap_or_default()
    }
}

pub fn spawn_anthropic_messages_stream(
    stream_response: StreamResponse,
    idle_timeout: Duration,
    telemetry: Option<Arc<dyn SseTelemetry>>,
    tool_name_map: AnthropicToolNameMap,
) -> ResponseStream {
    let upstream_request_id = stream_response
        .headers
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let (tx_event, rx_event) = mpsc::channel::<Result<ResponseEvent, ApiError>>(1600);
    tokio::spawn(process_sse(
        stream_response.bytes,
        tx_event,
        idle_timeout,
        telemetry,
        tool_name_map,
    ));
    ResponseStream {
        rx_event,
        upstream_request_id,
    }
}

#[derive(Debug, Deserialize)]
struct AnthropicEvent {
    #[serde(rename = "type")]
    kind: String,
    message: Option<AnthropicMessage>,
    index: Option<usize>,
    content_block: Option<AnthropicContentBlock>,
    delta: Option<AnthropicDelta>,
    usage: Option<AnthropicUsage>,
    error: Option<AnthropicError>,
}

#[derive(Debug, Deserialize)]
struct AnthropicMessage {
    id: String,
    #[serde(default)]
    usage: AnthropicUsage,
}

#[derive(Debug, Default, Deserialize)]
struct AnthropicUsage {
    #[serde(default)]
    input_tokens: i64,
    #[serde(default)]
    output_tokens: i64,
    #[serde(default)]
    cache_read_input_tokens: i64,
    #[serde(default)]
    cache_creation_input_tokens: i64,
}

#[derive(Debug, Deserialize)]
struct AnthropicContentBlock {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
    id: Option<String>,
    name: Option<String>,
    input: Option<Value>,
    thinking: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AnthropicDelta {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
    thinking: Option<String>,
    partial_json: Option<String>,
    stop_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AnthropicError {
    #[serde(rename = "type")]
    kind: String,
    message: String,
}

#[derive(Debug)]
enum ActiveContentBlock {
    Text {
        item: ResponseItem,
    },
    ToolUse {
        id: String,
        tool_name: ToolName,
        kind: AnthropicToolKind,
        input: String,
    },
    Reasoning {
        item: ResponseItem,
    },
}

async fn process_sse(
    stream: codex_client::ByteStream,
    tx_event: mpsc::Sender<Result<ResponseEvent, ApiError>>,
    idle_timeout: Duration,
    telemetry: Option<Arc<dyn SseTelemetry>>,
    tool_name_map: AnthropicToolNameMap,
) {
    let mut stream = stream.eventsource();
    let mut response_id = None;
    let mut usage = AnthropicUsage::default();
    let mut stop_reason = None;
    let mut active_blocks = HashMap::new();

    loop {
        let start = Instant::now();
        let response = timeout(idle_timeout, stream.next()).await;
        if let Some(telemetry) = telemetry.as_ref() {
            telemetry.on_sse_poll(&response, start.elapsed());
        }
        let sse = match response {
            Ok(Some(Ok(sse))) => sse,
            Ok(Some(Err(error))) => {
                let _ = tx_event
                    .send(Err(ApiError::Stream(error.to_string())))
                    .await;
                return;
            }
            Ok(None) => {
                let _ = tx_event
                    .send(Err(ApiError::Stream(
                        "stream closed before message_stop".to_string(),
                    )))
                    .await;
                return;
            }
            Err(_) => {
                let _ = tx_event
                    .send(Err(ApiError::Stream(
                        "idle timeout waiting for SSE".to_string(),
                    )))
                    .await;
                return;
            }
        };

        trace!("Anthropic Messages SSE event: {}", sse.data);
        let event = match serde_json::from_str::<AnthropicEvent>(&sse.data) {
            Ok(event) => event,
            Err(error) => {
                debug!("failed to parse Anthropic Messages event: {error}");
                continue;
            }
        };

        match event.kind.as_str() {
            "message_start" => {
                if let Some(message) = event.message {
                    response_id = Some(message.id);
                    usage = message.usage;
                }
                if tx_event
                    .send(Ok(ResponseEvent::Created {
                        response_id: response_id.clone(),
                    }))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            "content_block_start" => {
                let (Some(index), Some(content_block)) = (event.index, event.content_block) else {
                    continue;
                };
                match content_block.kind.as_str() {
                    "text" => {
                        let item = assistant_message(content_block.text.unwrap_or_default());
                        if tx_event
                            .send(Ok(ResponseEvent::OutputItemAdded(item.clone())))
                            .await
                            .is_err()
                        {
                            return;
                        }
                        active_blocks.insert(index, ActiveContentBlock::Text { item });
                    }
                    "tool_use" => {
                        let Some(id) = content_block.id else {
                            continue;
                        };
                        let Some(name) = content_block.name else {
                            continue;
                        };
                        let input = match content_block.input {
                            Some(Value::Object(input)) if input.is_empty() => String::new(),
                            Some(input) => input.to_string(),
                            None => String::new(),
                        };
                        let tool_name = tool_name_map.tool_name(&name);
                        let kind = tool_name_map.tool_kind(&name);
                        let item = tool_call(&id, &tool_name, kind, input.clone());
                        if tx_event
                            .send(Ok(ResponseEvent::OutputItemAdded(item)))
                            .await
                            .is_err()
                        {
                            return;
                        }
                        active_blocks.insert(
                            index,
                            ActiveContentBlock::ToolUse {
                                id,
                                tool_name,
                                kind,
                                input,
                            },
                        );
                    }
                    "thinking" => {
                        let item = reasoning_message(content_block.thinking.unwrap_or_default());
                        if tx_event
                            .send(Ok(ResponseEvent::OutputItemAdded(item.clone())))
                            .await
                            .is_err()
                        {
                            return;
                        }
                        active_blocks.insert(index, ActiveContentBlock::Reasoning { item });
                    }
                    _ => {}
                }
            }
            "content_block_delta" => {
                let (Some(index), Some(delta)) = (event.index, event.delta) else {
                    continue;
                };
                match (active_blocks.get_mut(&index), delta.kind.as_str()) {
                    (Some(ActiveContentBlock::Text { .. }), "text_delta") => {
                        if let Some(text) = delta.text {
                            if let Some(ActiveContentBlock::Text { item }) =
                                active_blocks.get_mut(&index)
                                && let ResponseItem::Message { content, .. } = item
                                && let Some(ContentItem::OutputText {
                                    text: buffered_text,
                                }) = content.first_mut()
                            {
                                buffered_text.push_str(&text);
                            }
                            if tx_event
                                .send(Ok(ResponseEvent::OutputTextDelta(text)))
                                .await
                                .is_err()
                            {
                                return;
                            }
                        }
                    }
                    (Some(ActiveContentBlock::ToolUse { id, input, .. }), "input_json_delta") => {
                        if let Some(partial_json) = delta.partial_json {
                            input.push_str(&partial_json);
                            if tx_event
                                .send(Ok(ResponseEvent::ToolCallInputDelta {
                                    item_id: id.clone(),
                                    call_id: Some(id.clone()),
                                    delta: partial_json,
                                }))
                                .await
                                .is_err()
                            {
                                return;
                            }
                        }
                    }
                    (Some(ActiveContentBlock::Reasoning { item }), "thinking_delta") => {
                        if let Some(thinking) = delta.thinking {
                            if let ResponseItem::Reasoning { content, .. } = item
                                && let Some(content) = content
                                && let Some(ReasoningItemContent::ReasoningText { text }) =
                                    content.first_mut()
                            {
                                text.push_str(&thinking);
                            }
                            if tx_event
                                .send(Ok(ResponseEvent::ReasoningContentDelta {
                                    delta: thinking,
                                    content_index: 0,
                                }))
                                .await
                                .is_err()
                            {
                                return;
                            }
                        }
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                let Some(index) = event.index else {
                    continue;
                };
                let Some(block) = active_blocks.remove(&index) else {
                    continue;
                };
                let item = match block {
                    ActiveContentBlock::Text { item } => item,
                    ActiveContentBlock::ToolUse {
                        id,
                        tool_name,
                        kind,
                        input,
                    } => {
                        let input = if !input.trim().is_empty() {
                            input
                        } else {
                            "{}".to_string()
                        };
                        tool_call(&id, &tool_name, kind, input)
                    }
                    ActiveContentBlock::Reasoning { item } => item,
                };
                if tx_event
                    .send(Ok(ResponseEvent::OutputItemDone(item)))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            "message_delta" => {
                if let Some(event_usage) = event.usage {
                    usage.output_tokens = event_usage.output_tokens;
                }
                if let Some(reason) = event.delta.and_then(|delta| delta.stop_reason) {
                    trace!("Anthropic Messages stop reason: {reason}");
                    stop_reason = Some(reason);
                }
            }
            "message_stop" => {
                let response_id = response_id.unwrap_or_else(|| "anthropic-message".to_string());
                let token_usage = TokenUsage {
                    input_tokens: usage.input_tokens,
                    cached_input_tokens: usage.cache_read_input_tokens,
                    cache_write_input_tokens: usage.cache_creation_input_tokens,
                    output_tokens: usage.output_tokens,
                    reasoning_output_tokens: 0,
                    total_tokens: usage.input_tokens + usage.output_tokens,
                    codex_rollout_budget_units: None,
                };
                let end_turn = match stop_reason.as_deref() {
                    Some("end_turn") => Some(true),
                    Some("tool_use") => Some(false),
                    Some("max_tokens" | "stop_sequence" | "pause_turn") | None => None,
                    Some(reason) => {
                        debug!("Unknown Anthropic Messages stop reason: {reason}");
                        None
                    }
                };
                let _ = tx_event
                    .send(Ok(ResponseEvent::Completed {
                        response_id,
                        token_usage: Some(token_usage),
                        usage_metadata: None,
                        end_turn,
                    }))
                    .await;
                return;
            }
            "error" => {
                let error = event.error.map_or_else(
                    || ApiError::Stream("Anthropic Messages API returned an error".to_string()),
                    |error| ApiError::Stream(format!("{}: {}", error.kind, error.message)),
                );
                let _ = tx_event.send(Err(error)).await;
                return;
            }
            "ping" => {}
            _ => debug!("unhandled Anthropic Messages SSE event: {}", event.kind),
        }
    }
}

fn assistant_message(text: String) -> ResponseItem {
    ResponseItem::Message {
        id: Some(ResponseItemId::new("msg")),
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText { text }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

fn reasoning_message(text: String) -> ResponseItem {
    ResponseItem::Reasoning {
        id: Some(ResponseItemId::new("rs")),
        summary: Vec::new(),
        content: Some(vec![ReasoningItemContent::ReasoningText { text }]),
        encrypted_content: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

fn function_call(call_id: &str, tool_name: &ToolName, arguments: String) -> ResponseItem {
    ResponseItem::FunctionCall {
        id: Some(ResponseItemId::new("fc")),
        name: tool_name.name.clone(),
        namespace: tool_name.namespace.clone(),
        arguments,
        encrypted_function_args: None,
        call_id: call_id.to_string(),
        internal_chat_message_metadata_passthrough: None,
    }
}

fn tool_call(
    call_id: &str,
    tool_name: &ToolName,
    kind: AnthropicToolKind,
    input: String,
) -> ResponseItem {
    match kind {
        AnthropicToolKind::Function => function_call(call_id, tool_name, input),
        AnthropicToolKind::Custom => ResponseItem::CustomToolCall {
            id: Some(ResponseItemId::new("ctc")),
            status: None,
            call_id: call_id.to_string(),
            name: tool_name.name.clone(),
            namespace: tool_name.namespace.clone(),
            input: custom_tool_input(input),
            internal_chat_message_metadata_passthrough: None,
        },
    }
}

fn custom_tool_input(input: String) -> String {
    serde_json::from_str::<Value>(&input)
        .ok()
        .and_then(|value| {
            value
                .get("input")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or(input)
}

#[cfg(test)]
#[path = "anthropic_messages_tests.rs"]
mod tests;
