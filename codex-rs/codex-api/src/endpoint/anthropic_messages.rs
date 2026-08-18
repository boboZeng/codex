use crate::auth::SharedAuthProvider;
use crate::common::ResponseStream;
use crate::common::ResponsesApiRequest;
use crate::endpoint::session::EndpointSession;
use crate::error::ApiError;
use crate::provider::Provider;
use crate::requests::Compression;
use crate::sse::anthropic_messages::AnthropicToolKind;
use crate::sse::anthropic_messages::AnthropicToolNameMap;
use crate::sse::spawn_anthropic_messages_stream;
use crate::telemetry::SseTelemetry;
use codex_client::EncodedJsonBody;
use codex_client::HttpTransport;
use codex_client::RequestCompression;
use codex_client::RequestTelemetry;
use codex_protocol::DEFAULT_FUNCTION_NAMESPACE;
use codex_protocol::ToolName;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::ReasoningEffort;
use http::HeaderMap;
use http::HeaderValue;
use http::Method;
use serde_json::Map;
use serde_json::Value;
use std::sync::Arc;
use tracing::instrument;

const ANTHROPIC_MESSAGES_SHELL_RELIABILITY_INSTRUCTIONS: &str = "When executing shell commands, honor the shell specified in the environment context. In zsh, never put an unquoted wildcard anywhere in a command, including path operands such as `src/*.kt`; quote it or prefer `rg -g '*.kt' <pattern> <directory>`. Use `rg -g`, not `--include`. Do not use a file extension as an `rg --type` value; prefer quoted `-g` filters unless you have verified the type with `rg --type-list`. When locating source code, first search for the identifier itself across the repository instead of assuming its module, file path, or declaration syntax, for example search `AgentWsCommand` before `class AgentWsCommand`. Do not redirect stderr to `/dev/null` while locating files because it hides missing-path diagnostics. When searching Android resource identifiers, include `*.xml` files as well as source files; string resource keys commonly live in `strings.xml`. For HTTP requests, do not background the command or suppress diagnostics while waiting for its result; use `curl --fail --show-error --location`, check the exit status, and verify any output file. An exit status of 1 from `rg` or `grep` means that exact pattern had no matches, not that the requested code or behavior does not exist. Broaden the search or inspect the relevant files before concluding that no change is needed.";

const DEFAULT_ANTHROPIC_MAX_TOKENS: u64 = 8192;
const ANTHROPIC_MESSAGES_CONTINUATION_PROMPT: &str = "Continue.";

/// Client for providers that implement Anthropic's Messages API.
///
/// It accepts Codex's provider-neutral Responses request and translates the
/// conversation, function tools, and streaming events at the protocol edge.
/// Keeping this translation here allows the session runtime to continue using
/// its existing `ResponseEvent` and `ResponseItem` abstractions.
pub struct AnthropicMessagesClient<T: HttpTransport> {
    session: EndpointSession<T>,
    sse_telemetry: Option<Arc<dyn SseTelemetry>>,
}

/// Controls whether Messages requests include Anthropic prompt-cache breakpoints.
///
/// The provider selection layer keeps this explicit so incompatible gateways
/// can opt out of sending `cache_control`.
#[derive(Debug, Clone, Copy)]
pub enum AnthropicPromptCaching {
    Disabled,
    SystemAndTools,
    RollingHistory,
}

impl<T: HttpTransport> AnthropicMessagesClient<T> {
    pub fn new(transport: T, provider: Provider, auth: SharedAuthProvider) -> Self {
        Self {
            session: EndpointSession::new(transport, provider, auth),
            sse_telemetry: None,
        }
    }

    pub fn with_telemetry(
        self,
        request: Option<Arc<dyn RequestTelemetry>>,
        sse: Option<Arc<dyn SseTelemetry>>,
    ) -> Self {
        Self {
            session: self.session.with_request_telemetry(request),
            sse_telemetry: sse,
        }
    }

    #[instrument(
        name = "anthropic_messages.stream_request",
        level = "info",
        skip_all,
        fields(
            transport = "anthropic_messages_http",
            http.method = "POST",
            api.path = "messages"
        )
    )]
    pub async fn stream_request(
        &self,
        request: ResponsesApiRequest,
        extra_headers: HeaderMap,
        compression: Compression,
        prompt_caching: AnthropicPromptCaching,
    ) -> Result<ResponseStream, ApiError> {
        let AnthropicMessagesRequest {
            body,
            tool_name_map,
        } = anthropic_messages_request(request, prompt_caching)?;
        let body = EncodedJsonBody::encode(&body).map_err(|error| {
            ApiError::Stream(format!("failed to encode Messages request: {error}"))
        })?;
        let request_compression = match compression {
            Compression::None => RequestCompression::None,
            Compression::Zstd => RequestCompression::Zstd,
        };
        let stream_response = self
            .session
            .stream_encoded_json_with(
                Method::POST,
                "messages",
                extra_headers,
                Some(body),
                |request| {
                    request.headers.insert(
                        http::header::ACCEPT,
                        HeaderValue::from_static("text/event-stream"),
                    );
                    request.compression = request_compression;
                },
            )
            .await?;

        Ok(spawn_anthropic_messages_stream(
            stream_response,
            self.session.provider().stream_idle_timeout,
            self.sse_telemetry.clone(),
            tool_name_map,
        ))
    }
}

struct AnthropicMessagesRequest {
    body: Value,
    tool_name_map: AnthropicToolNameMap,
}

fn anthropic_messages_request(
    request: ResponsesApiRequest,
    prompt_caching: AnthropicPromptCaching,
) -> Result<AnthropicMessagesRequest, ApiError> {
    let mut raw_tools = request
        .tools
        .as_ref()
        .map(|tools| {
            serde_json::to_value(tools).map_err(|error| {
                ApiError::Stream(format!("failed to serialize Responses tools: {error}"))
            })
        })
        .transpose()?
        .map(|tools| tools.as_array().cloned().unwrap_or_default())
        .unwrap_or_default();
    let tool_choice = anthropic_tool_choice(&request.tool_choice, request.parallel_tool_calls);
    let additional_tools = request
        .input
        .iter()
        .filter_map(|item| match item {
            ResponseItem::AdditionalTools { tools, .. } => Some(tools.clone()),
            _ => None,
        })
        .flatten()
        .collect::<Vec<_>>();
    raw_tools.extend(additional_tools);
    let (mut tools, tool_name_map) = anthropic_tools(Value::Array(raw_tools))?;
    if tool_choice.is_none() {
        tools.clear();
    }
    let mut system = request.instructions;
    let mut messages = Vec::new();
    let mut latest_content_start = None;

    for item in request.input {
        match item {
            ResponseItem::Message { role, content, .. }
                if role == "developer" || role == "system" =>
            {
                append_system_text(&mut system, content_text(&content));
            }
            ResponseItem::Message { role, content, .. }
                if role == "user" || role == "assistant" =>
            {
                let content = content
                    .into_iter()
                    .filter_map(content_item_to_anthropic)
                    .collect::<Vec<_>>();
                latest_content_start = append_message(&mut messages, role, content);
            }
            ResponseItem::Message { .. } => {}
            ResponseItem::FunctionCall {
                name,
                namespace,
                arguments,
                call_id,
                ..
            } => {
                let input =
                    serde_json::from_str(&arguments).unwrap_or_else(|_| Value::Object(Map::new()));
                let original_tool_name = ToolName::new(namespace, name);
                let anthropic_name = tool_name_map
                    .anthropic_name(&original_tool_name)
                    .unwrap_or(original_tool_name.name.as_str());
                latest_content_start = append_message(
                    &mut messages,
                    "assistant".to_string(),
                    vec![serde_json::json!({
                        "type": "tool_use",
                        "id": call_id,
                        "name": anthropic_name,
                        "input": input,
                    })],
                );
            }
            ResponseItem::CustomToolCall {
                call_id,
                name,
                namespace,
                input,
                ..
            } => {
                let original_tool_name = ToolName::new(namespace, name);
                let anthropic_name = tool_name_map
                    .anthropic_name(&original_tool_name)
                    .unwrap_or(original_tool_name.name.as_str());
                latest_content_start = append_message(
                    &mut messages,
                    "assistant".to_string(),
                    vec![serde_json::json!({
                        "type": "tool_use",
                        "id": call_id,
                        "name": anthropic_name,
                        "input": { "input": input },
                    })],
                );
            }
            ResponseItem::FunctionCallOutput {
                call_id: Some(call_id),
                output,
                ..
            } => {
                latest_content_start = append_message(
                    &mut messages,
                    "user".to_string(),
                    vec![serde_json::json!({
                        "type": "tool_result",
                        "tool_use_id": call_id,
                        "content": tool_output_to_anthropic(output),
                    })],
                );
            }
            ResponseItem::FunctionCallOutput { call_id: None, .. } => {}
            ResponseItem::CustomToolCallOutput {
                call_id, output, ..
            } => {
                latest_content_start = append_message(
                    &mut messages,
                    "user".to_string(),
                    vec![serde_json::json!({
                        "type": "tool_result",
                        "tool_use_id": call_id,
                        "content": tool_output_to_anthropic(output),
                    })],
                );
            }
            ResponseItem::AdditionalTools { .. }
            | ResponseItem::AgentMessage { .. }
            | ResponseItem::Reasoning { .. }
            | ResponseItem::LocalShellCall { .. }
            | ResponseItem::ToolSearchCall { .. }
            | ResponseItem::ToolSearchOutput { .. }
            | ResponseItem::WebSearchCall { .. }
            | ResponseItem::ImageGenerationCall { .. }
            | ResponseItem::Compaction { .. }
            | ResponseItem::CompactionTrigger { .. }
            | ResponseItem::ContextCompaction { .. }
            | ResponseItem::ConfigurationUpdate { .. }
            | ResponseItem::Other => {}
        }
    }
    let ends_with_assistant_prefill = messages.last().is_some_and(|message| {
        message.get("role").and_then(Value::as_str) == Some("assistant")
            && !message
                .get("content")
                .and_then(Value::as_array)
                .is_some_and(|content| {
                    content
                        .iter()
                        .any(|item| item.get("type").and_then(Value::as_str) == Some("tool_use"))
                })
    });
    if ends_with_assistant_prefill {
        // Bedrock Claude rejects Anthropic assistant-prefill requests. Keep the
        // assistant history, but make the next model turn an explicit user turn.
        append_message(
            &mut messages,
            "user".to_string(),
            vec![serde_json::json!({
                "type": "text",
                "text": ANTHROPIC_MESSAGES_CONTINUATION_PROMPT,
            })],
        );
    }
    append_system_text(
        &mut system,
        ANTHROPIC_MESSAGES_SHELL_RELIABILITY_INSTRUCTIONS.to_string(),
    );

    if matches!(prompt_caching, AnthropicPromptCaching::RollingHistory) {
        add_history_cache_checkpoint(&mut messages, latest_content_start);
    }

    let mut body = serde_json::json!({
        "model": request.model,
        "max_tokens": anthropic_max_tokens(request.reasoning.as_ref()),
        "stream": true,
        "messages": messages,
    });
    if !system.trim().is_empty() {
        body["system"] = match prompt_caching {
            AnthropicPromptCaching::Disabled => Value::String(system),
            AnthropicPromptCaching::SystemAndTools | AnthropicPromptCaching::RollingHistory => {
                serde_json::json!([{
                    "type": "text",
                    "text": system,
                    "cache_control": { "type": "ephemeral" },
                }])
            }
        };
    }
    if let Some(thinking) = anthropic_thinking(request.reasoning.as_ref()) {
        body["thinking"] = thinking;
    }
    if let Some(output_config) = request.text.as_ref().and_then(anthropic_output_config) {
        body["output_config"] = output_config;
    }
    if let Some(service_tier) = request.service_tier {
        body["service_tier"] = Value::String(service_tier);
    }
    if !tools.is_empty() {
        if matches!(
            prompt_caching,
            AnthropicPromptCaching::SystemAndTools | AnthropicPromptCaching::RollingHistory
        ) && let Some(last_tool) = tools.last_mut()
            && let Some(last_tool) = last_tool.as_object_mut()
        {
            last_tool.insert(
                "cache_control".to_string(),
                serde_json::json!({ "type": "ephemeral" }),
            );
        }
        body["tools"] = Value::Array(tools);
        if let Some(tool_choice) = tool_choice {
            body["tool_choice"] = tool_choice;
        }
    }
    Ok(AnthropicMessagesRequest {
        body,
        tool_name_map,
    })
}

fn anthropic_max_tokens(reasoning: Option<&crate::common::Reasoning>) -> u64 {
    reasoning
        .and_then(|reasoning| reasoning_budget(reasoning.effort.as_ref()))
        .map_or(DEFAULT_ANTHROPIC_MAX_TOKENS, |budget| {
            DEFAULT_ANTHROPIC_MAX_TOKENS.max(budget.saturating_add(1024))
        })
}

fn anthropic_thinking(reasoning: Option<&crate::common::Reasoning>) -> Option<Value> {
    reasoning
        .and_then(|reasoning| reasoning_budget(reasoning.effort.as_ref()))
        .map(|budget_tokens| {
            serde_json::json!({
                "type": "enabled",
                "budget_tokens": budget_tokens,
            })
        })
}

fn reasoning_budget(effort: Option<&ReasoningEffort>) -> Option<u64> {
    match effort {
        None | Some(ReasoningEffort::None) => None,
        Some(ReasoningEffort::Minimal) => Some(1024),
        Some(ReasoningEffort::Low) => Some(2048),
        Some(ReasoningEffort::Medium) => Some(4096),
        Some(ReasoningEffort::High) => Some(8192),
        Some(ReasoningEffort::XHigh) => Some(12_288),
        Some(ReasoningEffort::Max) => Some(16_384),
        Some(ReasoningEffort::Ultra) => Some(24_576),
        Some(ReasoningEffort::Persistent) => None,
        Some(ReasoningEffort::Custom(value)) => value.parse().ok().filter(|value| *value > 0),
    }
}

fn anthropic_output_config(text: &crate::common::TextControls) -> Option<Value> {
    let format = text.format.as_ref()?;
    Some(serde_json::json!({
        "format": {
            "type": "json_schema",
            "schema": format.schema,
        }
    }))
}

fn anthropic_tool_choice(tool_choice: &str, parallel_tool_calls: bool) -> Option<Value> {
    let tool_choice_type = match tool_choice {
        "none" => return None,
        "required" | "any" => "any",
        _ => "auto",
    };
    Some(serde_json::json!({
        "type": tool_choice_type,
        "disable_parallel_tool_use": !parallel_tool_calls,
    }))
}

fn append_system_text(system: &mut String, text: String) {
    if text.trim().is_empty() {
        return;
    }
    if !system.trim().is_empty() {
        system.push_str("\n\n");
    }
    system.push_str(&text);
}

#[derive(Clone, Copy)]
struct MessageContentStart {
    message_index: usize,
    content_index: usize,
}

fn append_message(
    messages: &mut Vec<Value>,
    role: String,
    content: Vec<Value>,
) -> Option<MessageContentStart> {
    if content.is_empty() {
        return None;
    }
    let last_message_index = messages.len().checked_sub(1);
    if let Some(last_message_index) = last_message_index
        && let Some(last) = messages.last_mut()
        && last.get("role").and_then(Value::as_str) == Some(role.as_str())
        && let Some(existing_content) = last.get_mut("content").and_then(Value::as_array_mut)
    {
        let content_start = MessageContentStart {
            message_index: last_message_index,
            content_index: existing_content.len(),
        };
        existing_content.extend(content);
        return Some(content_start);
    }
    messages.push(serde_json::json!({ "role": role, "content": content }));
    Some(MessageContentStart {
        message_index: messages.len() - 1,
        content_index: 0,
    })
}

fn add_history_cache_checkpoint(
    messages: &mut [Value],
    latest_content_start: Option<MessageContentStart>,
) {
    let Some(latest_content_start) = latest_content_start else {
        return;
    };

    let checkpoint = if latest_content_start.content_index > 0 {
        Some((
            latest_content_start.message_index,
            latest_content_start.content_index - 1,
        ))
    } else {
        messages[..latest_content_start.message_index]
            .iter()
            .enumerate()
            .rev()
            .find_map(|(message_index, message)| {
                message
                    .get("content")
                    .and_then(Value::as_array)
                    .and_then(|content| content.len().checked_sub(1))
                    .map(|content_index| (message_index, content_index))
            })
    };

    let Some((message_index, content_index)) = checkpoint else {
        return;
    };
    let Some(content) = messages[message_index]
        .get_mut("content")
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    let Some(content_block) = content
        .get_mut(content_index)
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    content_block.insert(
        "cache_control".to_string(),
        serde_json::json!({ "type": "ephemeral" }),
    );
}

fn content_text(content: &[ContentItem]) -> String {
    content
        .iter()
        .filter_map(|item| match item {
            ContentItem::InputText { text } | ContentItem::OutputText { text }
                if !text.trim().is_empty() =>
            {
                Some(text.as_str())
            }
            ContentItem::InputImage { .. } | ContentItem::InputAudio { .. } => None,
            ContentItem::InputText { .. } | ContentItem::OutputText { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn content_item_to_anthropic(item: ContentItem) -> Option<Value> {
    match item {
        ContentItem::InputText { text } | ContentItem::OutputText { text }
            if !text.trim().is_empty() =>
        {
            Some(serde_json::json!({ "type": "text", "text": text }))
        }
        ContentItem::InputImage { image_url, .. } => data_url_image_block(&image_url),
        ContentItem::InputText { .. }
        | ContentItem::OutputText { .. }
        | ContentItem::InputAudio { .. } => None,
    }
}

fn data_url_image_block(image_url: &str) -> Option<Value> {
    if let Some(data_url) = image_url.strip_prefix("data:")
        && let Some((metadata, data)) = data_url.split_once(',')
        && let Some(media_type) = metadata.strip_suffix(";base64")
    {
        return Some(serde_json::json!({
            "type": "image",
            "source": { "type": "base64", "media_type": media_type, "data": data },
        }));
    }

    (image_url.starts_with("http://") || image_url.starts_with("https://")).then(|| {
        serde_json::json!({
            "type": "image",
            "source": { "type": "url", "url": image_url },
        })
    })
}

fn tool_output_to_anthropic(output: FunctionCallOutputPayload) -> Value {
    match serde_json::to_value(output) {
        Ok(Value::String(text)) if !text.trim().is_empty() => Value::String(text),
        Ok(Value::String(_)) => Value::String("Tool completed with no output.".to_string()),
        Ok(Value::Array(content)) => {
            let content = content
                .into_iter()
                .filter_map(|item| {
                    item.get("text")
                        .and_then(Value::as_str)
                        .filter(|text| !text.trim().is_empty())
                        .map(|text| serde_json::json!({ "type": "text", "text": text }))
                })
                .collect::<Vec<_>>();
            if content.is_empty() {
                Value::String("Tool completed with no output.".to_string())
            } else {
                Value::Array(content)
            }
        }
        Ok(value) => Value::String(value.to_string()),
        Err(_) => Value::String("Tool returned an unserializable result.".to_string()),
    }
}

fn anthropic_tools(tools: Value) -> Result<(Vec<Value>, AnthropicToolNameMap), ApiError> {
    let mut anthropic_tools = Vec::new();
    let mut tool_name_map = AnthropicToolNameMap::default();

    for tool in tools.as_array().into_iter().flatten() {
        match tool.get("type").and_then(Value::as_str) {
            Some("function") => {
                let Some(name) = tool.get("name").and_then(Value::as_str) else {
                    continue;
                };
                let tool_name = ToolName::plain(name);
                tool_name_map.insert(name.to_string(), tool_name, AnthropicToolKind::Function)?;
                if let Some(tool) = function_tool_to_anthropic(tool, name) {
                    anthropic_tools.push(tool);
                }
            }
            Some("namespace") => {
                let Some(namespace) = tool.get("name").and_then(Value::as_str) else {
                    continue;
                };
                for tool in tool
                    .get("tools")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let tool_kind = match tool.get("type").and_then(Value::as_str) {
                        Some("function") => AnthropicToolKind::Function,
                        Some("custom") => AnthropicToolKind::Custom,
                        _ => {
                            return Err(ApiError::Stream(
                                "Anthropic Messages only supports function-compatible namespace tools"
                                    .to_string(),
                            ));
                        }
                    };
                    let Some(name) = tool.get("name").and_then(Value::as_str) else {
                        continue;
                    };
                    let anthropic_name = anthropic_tool_name(namespace, name);
                    let tool_name = if namespace == DEFAULT_FUNCTION_NAMESPACE {
                        ToolName::plain(name)
                    } else {
                        ToolName::namespaced(namespace, name)
                    };
                    tool_name_map.insert(anthropic_name.clone(), tool_name, tool_kind)?;
                    if let Some(tool) = function_tool_to_anthropic(tool, &anthropic_name) {
                        anthropic_tools.push(tool);
                    }
                }
            }
            Some("web_search") | Some("tool_search") => {
                return Err(ApiError::Stream(
                    "Anthropic Messages does not support hosted search tools".to_string(),
                ));
            }
            Some("custom") => {
                let Some(name) = tool.get("name").and_then(Value::as_str) else {
                    continue;
                };
                let tool_name = ToolName::plain(name);
                tool_name_map.insert(name.to_string(), tool_name, AnthropicToolKind::Custom)?;
                if let Some(tool) = function_tool_to_anthropic(tool, name) {
                    anthropic_tools.push(tool);
                }
            }
            _ => {}
        }
    }

    Ok((anthropic_tools, tool_name_map))
}

fn anthropic_tool_name(namespace: &str, name: &str) -> String {
    if namespace == DEFAULT_FUNCTION_NAMESPACE {
        name.to_string()
    } else {
        format!("{namespace}__{name}")
    }
}

fn function_tool_to_anthropic(tool: &Value, name: &str) -> Option<Value> {
    let description = tool
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let input_schema = match tool.get("type").and_then(Value::as_str) {
        Some("custom") => serde_json::json!({
            "type": "object",
            "properties": {
                "input": { "type": "string" },
            },
            "required": ["input"],
            "additionalProperties": false,
        }),
        _ => tool.get("parameters").cloned().unwrap_or_else(|| {
            serde_json::json!({
                "type": "object",
                "properties": {},
            })
        }),
    };
    Some(serde_json::json!({
        "name": name,
        "description": description,
        "input_schema": input_schema,
    }))
}

#[cfg(test)]
#[path = "anthropic_messages_tests.rs"]
mod tests;
