//! SWAI — Council streaming module SSE-formatting unit tests.
//!
//! These cover the `CouncilStreamEvent` SSE serialization helpers in
//! `streaming.rs`. Live broadcast-order tests live in `tests_streaming.rs`.

use crate::council::streaming::*;
use serde_json;

#[test]
fn test_format_sse_event_status() {
    let event = CouncilStreamEvent::Status {
        stage: "generator".into(),
        model_id: "llama3-8b".into(),
        elapsed_secs: 1.5,
    };
    let sse = format_sse_event(&event, 1);

    assert!(sse.contains("event: council_status"));
    let data: serde_json::Value = serde_json::from_str(
        sse.lines()
            .find(|l| l.starts_with("data:"))
            .unwrap()
            .trim_start_matches("data: ")
            .trim(),
    )
    .unwrap();
    assert_eq!(data["sequence"], 1);
    assert_eq!(data["stage"], "generator");
    assert_eq!(data["model_id"], "llama3-8b");
    assert!((data["elapsed_secs"].as_f64().unwrap() - 1.5).abs() < 1e-6);
    // Double newline at end of SSE event.
    assert!(sse.ends_with("\n\n"));
}

#[test]
fn test_format_sse_event_draft() {
    let event = CouncilStreamEvent::Draft {
        draft_text: "This is the initial draft.".into(),
        model_id: "mistral-7b".into(),
    };
    let sse = format_sse_event(&event, 2);

    assert!(sse.contains("event: council_draft"));
    let data: serde_json::Value = serde_json::from_str(
        sse.lines()
            .find(|l| l.starts_with("data:"))
            .unwrap()
            .trim_start_matches("data: ")
            .trim(),
    )
    .unwrap();
    assert_eq!(data["sequence"], 2);
    assert_eq!(data["model_id"], "mistral-7b");
    assert_eq!(data["draft"], "This is the initial draft.");
}

#[test]
fn test_format_sse_event_critique() {
    let event = CouncilStreamEvent::Critique {
        auditor_index: 0,
        critique: "The draft needs more detail.".into(),
        model_id: "claude-3-haiku".into(),
    };
    let sse = format_sse_event(&event, 3);

    assert!(sse.contains("event: council_critique"));
    let data: serde_json::Value = serde_json::from_str(
        sse.lines()
            .find(|l| l.starts_with("data:"))
            .unwrap()
            .trim_start_matches("data: ")
            .trim(),
    )
    .unwrap();
    assert_eq!(data["sequence"], 3);
    assert_eq!(data["auditor_index"], 0);
    assert_eq!(data["model_id"], "claude-3-haiku");
    assert_eq!(data["critique"], "The draft needs more detail.");
}

#[test]
fn test_format_sse_event_chunk() {
    let event = CouncilStreamEvent::Chunk {
        text: "Hello, world!".into(),
    };
    let sse = format_sse_event(&event, 4);

    assert!(sse.contains("event: council_chunk"));
    let data: serde_json::Value = serde_json::from_str(
        sse.lines()
            .find(|l| l.starts_with("data:"))
            .unwrap()
            .trim_start_matches("data: ")
            .trim(),
    )
    .unwrap();
    assert_eq!(data["sequence"], 4);
    assert_eq!(data["text"], "Hello, world!");
}

#[test]
fn test_format_sse_event_done() {
    let event = CouncilStreamEvent::Done;
    let sse = format_sse_event(&event, 5);

    assert!(sse.contains("event: council_done"));
    let data: serde_json::Value = serde_json::from_str(
        sse.lines()
            .find(|l| l.starts_with("data:"))
            .unwrap()
            .trim_start_matches("data: ")
            .trim(),
    )
    .unwrap();
    assert_eq!(data["sequence"], 5);
    // Done event has no extra fields beyond sequence and timestamp.
    assert!(data.get("timestamp").is_some());
}

#[test]
fn test_encode_stream_events_multiple() {
    let events = vec![
        CouncilStreamEvent::Status {
            stage: "generator".into(),
            model_id: "llama3-8b".into(),
            elapsed_secs: 0.5,
        },
        CouncilStreamEvent::Draft {
            draft_text: "Draft content.".into(),
            model_id: "llama3-8b".into(),
        },
        CouncilStreamEvent::Done,
    ];

    let encoded = encode_stream_events(&events);

    // Should contain all three events.
    assert!(encoded.contains("event: council_status"));
    assert!(encoded.contains("event: council_draft"));
    assert!(encoded.contains("event: council_done"));

    // Each event should be separated by double newline.
    let event_count = encoded.matches("\n\n").count();
    assert_eq!(event_count, 3);
}

#[test]
fn test_encode_stream_events_empty() {
    let events: Vec<CouncilStreamEvent> = vec![];
    let encoded = encode_stream_events(&events);
    assert!(encoded.is_empty());
}

#[test]
fn test_sse_event_sequence_increments() {
    let event = CouncilStreamEvent::Chunk {
        text: "test".into(),
    };
    let sse1 = format_sse_event(&event, 10);
    let sse2 = format_sse_event(&event, 20);

    let data1: serde_json::Value = serde_json::from_str(
        sse1.lines()
            .find(|l| l.starts_with("data:"))
            .unwrap()
            .trim_start_matches("data: ")
            .trim(),
    )
    .unwrap();
    let data2: serde_json::Value = serde_json::from_str(
        sse2.lines()
            .find(|l| l.starts_with("data:"))
            .unwrap()
            .trim_start_matches("data: ")
            .trim(),
    )
    .unwrap();

    assert_eq!(data1["sequence"], 10);
    assert_eq!(data2["sequence"], 20);
}

#[test]
fn test_sse_event_timestamp_is_rfc3339() {
    let event = CouncilStreamEvent::Status {
        stage: "synthesizer".into(),
        model_id: "gpt-4".into(),
        elapsed_secs: 2.0,
    };
    let sse = format_sse_event(&event, 1);
    let data: serde_json::Value = serde_json::from_str(
        sse.lines()
            .find(|l| l.starts_with("data:"))
            .unwrap()
            .trim_start_matches("data: ")
            .trim(),
    )
    .unwrap();

    let timestamp = data["timestamp"].as_str().unwrap();
    // RFC 3339 timestamps contain 'T' separator and end with timezone offset.
    assert!(timestamp.contains('T'));
    assert!(timestamp.ends_with("+00:00") || timestamp.ends_with("Z"));
}

#[test]
fn test_council_stream_event_serialization_roundtrip() {
    let event = CouncilStreamEvent::Critique {
        auditor_index: 2,
        critique: "Critical feedback.".into(),
        model_id: "custom-model".into(),
    };

    let json = serde_json::to_string(&event).unwrap();
    let back: CouncilStreamEvent = serde_json::from_str(&json).unwrap();
    assert_eq!(back, event);
}
