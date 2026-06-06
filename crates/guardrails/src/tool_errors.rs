//! Typed tool-channel error messages and execution/resolution error budgets.
//!
//! Паритет с Forge `guardrails/error_tracker.py` ErrorTracker и
//! `core/runner.py` [ToolError]/[ToolResolutionError] emission.
//!
//! - `[ToolError]` — tool был вызван, но упал при выполнении (soft error).
//!   Накапливает consecutive budget; исчерпание → retry до budget.
//! - `[ToolResolutionError]` — tool не найден/не резолвится (hard error).
//!   Имеет отдельный budget; исчерпание → типизированная финальная ошибка.

use crate::error::GuardrailError;

/// Формирует tool-канальное сообщение об ошибке выполнения tool
/// (паритет с Forge `[ToolError]`).
pub fn tool_error_message(tool_name: &str, error: &str) -> String {
    format!("[ToolError] {tool_name}: {error}")
}

/// Формирует tool-канальное сообщение об ошибке резолюции tool
/// (паритет с Forge `[ToolResolutionError]`).
pub fn tool_resolution_error_message(tool_name: &str, error: &str) -> String {
    format!("[ToolResolutionError] {tool_name}: {error}")
}

/// Отслеживает consecutive soft/hard tool-ошибки против лимитов.
///
/// Stateful — создаётся на сессию/задачу. Раздельные счётчики:
/// - `consecutive_tool_errors` для [ToolError] (soft);
/// - `consecutive_resolution_errors` для [ToolResolutionError] (hard).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolErrorBudget {
    max_tool_errors: u32,
    max_tool_resolution_errors: u32,
    consecutive_tool_errors: u32,
    consecutive_resolution_errors: u32,
}

impl ToolErrorBudget {
    /// Создать бюджет с заданными лимитами.
    pub fn new(max_tool_errors: u32, max_tool_resolution_errors: u32) -> Self {
        Self {
            max_tool_errors,
            max_tool_resolution_errors,
            consecutive_tool_errors: 0,
            consecutive_resolution_errors: 0,
        }
    }

    /// Увеличить soft-счётчик [ToolError].
    pub fn record_tool_error(&mut self) {
        self.consecutive_tool_errors += 1;
    }

    /// Увеличить hard-счётчик [ToolResolutionError].
    pub fn record_tool_resolution_error(&mut self) {
        self.consecutive_resolution_errors += 1;
    }

    /// Сбросить оба счётчика (после чистого batch, паритет с Forge reset_errors).
    pub fn reset(&mut self) {
        self.consecutive_tool_errors = 0;
        self.consecutive_resolution_errors = 0;
    }

    /// Soft-бюджет исчерпан (consecutive_tool_errors > max_tool_errors).
    pub fn tool_errors_exhausted(&self) -> bool {
        self.consecutive_tool_errors > self.max_tool_errors
    }

    /// Hard-бюджет исчерпан (consecutive_resolution_errors > max_tool_resolution_errors).
    pub fn resolution_errors_exhausted(&self) -> bool {
        self.consecutive_resolution_errors > self.max_tool_resolution_errors
    }

    /// Текущий soft-счётчик.
    pub fn consecutive_tool_errors(&self) -> u32 {
        self.consecutive_tool_errors
    }

    /// Текущий hard-счётчик.
    pub fn consecutive_resolution_errors(&self) -> u32 {
        self.consecutive_resolution_errors
    }

    /// Soft-лимит.
    pub fn max_tool_errors(&self) -> u32 {
        self.max_tool_errors
    }

    /// Hard-лимит.
    pub fn max_tool_resolution_errors(&self) -> u32 {
        self.max_tool_resolution_errors
    }

    /// Если любой из бюджетов исчерпан — вернуть типизированную финальную ошибку.
    /// Паритет с Forge `tool_errors_exhausted` → raise `ToolExecutionError`.
    pub fn into_final_error_if_exhausted(&self) -> Option<GuardrailError> {
        if self.tool_errors_exhausted() || self.resolution_errors_exhausted() {
            Some(GuardrailError::ToolExecutionBudgetExhausted {
                consecutive_tool_errors: self.consecutive_tool_errors,
                consecutive_resolution_errors: self.consecutive_resolution_errors,
            })
        } else {
            None
        }
    }
}

impl Default for ToolErrorBudget {
    /// Паритет с Forge ErrorTracker default: max_tool_errors = 2.
    fn default() -> Self {
        Self::new(2, 2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_tool_error_message() {
        let msg = tool_error_message("lookup", "connection refused");
        assert_eq!(msg, "[ToolError] lookup: connection refused");
    }

    #[test]
    fn formats_tool_resolution_error_message() {
        let msg = tool_resolution_error_message("delete_all", "unknown tool");
        assert_eq!(msg, "[ToolResolutionError] delete_all: unknown tool");
    }

    #[test]
    fn budget_tracks_separate_counters() {
        let mut budget = ToolErrorBudget::new(1, 1);
        budget.record_tool_error();
        budget.record_tool_resolution_error();
        assert_eq!(budget.consecutive_tool_errors(), 1);
        assert_eq!(budget.consecutive_resolution_errors(), 1);
    }

    #[test]
    fn reset_clears_both() {
        let mut budget = ToolErrorBudget::new(1, 1);
        budget.record_tool_error();
        budget.record_tool_resolution_error();
        budget.reset();
        assert_eq!(budget.consecutive_tool_errors(), 0);
        assert_eq!(budget.consecutive_resolution_errors(), 0);
    }

    #[test]
    fn exhausts_soft_budget_after_max_plus_one() {
        let mut budget = ToolErrorBudget::new(2, 2);
        budget.record_tool_error();
        assert!(!budget.tool_errors_exhausted());
        budget.record_tool_error();
        assert!(!budget.tool_errors_exhausted());
        budget.record_tool_error();
        assert!(budget.tool_errors_exhausted());
    }

    #[test]
    fn exhausts_hard_budget_after_max_plus_one() {
        let mut budget = ToolErrorBudget::new(2, 2);
        budget.record_tool_resolution_error();
        assert!(!budget.resolution_errors_exhausted());
        budget.record_tool_resolution_error();
        assert!(!budget.resolution_errors_exhausted());
        budget.record_tool_resolution_error();
        assert!(budget.resolution_errors_exhausted());
    }

    #[test]
    fn returns_final_error_when_exhausted() {
        let mut budget = ToolErrorBudget::new(1, 1);
        budget.record_tool_error();
        budget.record_tool_error();
        let err = budget.into_final_error_if_exhausted().unwrap();
        assert!(
            matches!(
                err,
                GuardrailError::ToolExecutionBudgetExhausted {
                    consecutive_tool_errors: 2,
                    consecutive_resolution_errors: 0,
                }
            ),
            "got {err:?}"
        );
    }

    #[test]
    fn returns_none_when_not_exhausted() {
        let budget = ToolErrorBudget::new(5, 5);
        assert!(budget.into_final_error_if_exhausted().is_none());
    }

    #[test]
    fn default_matches_forge() {
        let budget = ToolErrorBudget::default();
        assert_eq!(budget.max_tool_errors(), 2);
        assert_eq!(budget.max_tool_resolution_errors(), 2);
    }
}
