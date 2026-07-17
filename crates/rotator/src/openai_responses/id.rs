use std::time::{SystemTime, UNIX_EPOCH};

pub trait Clock: Send + Sync {
    fn unix_seconds(&self) -> u64;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn unix_seconds(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ResponseIdFactory {
    prefix: &'static str,
}

impl Default for ResponseIdFactory {
    fn default() -> Self {
        Self { prefix: "resp" }
    }
}

impl ResponseIdFactory {
    pub fn new(prefix: &'static str) -> Self {
        Self { prefix }
    }

    pub fn response_id_from_chat_id(&self, chat_id: &str) -> String {
        let expected = format!("{}_", self.prefix);
        if chat_id.starts_with(&expected) {
            chat_id.to_owned()
        } else {
            format!("{}_{chat_id}", self.prefix)
        }
    }

    pub fn message_id(&self, response_id: &str) -> String {
        format!("msg_{response_id}")
    }

    pub fn function_call_id(&self, tool_call_id: &str) -> String {
        Self::ensure_fc_prefix(tool_call_id)
    }

    /// Ensure a tool-call id meets OpenAI's Responses-API requirement: the `id`
    /// field of a `function_call` input/output item must begin with `fc_`.
    /// Clients (e.g. Roo Code) send ids like `search_files_101` — prefix them.
    pub fn ensure_fc_prefix(tool_call_id: &str) -> String {
        if tool_call_id.starts_with("fc_") {
            tool_call_id.to_owned()
        } else {
            format!("fc_{tool_call_id}")
        }
    }
}
