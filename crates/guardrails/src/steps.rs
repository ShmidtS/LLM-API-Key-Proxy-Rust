use crate::error::GuardrailError;
use crate::prerequisites::StepTracker;
use crate::types::{GuardrailRequest, GuardrailResponse, StepPolicy, ValidationIssue};
use serde_json::Value;

pub trait StepEnforcer: Send + Sync {
    fn before_request(
        &self,
        request: GuardrailRequest,
        policy: &StepPolicy,
    ) -> Result<GuardrailRequest, GuardrailError>;

    fn after_response(
        &self,
        response: &GuardrailResponse,
        policy: &StepPolicy,
    ) -> Result<Vec<ValidationIssue>, GuardrailError>;
}

#[derive(Debug, Clone, Default)]
pub struct DefaultStepEnforcer;

impl StepEnforcer for DefaultStepEnforcer {
    fn before_request(
        &self,
        mut request: GuardrailRequest,
        policy: &StepPolicy,
    ) -> Result<GuardrailRequest, GuardrailError> {
        if request.step_policy.is_none() {
            request.step_policy = Some(policy.clone());
        }
        Ok(request)
    }

    fn after_response(
        &self,
        response: &GuardrailResponse,
        policy: &StepPolicy,
    ) -> Result<Vec<ValidationIssue>, GuardrailError> {
        let text = response.body.to_string().to_lowercase();
        let issues = policy
            .required_steps
            .iter()
            .filter(|step| !text.contains(&step.to_lowercase()))
            .map(|step| ValidationIssue {
                field: "steps".to_owned(),
                reason: format!("required step `{step}` was not completed"),
                severity: "error".to_owned(),
            })
            .collect();
        Ok(issues)
    }
}

/// Проверяет, вызван ли terminal-tool до завершения required_steps.
/// Если да — возвращает сообщение об ошибке [StepEnforcementError] для tool-канала.
pub fn check_premature_terminal(
    response: &GuardrailResponse,
    policy: &StepPolicy,
    tracker: &StepTracker,
) -> Option<String> {
    if tracker.is_satisfied() || policy.terminal_tools.is_empty() {
        return None;
    }
    let tool_names = extract_tool_names_from_response(response);
    for name in tool_names {
        if policy.terminal_tools.contains(&name) {
            let pending = tracker.pending();
            if !pending.is_empty() {
                return Some(step_enforcement_error_message(&name, &pending));
            }
        }
    }
    None
}

/// Формирует tool-канальное сообщение об ошибке step enforcement
/// (паритет с Forge `[StepEnforcementError]`).
pub fn step_enforcement_error_message(terminal_tool: &str, pending: &[String]) -> String {
    format!(
        "[StepEnforcementError] Tool `{terminal_tool}` requires these steps to be completed first: [{}]. \
         Complete the required step(s) before calling `{terminal_tool}`.",
        pending.join(", ")
    )
}

/// Извлекает имена tool-вызовов из тела ответа модели (OpenAI chat completion формат).
pub fn extract_tool_names_from_response(response: &GuardrailResponse) -> Vec<String> {
    let mut names = Vec::new();
    if let Some(choices) = response.body.get("choices").and_then(Value::as_array) {
        for choice in choices {
            if let Some(msg) = choice.get("message")
                && let Some(tool_calls) = msg.get("tool_calls").and_then(Value::as_array)
            {
                for tc in tool_calls {
                    if let Some(name) = tc
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(Value::as_str)
                    {
                        names.push(name.to_owned());
                    }
                }
            }
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn make_response_with_tools(tools: &[(&str, &str)]) -> GuardrailResponse {
        let tool_calls: Vec<Value> = tools
            .iter()
            .enumerate()
            .map(|(i, (name, args))| {
                json!({
                    "id": format!("call_{}", i + 1),
                    "type": "function",
                    "function": {"name": name, "arguments": args}
                })
            })
            .collect();
        GuardrailResponse {
            status: 200,
            headers: BTreeMap::new(),
            body: json!({
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "tool_calls": tool_calls},
                    "finish_reason": "tool_calls"
                }]
            }),
        }
    }

    #[test]
    fn detects_premature_terminal_tool() {
        let mut tracker = StepTracker::new();
        tracker.set_required_steps(vec!["plan".into()]);
        let policy = StepPolicy {
            required_steps: vec!["plan".into()],
            before_steps: vec![],
            terminal_tools: vec!["finish".into()],
        };
        let response = make_response_with_tools(&[("finish", "{}")]);
        let error = check_premature_terminal(&response, &policy, &tracker).unwrap();
        assert!(error.contains("[StepEnforcementError]"));
        assert!(error.contains("finish"));
        assert!(error.contains("plan"));
    }

    #[test]
    fn allows_terminal_tool_when_satisfied() {
        let mut tracker = StepTracker::new();
        tracker.set_required_steps(vec!["plan".into()]);
        tracker.record("plan", json!({}));
        let policy = StepPolicy {
            required_steps: vec!["plan".into()],
            before_steps: vec![],
            terminal_tools: vec!["finish".into()],
        };
        let response = make_response_with_tools(&[("finish", "{}")]);
        assert!(check_premature_terminal(&response, &policy, &tracker).is_none());
    }

    #[test]
    fn non_terminal_tool_ignored() {
        let mut tracker = StepTracker::new();
        tracker.set_required_steps(vec!["plan".into()]);
        let policy = StepPolicy {
            required_steps: vec!["plan".into()],
            before_steps: vec![],
            terminal_tools: vec!["finish".into()],
        };
        let response = make_response_with_tools(&[("lookup", "{}")]);
        assert!(check_premature_terminal(&response, &policy, &tracker).is_none());
    }

    #[test]
    fn handles_multiple_terminal_tools() {
        let mut tracker = StepTracker::new();
        tracker.set_required_steps(vec!["plan".into()]);
        let policy = StepPolicy {
            required_steps: vec!["plan".into()],
            before_steps: vec![],
            terminal_tools: vec!["finish".into(), "end".into()],
        };
        let response = make_response_with_tools(&[("end", "{}")]);
        let error = check_premature_terminal(&response, &policy, &tracker).unwrap();
        assert!(error.contains("end"));
    }

    #[test]
    fn no_error_when_no_terminal_tools_configured() {
        let mut tracker = StepTracker::new();
        tracker.set_required_steps(vec!["plan".into()]);
        let policy = StepPolicy {
            required_steps: vec!["plan".into()],
            before_steps: vec![],
            terminal_tools: vec![],
        };
        let response = make_response_with_tools(&[("finish", "{}")]);
        assert!(check_premature_terminal(&response, &policy, &tracker).is_none());
    }
}
