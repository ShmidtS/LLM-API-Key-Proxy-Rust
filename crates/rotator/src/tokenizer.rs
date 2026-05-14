use serde_json::Value;

const CHAT_MESSAGE_OVERHEAD: usize = 3;

pub fn count_tokens(text: &str, model: &str) -> usize {
    count_tokens_with_bpe(text, model).unwrap_or_else(|| approximate_tokens(text))
}

pub fn count_chat_tokens(messages: &[Value], model: &str) -> usize {
    messages
        .iter()
        .map(|message| {
            let content_tokens = message
                .get("content")
                .map(content_to_text)
                .map(|content| count_tokens(&content, model))
                .unwrap_or(0);

            content_tokens + CHAT_MESSAGE_OVERHEAD
        })
        .sum()
}

fn count_tokens_with_bpe(text: &str, model: &str) -> Option<usize> {
    if is_openai_model(model)
        && let Ok(bpe) = tiktoken_rs::get_bpe_from_model(model)
    {
        return Some(bpe.encode_with_special_tokens(text).len());
    }

    tiktoken_rs::cl100k_base()
        .map(|bpe| bpe.encode_with_special_tokens(text).len())
        .ok()
}

fn is_openai_model(model: &str) -> bool {
    let model = model
        .strip_prefix("openai/")
        .or_else(|| model.strip_prefix("azure/"))
        .unwrap_or(model);

    model.starts_with("gpt-4")
        || model.starts_with("gpt-4o")
        || model.starts_with("gpt-3.5-turbo")
        || model.starts_with("o1-")
        || model == "o1"
}

fn content_to_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn approximate_tokens(text: &str) -> usize {
    let words = text.split_whitespace().count();
    if words == 0 {
        text.chars().count().div_ceil(4)
    } else {
        (words * 4).div_ceil(3)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{count_chat_tokens, count_tokens};

    #[test]
    fn counts_openai_text_with_bpe() {
        assert_eq!(count_tokens("hello world", "gpt-4o"), 2);
    }

    #[test]
    fn counts_anthropic_text_with_cl100k_approximation() {
        assert_eq!(count_tokens("hello world", "claude-3-5-sonnet"), 2);
    }

    #[test]
    fn counts_chat_tokens_with_message_overhead() {
        let messages = [
            json!({"role": "system", "content": "You are helpful."}),
            json!({"role": "user", "content": "hello world"}),
        ];

        let content_tokens =
            count_tokens("You are helpful.", "gpt-4o") + count_tokens("hello world", "gpt-4o");

        assert_eq!(count_chat_tokens(&messages, "gpt-4o"), content_tokens + 6);
    }

    #[test]
    fn counts_non_string_content_as_json_text() {
        let messages = [json!({"role": "user", "content": [{"type": "text", "text": "hello"}]})];

        assert!(count_chat_tokens(&messages, "gpt-4o") > 3);
    }
}
