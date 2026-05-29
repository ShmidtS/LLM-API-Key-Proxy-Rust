use proxy_app::compat::anthropic::{anthropic_to_openai_response, translate_anthropic_request};
use serde_json::{Value, json};

mod anthropic_compat {
    use super::*;

    #[test]
    fn translates_openai_request_to_anthropic_json_shape() {
        let input = json!({
            "model": "claude-3-5-sonnet-latest",
            "messages": [
                {"role": "system", "content": "Be concise"},
                {"role": "user", "content": [
                    {"type": "text", "text": "Describe this"},
                    {"type": "image_url", "image_url": {"url": "data:image/png;base64,abc123"}}
                ]},
                {"role": "assistant", "content": "A short answer"}
            ],
            "max_completion_tokens": 128,
            "temperature": 0.2,
            "top_p": 0.9,
            "stream": false,
            "stop": ["END"]
        });

        let output = translate_anthropic_request(&input);

        assert_eq!(output["model"], "claude-3-5-sonnet-latest");
        assert_eq!(output["max_tokens"], 128);
        assert_eq!(output["temperature"], 0.2);
        assert_eq!(output["top_p"], 0.9);
        assert_eq!(output["stream"], false);
        assert_eq!(output["stop_sequences"], json!(["END"]));
        assert_eq!(output["system"], "Be concise");
        assert_eq!(output["messages"].as_array().unwrap().len(), 2);
        assert_eq!(output["messages"][0]["role"], "user");
        assert_eq!(
            output["messages"][0]["content"][0],
            json!({"type": "text", "text": "Describe this"})
        );
        assert_eq!(output["messages"][0]["content"][1]["type"], "image");
        assert_eq!(
            output["messages"][0]["content"][1]["source"]["media_type"],
            "image/png"
        );
        assert_eq!(
            output["messages"][0]["content"][1]["source"]["data"],
            "abc123"
        );
        assert_eq!(output["messages"][1]["role"], "assistant");
        assert_eq!(output["messages"][1]["content"], "A short answer");
    }

    #[test]
    fn translates_anthropic_response_to_openai_json_shape() {
        let input = json!({
            "id": "msg_123",
            "type": "message",
            "role": "assistant",
            "content": [
                {"type": "text", "text": "Hello "},
                {"type": "text", "text": "there"}
            ],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 9, "output_tokens": 4}
        });

        let output = anthropic_to_openai_response(&input, "claude-test");

        assert_eq!(output["id"], "msg_123");
        assert_eq!(output["object"], "chat.completion");
        assert!(output["created"].as_u64().is_some());
        assert_eq!(output["model"], "claude-test");
        assert_eq!(output["choices"].as_array().unwrap().len(), 1);
        assert_eq!(output["choices"][0]["index"], 0);
        assert_eq!(output["choices"][0]["message"]["role"], "assistant");
        assert_eq!(output["choices"][0]["message"]["content"], "Hello there");
        assert_eq!(output["choices"][0]["finish_reason"], "stop");
        assert_eq!(
            output["usage"],
            json!({
                "prompt_tokens": 9,
                "completion_tokens": 4,
                "total_tokens": 13
            })
        );
    }

    #[test]
    fn handles_system_string_and_list_messages() {
        let string_input = json!({
            "model": "claude-test",
            "messages": [
                {"role": "system", "content": "First"},
                {"role": "system", "content": "Second"},
                {"role": "user", "content": "Hi"}
            ]
        });
        let list_input = json!({
            "model": "claude-test",
            "messages": [
                {"role": "system", "content": [
                    {"type": "text", "text": "A"},
                    "B",
                    {"type": "custom", "value": true}
                ]},
                {"role": "user", "content": "Hi"}
            ]
        });

        let string_output = translate_anthropic_request(&string_input);
        let list_output = translate_anthropic_request(&list_input);

        assert_eq!(string_output["system"], "First\nSecond");
        assert_eq!(string_output["messages"][0]["role"], "user");
        assert_eq!(
            list_output["system"],
            json!([
                {"type": "text", "text": "A"},
                {"type": "text", "text": "B"},
                {"type": "custom", "value": true}
            ])
        );
    }

    #[test]
    fn converts_tool_use_and_tool_result_between_shapes() {
        let request = json!({
            "model": "claude-test",
            "messages": [{
                "role": "assistant",
                "content": "Checking",
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "lookup", "arguments": "{\"city\":\"Paris\"}"}
                }]
            }, {
                "role": "tool",
                "tool_call_id": "call_1",
                "content": [{"type": "text", "text": "sunny"}]
            }],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "lookup",
                    "description": "Lookup weather",
                    "parameters": {"type": "object", "properties": {"city": {"type": "string"}}}
                }
            }],
            "tool_choice": {"type": "function", "function": {"name": "lookup"}}
        });

        let translated_request = translate_anthropic_request(&request);

        assert_eq!(translated_request["tools"][0]["name"], "lookup");
        assert_eq!(
            translated_request["tools"][0]["description"],
            "Lookup weather"
        );
        assert_eq!(
            translated_request["tools"][0]["input_schema"]["type"],
            "object"
        );
        assert_eq!(
            translated_request["tool_choice"],
            json!({"type": "tool", "name": "lookup"})
        );
        assert_eq!(
            translated_request["messages"][0]["content"][0],
            json!({"type": "text", "text": "Checking"})
        );
        assert_eq!(
            translated_request["messages"][0]["content"][1],
            json!({
                "type": "tool_use",
                "id": "call_1",
                "name": "lookup",
                "input": {"city": "Paris"}
            })
        );
        assert_eq!(translated_request["messages"][1]["role"], "user");
        assert_eq!(
            translated_request["messages"][1]["content"][0]["type"],
            "tool_result"
        );
        assert_eq!(
            translated_request["messages"][1]["content"][0]["tool_use_id"],
            "call_1"
        );

        let response = json!({
            "id": "msg_tool",
            "content": [{"type": "tool_use", "id": "toolu_1", "name": "lookup", "input": {"city": "Paris"}}],
            "stop_reason": "tool_use"
        });
        let translated_response = anthropic_to_openai_response(&response, "claude-test");

        assert_eq!(
            translated_response["choices"][0]["finish_reason"],
            "tool_calls"
        );
        assert_eq!(translated_response["choices"][0]["message"]["content"], "");
        assert_eq!(
            translated_response["choices"][0]["message"]["tool_calls"][0],
            json!({
                "id": "toolu_1",
                "type": "function",
                "function": {"name": "lookup", "arguments": "{\"city\":\"Paris\"}"}
            })
        );
    }

    #[test]
    fn maps_stop_reasons_to_openai_finish_reasons() {
        for (stop_reason, expected) in [
            ("end_turn", "stop"),
            ("end_sequence", "stop"),
            ("stop_sequence", "stop"),
            ("max_tokens", "length"),
            ("tool_use", "tool_calls"),
        ] {
            let input = json!({"content": [], "stop_reason": stop_reason});
            let output = anthropic_to_openai_response(&input, "claude-test");

            assert_eq!(output["choices"][0]["finish_reason"], expected);
        }
    }

    #[test]
    fn maps_usage_tokens_to_openai_usage_fields() {
        let input = json!({
            "content": [],
            "usage": {"input_tokens": 31, "output_tokens": 17}
        });

        let output = anthropic_to_openai_response(&input, "claude-test");

        assert_eq!(output["usage"]["prompt_tokens"], 31);
        assert_eq!(output["usage"]["completion_tokens"], 17);
        assert_eq!(output["usage"]["total_tokens"], 48);
    }

    #[test]
    fn handles_thinking_config_and_reasoning_content() {
        let request = json!({
            "model": "claude-test",
            "messages": [{"role": "user", "content": "think"}],
            "thinking": {"type": "enabled", "budget_tokens": 4096}
        });
        let response = json!({
            "content": [
                {"type": "thinking", "thinking": "internal ", "signature": "sig_1"},
                {"type": "redacted_thinking", "data": "encrypted"},
                {"type": "text", "text": "answer"}
            ]
        });

        let translated_request = translate_anthropic_request(&request);
        let translated_response = anthropic_to_openai_response(&response, "claude-test");

        assert_eq!(
            translated_request["thinking"],
            json!({"type": "enabled", "budget_tokens": 4096})
        );
        assert_eq!(
            translated_response["choices"][0]["message"]["content"],
            "answer"
        );
        assert_eq!(
            translated_response["choices"][0]["message"]["reasoning_content"],
            "internal "
        );
        assert_eq!(
            translated_response["choices"][0]["message"]["reasoning_details"][0]["type"],
            "thinking"
        );
        assert_eq!(
            translated_response["choices"][0]["message"]["reasoning_details"][1]["type"],
            "redacted_thinking"
        );
    }

    #[test]
    fn reorders_assistant_content_for_anthropic_requirements() {
        let input = json!({
            "model": "claude-test",
            "messages": [{
                "role": "assistant",
                "content": [
                    {"type": "tool_use", "id": "call_1", "name": "lookup", "input": {}},
                    {"type": "text", "text": "answer"},
                    {"type": "thinking", "thinking": "reason", "signature": "sig", "cache_control": {"type": "ephemeral"}},
                    {"type": "text", "text": "   "}
                ]
            }]
        });

        let output = translate_anthropic_request(&input);
        let content = output["messages"][0]["content"].as_array().unwrap();

        assert_eq!(content.len(), 3);
        assert_eq!(
            content[0],
            json!({"type": "thinking", "thinking": "reason", "signature": "sig", "cache_control": {"type": "ephemeral"}})
        );
        assert_eq!(content[1], json!({"type": "text", "text": "answer"}));
        assert_eq!(content[2]["type"], "tool_use");
    }

    #[test]
    fn handles_malformed_anthropic_response_without_panicking() {
        let cases = [
            Value::Null,
            json!({}),
            json!({"content": {"unexpected": true}, "usage": {"input_tokens": "bad"}}),
            json!({"content": [{"type": "tool_use", "input": null}], "stop_reason": null}),
        ];

        for input in cases {
            let output = anthropic_to_openai_response(&input, "claude-test");

            assert_eq!(output["object"], "chat.completion");
            assert_eq!(output["choices"][0]["message"]["role"], "assistant");
            assert!(output["choices"][0]["message"]["content"].is_string());
            assert_eq!(output["usage"]["prompt_tokens"], 0);
            assert_eq!(output["usage"]["completion_tokens"], 0);
            assert_eq!(output["usage"]["total_tokens"], 0);
        }
    }

    #[test]
    fn preserves_extra_anthropic_response_fields() {
        let input = json!({
            "id": "msg_extra",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "Hello"}],
            "model": "provider-model",
            "stop_reason": "end_turn",
            "stop_sequence": null,
            "usage": {"input_tokens": 1, "output_tokens": 2},
            "container": {"id": "container_123"},
            "service_tier": "standard_only",
            "metadata": {"trace_id": "trace_1"}
        });

        let output = anthropic_to_openai_response(&input, "claude-test");

        assert_eq!(output["container"], json!({"id": "container_123"}));
        assert_eq!(output["service_tier"], "standard_only");
        assert_eq!(output["metadata"], json!({"trace_id": "trace_1"}));
        assert_eq!(output.get("type"), None);
        assert_eq!(output.get("role"), None);
        assert_eq!(output.get("stop_sequence"), None);
        assert_eq!(output["model"], "claude-test");
    }
}
