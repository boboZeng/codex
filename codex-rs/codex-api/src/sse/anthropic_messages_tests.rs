use super::*;
use bytes::Bytes;
use codex_client::TransportError;
use futures::stream;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn translates_tool_use_stream_events() {
    let events = [
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_123\",\"usage\":{\"input_tokens\":10,\"cache_read_input_tokens\":2,\"cache_creation_input_tokens\":4}}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_123\",\"name\":\"shell\",\"input\":{}}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"command\\\":\\\"pwd\\\"}\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"usage\":{\"output_tokens\":3},\"delta\":{\"type\":\"text_delta\",\"stop_reason\":\"tool_use\"}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    ];
    let stream = stream::iter(
        events
            .into_iter()
            .map(|event| Ok::<_, TransportError>(Bytes::copy_from_slice(event.as_bytes()))),
    );
    let (tx, mut rx) = mpsc::channel(16);
    tokio::spawn(process_sse(
        Box::pin(stream),
        tx,
        Duration::from_secs(1),
        None,
        AnthropicToolNameMap::default(),
    ));

    let mut actual = Vec::new();
    while let Some(event) = rx.recv().await {
        actual.push(event.expect("valid stream event"));
    }

    assert_eq!(actual.len(), 5);
    assert!(matches!(actual[0], ResponseEvent::Created { .. }));
    assert!(matches!(actual[1], ResponseEvent::OutputItemAdded(_)));
    assert!(matches!(
        actual[2],
        ResponseEvent::ToolCallInputDelta { .. }
    ));
    assert!(matches!(
        &actual[3],
        ResponseEvent::OutputItemDone(ResponseItem::FunctionCall { arguments, .. })
            if arguments == r#"{"command":"pwd"}"#
    ));
    assert!(matches!(
        actual[4],
        ResponseEvent::Completed {
            ref response_id,
            token_usage: Some(TokenUsage {
                input_tokens: 10,
                cached_input_tokens: 2,
                cache_write_input_tokens: 4,
                output_tokens: 3,
                total_tokens: 13,
                ..
            }),
            ..
        } if response_id == "msg_123"
    ));
}

#[tokio::test]
async fn restores_namespaced_tool_identity_from_anthropic_name() {
    let events = [
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_123\",\"usage\":{}}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_123\",\"name\":\"mcp__figma__get_figma_data\",\"input\":{\"fileKey\":\"design\"}}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    ];
    let stream = stream::iter(
        events
            .into_iter()
            .map(|event| Ok::<_, TransportError>(Bytes::copy_from_slice(event.as_bytes()))),
    );
    let mut tool_name_map = AnthropicToolNameMap::default();
    tool_name_map
        .insert(
            "mcp__figma__get_figma_data".to_string(),
            ToolName::namespaced("mcp__figma", "get_figma_data"),
            AnthropicToolKind::Function,
        )
        .expect("insert tool name mapping");
    let (tx, mut rx) = mpsc::channel(16);
    tokio::spawn(process_sse(
        Box::pin(stream),
        tx,
        Duration::from_secs(1),
        None,
        tool_name_map,
    ));

    let mut actual = Vec::new();
    while let Some(event) = rx.recv().await {
        actual.push(event.expect("valid stream event"));
    }

    assert!(matches!(
        &actual[2],
        ResponseEvent::OutputItemDone(ResponseItem::FunctionCall {
            name,
            namespace,
            arguments,
            ..
        }) if name == "get_figma_data"
            && namespace.as_deref() == Some("mcp__figma")
            && arguments == r#"{"fileKey":"design"}"#
    ));
}

#[tokio::test]
async fn preserves_text_in_the_completed_message() {
    let events = [
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_123\",\"usage\":{}}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"你好\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    ];
    let stream = stream::iter(
        events
            .into_iter()
            .map(|event| Ok::<_, TransportError>(Bytes::copy_from_slice(event.as_bytes()))),
    );
    let (tx, mut rx) = mpsc::channel(16);
    tokio::spawn(process_sse(
        Box::pin(stream),
        tx,
        Duration::from_secs(1),
        None,
        AnthropicToolNameMap::default(),
    ));

    let mut actual = Vec::new();
    while let Some(event) = rx.recv().await {
        actual.push(event.expect("valid stream event"));
    }

    assert_eq!(actual.len(), 5);
    let added_id = match &actual[1] {
        ResponseEvent::OutputItemAdded(ResponseItem::Message { id, .. }) => id,
        _ => panic!("expected an added assistant message"),
    };
    assert!(matches!(
        &actual[2],
        ResponseEvent::OutputTextDelta(text) if text == "你好"
    ));
    match &actual[3] {
        ResponseEvent::OutputItemDone(ResponseItem::Message {
            id,
            role,
            content,
            phase,
            internal_chat_message_metadata_passthrough,
        }) => {
            assert_eq!(id, added_id);
            assert_eq!(role, "assistant");
            assert_eq!(
                content,
                &[ContentItem::OutputText {
                    text: "你好".to_string(),
                }]
            );
            assert_eq!(phase, &None);
            assert_eq!(internal_chat_message_metadata_passthrough, &None);
        }
        _ => panic!("expected a completed assistant message"),
    }
    assert!(matches!(actual[4], ResponseEvent::Completed { .. }));
}

#[tokio::test]
async fn translates_function_compatible_custom_tool_events() {
    let events = [
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_123\",\"usage\":{}}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_123\",\"name\":\"custom_echo\",\"input\":{}}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"input\\\":\\\"hello\\\"}\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    ];
    let stream = stream::iter(
        events
            .into_iter()
            .map(|event| Ok::<_, TransportError>(Bytes::copy_from_slice(event.as_bytes()))),
    );
    let mut tool_name_map = AnthropicToolNameMap::default();
    tool_name_map
        .insert(
            "custom_echo".to_string(),
            ToolName::plain("custom_echo"),
            AnthropicToolKind::Custom,
        )
        .expect("insert custom tool name mapping");
    let (tx, mut rx) = mpsc::channel(16);
    tokio::spawn(process_sse(
        Box::pin(stream),
        tx,
        Duration::from_secs(1),
        None,
        tool_name_map,
    ));

    let mut actual = Vec::new();
    while let Some(event) = rx.recv().await {
        actual.push(event.expect("valid stream event"));
    }

    assert!(matches!(
        &actual[1],
        ResponseEvent::OutputItemAdded(ResponseItem::CustomToolCall { input, .. })
            if input.is_empty()
    ));
    assert!(matches!(
        &actual[3],
        ResponseEvent::OutputItemDone(ResponseItem::CustomToolCall { input, .. })
            if input == "hello"
    ));
}

#[tokio::test]
async fn translates_thinking_blocks_to_reasoning_events() {
    let events = [
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_123\",\"usage\":{}}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"先检查工具\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    ];
    let stream = stream::iter(
        events
            .into_iter()
            .map(|event| Ok::<_, TransportError>(Bytes::copy_from_slice(event.as_bytes()))),
    );
    let (tx, mut rx) = mpsc::channel(16);
    tokio::spawn(process_sse(
        Box::pin(stream),
        tx,
        Duration::from_secs(1),
        None,
        AnthropicToolNameMap::default(),
    ));

    let mut actual = Vec::new();
    while let Some(event) = rx.recv().await {
        actual.push(event.expect("valid stream event"));
    }

    assert!(matches!(
        actual[1],
        ResponseEvent::OutputItemAdded(ResponseItem::Reasoning { .. })
    ));
    assert!(matches!(
        &actual[2],
        ResponseEvent::ReasoningContentDelta { delta, content_index: 0 } if delta == "先检查工具"
    ));
    assert!(matches!(
        &actual[3],
        ResponseEvent::OutputItemDone(ResponseItem::Reasoning { content: Some(content), .. })
            if content.iter().any(|item| matches!(item, ReasoningItemContent::ReasoningText { text } if text == "先检查工具"))
    ));
    assert!(matches!(actual[4], ResponseEvent::Completed { .. }));
}

async fn completed_end_turn_for_stop_reason(stop_reason: Option<&str>) -> Option<bool> {
    let mut events = vec![
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_123\",\"usage\":{}}}\n\n"
            .to_string(),
    ];
    if let Some(stop_reason) = stop_reason {
        events.push(format!(
            "event: message_delta\ndata: {{\"type\":\"message_delta\",\"delta\":{{\"type\":\"message_delta\",\"stop_reason\":\"{stop_reason}\"}}}}\n\n"
        ));
    }
    events.push("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n".to_string());

    let stream = stream::iter(
        events
            .into_iter()
            .map(|event| Ok::<_, TransportError>(Bytes::copy_from_slice(event.as_bytes()))),
    );
    let (tx, mut rx) = mpsc::channel(16);
    tokio::spawn(process_sse(
        Box::pin(stream),
        tx,
        Duration::from_secs(1),
        None,
        AnthropicToolNameMap::default(),
    ));

    while let Some(event) = rx.recv().await {
        if let ResponseEvent::Completed { end_turn, .. } = event.expect("valid stream event") {
            return end_turn;
        }
    }
    panic!("expected a completed event");
}

#[tokio::test]
async fn only_explicit_tool_use_requests_anthropic_follow_up() {
    assert_eq!(
        completed_end_turn_for_stop_reason(Some("end_turn")).await,
        Some(true)
    );
    assert_eq!(
        completed_end_turn_for_stop_reason(Some("tool_use")).await,
        Some(false)
    );
    assert_eq!(
        completed_end_turn_for_stop_reason(Some("max_tokens")).await,
        None
    );
    assert_eq!(
        completed_end_turn_for_stop_reason(Some("stop_sequence")).await,
        None
    );
    assert_eq!(
        completed_end_turn_for_stop_reason(Some("pause_turn")).await,
        None
    );
    assert_eq!(
        completed_end_turn_for_stop_reason(Some("unknown_reason")).await,
        None
    );
    assert_eq!(completed_end_turn_for_stop_reason(None).await, None);
}
