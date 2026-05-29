use rotator::{SanitizerContext, sanitize_request};

#[test]
fn strips_provider_prefix_before_upstream() {
    let mut body = serde_json::json!({"model": "openai/gpt-5.1"});
    sanitize_request(
        &SanitizerContext {
            provider_id: "openai".to_owned(),
            model: "openai/gpt-5.1".to_owned(),
            endpoint: "chat/completions".to_owned(),
        },
        &mut body,
    );

    assert_eq!(body["model"], "gpt-5.1");
}

#[test]
fn openai_reasoning_models_remove_unsupported_temperature() {
    for model in ["gpt-5.1", "o1-mini", "o3-mini", "o4-mini"] {
        let mut body = serde_json::json!({"model": model, "temperature": 0.7});
        sanitize_request(
            &SanitizerContext {
                provider_id: "openai".to_owned(),
                model: model.to_owned(),
                endpoint: "chat/completions".to_owned(),
            },
            &mut body,
        );
        assert!(body.get("temperature").is_none(), "{model}");
    }
}

#[test]
fn anthropic_removes_openai_stream_options() {
    let mut body = serde_json::json!({
        "model": "anthropic/claude-sonnet-4",
        "stream_options": {"include_usage": true}
    });
    sanitize_request(
        &SanitizerContext {
            provider_id: "anthropic".to_owned(),
            model: "anthropic/claude-sonnet-4".to_owned(),
            endpoint: "messages".to_owned(),
        },
        &mut body,
    );

    assert_eq!(body["model"], "claude-sonnet-4");
    assert!(body.get("stream_options").is_none());
}

#[test]
fn gemini_removes_openai_only_fields() {
    let mut body = serde_json::json!({
        "model": "gemini/gemini-2.5-flash",
        "stream_options": {"include_usage": true},
        "logprobs": true,
        "top_logprobs": 3
    });
    sanitize_request(
        &SanitizerContext {
            provider_id: "gemini".to_owned(),
            model: "gemini/gemini-2.5-flash".to_owned(),
            endpoint: "chat/completions".to_owned(),
        },
        &mut body,
    );

    assert_eq!(body["model"], "gemini-2.5-flash");
    assert!(body.get("stream_options").is_none());
    assert!(body.get("logprobs").is_none());
    assert!(body.get("top_logprobs").is_none());
}
