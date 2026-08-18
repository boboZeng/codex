use super::*;
use crate::common::Reasoning;
use crate::common::ResponsesApiTools;
use crate::common::TextControls;
use crate::common::TextFormat;
use crate::common::TextFormatType;
use codex_protocol::models::ResponseItem;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::sync::Arc;

fn request(input: Vec<ResponseItem>, tools: Option<ResponsesApiTools>) -> ResponsesApiRequest {
    ResponsesApiRequest {
        model: "anthropic/claude-sonnet-5".to_string(),
        instructions: "Base instructions".to_string(),
        input,
        tools,
        tool_choice: "auto".to_string(),
        parallel_tool_calls: true,
        reasoning: None,
        store: false,
        stream: true,
        stream_options: None,
        include: Vec::new(),
        service_tier: None,
        prompt_cache_key: None,
        text: None,
        client_metadata: None,
        access_programs: None,
    }
}

fn message(role: &str, text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: role.to_string(),
        content: vec![ContentItem::InputText {
            text: text.to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

#[test]
fn converts_messages_and_function_tools() {
    let tools = serde_json::value::to_raw_value(&json!([
        {
            "type": "function",
            "name": "shell",
            "description": "Run a shell command.",
            "parameters": { "type": "object", "properties": { "command": { "type": "string" } } }
        }
    ]))
    .map(Arc::from)
    .map(ResponsesApiTools::from)
    .expect("serialize tools");
    let body = anthropic_messages_request(
        request(
            vec![
                message("developer", "Provider-specific instructions"),
                message("user", "Inspect the repository"),
                message("assistant", ""),
                ResponseItem::FunctionCall {
                    id: None,
                    name: "shell".to_string(),
                    namespace: None,
                    arguments: r#"{"command":"pwd"}"#.to_string(),
                    encrypted_function_args: None,
                    call_id: "toolu_123".to_string(),
                    internal_chat_message_metadata_passthrough: None,
                },
            ],
            Some(tools),
        ),
        AnthropicPromptCaching::Disabled,
    )
    .expect("convert request")
    .body;

    assert_eq!(
        body,
        json!({
            "model": "anthropic/claude-sonnet-5",
            "max_tokens": 8192,
            "stream": true,
            "system": format!(
                "Base instructions\n\nProvider-specific instructions\n\n{ANTHROPIC_MESSAGES_SHELL_RELIABILITY_INSTRUCTIONS}"
            ),
            "messages": [
                { "role": "user", "content": [{ "type": "text", "text": "Inspect the repository" }] },
                { "role": "assistant", "content": [{ "type": "tool_use", "id": "toolu_123", "name": "shell", "input": { "command": "pwd" } }] }
            ],
            "tools": [{
                "name": "shell",
                "description": "Run a shell command.",
                "input_schema": { "type": "object", "properties": { "command": { "type": "string" } } }
            }],
            "tool_choice": { "type": "auto", "disable_parallel_tool_use": false }
        })
    );
}

#[test]
fn appends_user_continuation_after_assistant_prefill() {
    let body = anthropic_messages_request(
        request(
            vec![
                message("user", "Inspect the repository"),
                message("assistant", "Continue"),
            ],
            None,
        ),
        AnthropicPromptCaching::Disabled,
    )
    .expect("convert request")
    .body;

    assert_eq!(
        body["messages"],
        json!([
            {
                "role": "user",
                "content": [{ "type": "text", "text": "Inspect the repository" }]
            },
            {
                "role": "assistant",
                "content": [{ "type": "text", "text": "Continue" }]
            },
            {
                "role": "user",
                "content": [{ "type": "text", "text": "Continue." }]
            }
        ])
    );
}

#[test]
fn adds_prompt_cache_breakpoints_when_enabled() {
    let tools = serde_json::value::to_raw_value(&json!([
        {
            "type": "function",
            "name": "shell",
            "description": "Run a shell command.",
            "parameters": { "type": "object" }
        },
        {
            "type": "function",
            "name": "read_file",
            "description": "Read a file.",
            "parameters": { "type": "object" }
        }
    ]))
    .map(Arc::from)
    .map(ResponsesApiTools::from)
    .expect("serialize tools");

    let body = anthropic_messages_request(
        request(vec![message("user", "Inspect the repository")], Some(tools)),
        AnthropicPromptCaching::SystemAndTools,
    )
    .expect("convert request")
    .body;

    assert_eq!(
        body,
        json!({
            "model": "anthropic/claude-sonnet-5",
            "max_tokens": 8192,
            "stream": true,
            "system": [{
                "type": "text",
                "text": format!(
                    "Base instructions\n\n{ANTHROPIC_MESSAGES_SHELL_RELIABILITY_INSTRUCTIONS}"
                ),
                "cache_control": { "type": "ephemeral" }
            }],
            "messages": [
                { "role": "user", "content": [{ "type": "text", "text": "Inspect the repository" }] }
            ],
            "tools": [
                {
                    "name": "shell",
                    "description": "Run a shell command.",
                    "input_schema": { "type": "object" }
                },
                {
                    "name": "read_file",
                    "description": "Read a file.",
                    "input_schema": { "type": "object" },
                    "cache_control": { "type": "ephemeral" }
                }
            ],
            "tool_choice": { "type": "auto", "disable_parallel_tool_use": false }
        })
    );
}

#[test]
fn adds_rolling_history_cache_checkpoint_before_latest_message() {
    let body = anthropic_messages_request(
        request(
            vec![
                message("user", "First question"),
                message("assistant", "First answer"),
                message("user", "Second question"),
            ],
            None,
        ),
        AnthropicPromptCaching::RollingHistory,
    )
    .expect("convert request")
    .body;

    assert_eq!(
        body,
        json!({
            "model": "anthropic/claude-sonnet-5",
            "max_tokens": 8192,
            "stream": true,
            "system": [{
                "type": "text",
                "text": format!(
                    "Base instructions\n\n{ANTHROPIC_MESSAGES_SHELL_RELIABILITY_INSTRUCTIONS}"
                ),
                "cache_control": { "type": "ephemeral" }
            }],
            "messages": [
                { "role": "user", "content": [{ "type": "text", "text": "First question" }] },
                { "role": "assistant", "content": [{
                    "type": "text",
                    "text": "First answer",
                    "cache_control": { "type": "ephemeral" }
                }] },
                { "role": "user", "content": [{ "type": "text", "text": "Second question" }] }
            ]
        })
    );
}

#[test]
fn adds_shell_reliability_instructions_for_messages_models() {
    let first_body = anthropic_messages_request(
        request(vec![message("user", "Inspect the repository")], None),
        AnthropicPromptCaching::Disabled,
    )
    .expect("convert request")
    .body;
    assert_eq!(
        first_body["system"],
        format!("Base instructions\n\n{ANTHROPIC_MESSAGES_SHELL_RELIABILITY_INSTRUCTIONS}")
    );

    let mut second_request = request(vec![message("user", "Inspect the repository")], None);
    second_request.model = "custom/model".to_string();
    let second_body = anthropic_messages_request(second_request, AnthropicPromptCaching::Disabled)
        .expect("convert request")
        .body;
    assert_eq!(
        second_body["system"],
        format!("Base instructions\n\n{ANTHROPIC_MESSAGES_SHELL_RELIABILITY_INSTRUCTIONS}")
    );
}

#[test]
fn preserves_namespaced_tool_identity() {
    let tools = serde_json::value::to_raw_value(&json!([
        {
            "type": "namespace",
            "name": "mcp__figma",
            "description": "Figma tools",
            "tools": [{
                "type": "function",
                "name": "get_figma_data",
                "description": "Read Figma data.",
                "parameters": { "type": "object" }
            }]
        }
    ]))
    .map(Arc::from)
    .map(ResponsesApiTools::from)
    .expect("serialize tools");

    let request = anthropic_messages_request(
        request(
            vec![ResponseItem::FunctionCall {
                id: None,
                name: "get_figma_data".to_string(),
                namespace: Some("mcp__figma".to_string()),
                arguments: r#"{"fileKey":"design"}"#.to_string(),
                encrypted_function_args: None,
                call_id: "toolu_123".to_string(),
                internal_chat_message_metadata_passthrough: None,
            }],
            Some(tools),
        ),
        AnthropicPromptCaching::Disabled,
    )
    .expect("convert request");

    assert_eq!(
        request.body["tools"],
        json!([{
            "name": "mcp__figma__get_figma_data",
            "description": "Read Figma data.",
            "input_schema": { "type": "object" }
        }])
    );
    assert_eq!(
        request.body["messages"][0]["content"][0]["name"],
        "mcp__figma__get_figma_data"
    );
    assert_eq!(
        request.body["tool_choice"],
        json!({ "type": "auto", "disable_parallel_tool_use": false })
    );
    assert_eq!(
        request
            .tool_name_map
            .tool_name("mcp__figma__get_figma_data"),
        ToolName::namespaced("mcp__figma", "get_figma_data")
    );
}

#[test]
fn maps_tool_choice_and_parallel_tool_calls() {
    let tools = serde_json::value::to_raw_value(&json!([
        {
            "type": "function",
            "name": "shell",
            "description": "Run a shell command.",
            "parameters": { "type": "object" }
        }
    ]))
    .map(Arc::from)
    .map(ResponsesApiTools::from)
    .expect("serialize tools");
    let mut no_parallel_request = request(Vec::new(), Some(tools.clone()));
    no_parallel_request.parallel_tool_calls = false;
    let no_parallel_body =
        anthropic_messages_request(no_parallel_request, AnthropicPromptCaching::Disabled)
            .expect("convert request")
            .body;
    assert_eq!(
        no_parallel_body["tool_choice"],
        json!({ "type": "auto", "disable_parallel_tool_use": true })
    );

    let mut no_tools_request = request(Vec::new(), Some(tools));
    no_tools_request.tool_choice = "none".to_string();
    let no_tools_body =
        anthropic_messages_request(no_tools_request, AnthropicPromptCaching::Disabled)
            .expect("convert request")
            .body;
    assert!(no_tools_body.get("tools").is_none());
    assert!(no_tools_body.get("tool_choice").is_none());
}

#[test]
fn maps_reasoning_structured_output_and_service_tier() {
    let mut request = request(Vec::new(), None);
    request.reasoning = Some(Reasoning {
        effort: Some(codex_protocol::openai_models::ReasoningEffort::High),
        summary: None,
        context: None,
    });
    request.text = Some(TextControls {
        verbosity: None,
        format: Some(TextFormat {
            r#type: TextFormatType::JsonSchema,
            strict: true,
            schema: json!({"type": "object", "properties": {"ok": {"type": "boolean"}}}),
            name: "result".to_string(),
        }),
    });
    request.service_tier = Some("priority".to_string());

    let body = anthropic_messages_request(request, AnthropicPromptCaching::Disabled)
        .expect("convert request")
        .body;

    assert_eq!(body["max_tokens"], 9216);
    assert_eq!(
        body["thinking"],
        json!({"type": "enabled", "budget_tokens": 8192})
    );
    assert_eq!(
        body["output_config"],
        json!({
            "format": {
                "type": "json_schema",
                "schema": {"type": "object", "properties": {"ok": {"type": "boolean"}}}
            }
        })
    );
    assert_eq!(body["service_tier"], "priority");
}

#[test]
fn converts_responses_lite_additional_tools() {
    let mut request = request(
        vec![ResponseItem::AdditionalTools {
            id: None,
            role: "developer".to_string(),
            tools: vec![json!({
                "type": "namespace",
                "name": "functions",
                "tools": [{
                    "type": "function",
                    "name": "shell",
                    "description": "Run a command.",
                    "parameters": {"type": "object"}
                }]
            })],
        }],
        None,
    );
    request.input.push(ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "run it".to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    });

    let body = anthropic_messages_request(request, AnthropicPromptCaching::Disabled)
        .expect("convert request")
        .body;
    assert_eq!(
        body["tools"],
        json!([{
            "name": "shell",
            "description": "Run a command.",
            "input_schema": {"type": "object"}
        }])
    );
}

#[test]
fn converts_custom_tools_to_function_compatible_tools() {
    let tools = serde_json::value::to_raw_value(&json!([
        {
            "type": "custom",
            "name": "custom_echo",
            "description": "Echo a custom payload.",
            "format": {
                "type": "grammar",
                "syntax": "lark",
                "definition": "start: /.+/"
            }
        }
    ]))
    .map(Arc::from)
    .map(ResponsesApiTools::from)
    .expect("serialize tools");

    let body = anthropic_messages_request(
        request(Vec::new(), Some(tools)),
        AnthropicPromptCaching::Disabled,
    )
    .expect("convert request")
    .body;

    assert_eq!(
        body["tools"],
        json!([{
            "name": "custom_echo",
            "description": "Echo a custom payload.",
            "input_schema": {
                "type": "object",
                "properties": { "input": { "type": "string" } },
                "required": ["input"],
                "additionalProperties": false
            }
        }])
    );
}
