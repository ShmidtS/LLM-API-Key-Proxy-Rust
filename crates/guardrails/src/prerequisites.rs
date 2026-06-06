//! Arg-matched tool prerequisites (паритет с Forge `core/steps.py` StepTracker).
//!
//! Tool может объявить зависимость от другого tool. Зависимость бывает двух видов:
//! - name-only: достаточно любого предшествующего вызова prerequisite-tool;
//! - arg-matched: требуется предшествующий вызов prerequisite-tool с тем же
//!   значением определённого аргумента (`match_arg`), что и у текущего вызова.
//!
//! Если вызов нарушает prerequisite, формируется `[PrerequisiteError]`-сообщение
//! для tool-канала, и модель ретраит в обученном паттерне "tool failed".

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// Объявление зависимости tool от другого tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Prerequisite {
    /// Любой предшествующий вызов tool с этим именем удовлетворяет зависимость.
    NameOnly(String),
    /// Предшествующий вызов `tool` с совпадающим значением аргумента `match_arg`.
    ArgMatched { tool: String, match_arg: String },
}

impl Prerequisite {
    fn tool_name(&self) -> &str {
        match self {
            Prerequisite::NameOnly(name) => name,
            Prerequisite::ArgMatched { tool, .. } => tool,
        }
    }
}

/// Результат проверки prerequisites для вызова tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrerequisiteCheck {
    pub satisfied: bool,
    /// Имена prerequisite-tool, которые не были вызваны (или вызваны без совпадения args).
    pub missing: Vec<String>,
}

/// Отслеживает выполненные tool-вызовы (с аргументами) для проверки prerequisites.
///
/// Живёт вне истории сообщений (на уровне runner/сессии), поэтому compaction
/// не может инвалидировать факт выполнения шага (Forge P0-1).
#[derive(Debug, Clone, Default)]
pub struct StepTracker {
    executed_tools: HashMap<String, Vec<Value>>,
    required_steps: Vec<String>,
}

impl StepTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Записать успешный вызов tool с его аргументами.
    pub fn record(&mut self, tool_name: &str, args: Value) {
        self.executed_tools
            .entry(tool_name.to_owned())
            .or_default()
            .push(args);
    }

    /// Был ли tool вызван хотя бы раз.
    pub fn was_executed(&self, tool_name: &str) -> bool {
        self.executed_tools.contains_key(tool_name)
    }

    /// Установить список обязательных шагов для отслеживания.
    pub fn set_required_steps(&mut self, steps: Vec<String>) {
        self.required_steps = steps;
    }

    /// Все обязательные шаги выполнены.
    pub fn is_satisfied(&self) -> bool {
        self.required_steps.iter().all(|s| self.was_executed(s))
    }

    /// Список обязательных шагов, которые ещё не выполнены.
    pub fn pending(&self) -> Vec<String> {
        self.required_steps
            .iter()
            .filter(|s| !self.was_executed(s))
            .cloned()
            .collect()
    }

    /// Проверить, удовлетворены ли prerequisites для вызова `tool_name` с `args`.
    pub fn check_prerequisites(
        &self,
        _tool_name: &str,
        args: &Value,
        prerequisites: &[Prerequisite],
    ) -> PrerequisiteCheck {
        let mut missing = Vec::new();

        for prereq in prerequisites {
            let prereq_tool = prereq.tool_name();
            let Some(prior_calls) = self.executed_tools.get(prereq_tool) else {
                missing.push(prereq_tool.to_owned());
                continue;
            };

            if let Prerequisite::ArgMatched { match_arg, .. } = prereq {
                let required_value = args.get(match_arg);
                let matched = prior_calls
                    .iter()
                    .any(|call| call.get(match_arg) == required_value);
                if !matched {
                    missing.push(prereq_tool.to_owned());
                }
            }
        }

        PrerequisiteCheck {
            satisfied: missing.is_empty(),
            missing,
        }
    }
}

/// Формирует tool-канальное сообщение об ошибке prerequisite (паритет с Forge
/// `[PrerequisiteError]`), которое возвращается модели для повторной попытки.
pub fn prerequisite_error_message(tool_name: &str, missing: &[String]) -> String {
    format!(
        "[PrerequisiteError] Tool `{tool_name}` requires these tools to be called first: [{}]. \
         Call the required tool(s) before `{tool_name}`.",
        missing.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn name_only_prereq_unsatisfied_when_tool_not_called() {
        let tracker = StepTracker::new();
        let check = tracker.check_prerequisites(
            "write_file",
            &json!({"path": "a.txt"}),
            &[Prerequisite::NameOnly("read_file".to_owned())],
        );
        assert!(!check.satisfied);
        assert_eq!(check.missing, vec!["read_file".to_owned()]);
    }

    #[test]
    fn name_only_prereq_satisfied_after_any_call() {
        let mut tracker = StepTracker::new();
        tracker.record("read_file", json!({"path": "other.txt"}));
        let check = tracker.check_prerequisites(
            "write_file",
            &json!({"path": "a.txt"}),
            &[Prerequisite::NameOnly("read_file".to_owned())],
        );
        assert!(check.satisfied);
        assert!(check.missing.is_empty());
    }

    #[test]
    fn arg_matched_prereq_requires_matching_arg_value() {
        let mut tracker = StepTracker::new();
        // read_file вызван с другим path — не удовлетворяет arg-matched prereq.
        tracker.record("read_file", json!({"path": "other.txt"}));
        let check = tracker.check_prerequisites(
            "write_file",
            &json!({"path": "a.txt"}),
            &[Prerequisite::ArgMatched {
                tool: "read_file".to_owned(),
                match_arg: "path".to_owned(),
            }],
        );
        assert!(!check.satisfied);
        assert_eq!(check.missing, vec!["read_file".to_owned()]);
    }

    #[test]
    fn arg_matched_prereq_satisfied_with_matching_arg() {
        let mut tracker = StepTracker::new();
        tracker.record("read_file", json!({"path": "a.txt"}));
        let check = tracker.check_prerequisites(
            "write_file",
            &json!({"path": "a.txt"}),
            &[Prerequisite::ArgMatched {
                tool: "read_file".to_owned(),
                match_arg: "path".to_owned(),
            }],
        );
        assert!(check.satisfied);
    }

    #[test]
    fn tool_without_prerequisites_is_unaffected() {
        let tracker = StepTracker::new();
        let check = tracker.check_prerequisites("search", &json!({"q": "rust"}), &[]);
        assert!(check.satisfied);
        assert!(check.missing.is_empty());
    }

    #[test]
    fn error_message_lists_missing_tools() {
        let msg = prerequisite_error_message("write_file", &["read_file".to_owned()]);
        assert!(msg.contains("[PrerequisiteError]"));
        assert!(msg.contains("write_file"));
        assert!(msg.contains("read_file"));
    }

    #[test]
    fn prerequisite_deserializes_both_forms() {
        let name_only: Prerequisite = serde_json::from_value(json!("read_file")).unwrap();
        assert_eq!(name_only, Prerequisite::NameOnly("read_file".to_owned()));

        let arg_matched: Prerequisite =
            serde_json::from_value(json!({"tool": "read_file", "match_arg": "path"})).unwrap();
        assert_eq!(
            arg_matched,
            Prerequisite::ArgMatched {
                tool: "read_file".to_owned(),
                match_arg: "path".to_owned()
            }
        );
    }
}
