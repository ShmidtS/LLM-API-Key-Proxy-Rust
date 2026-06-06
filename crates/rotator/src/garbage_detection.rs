use regex::Regex;
use std::sync::OnceLock;

/// Ошибка, возникающая при обнаружении "мусорного" ответа.
#[derive(Debug, Clone, PartialEq)]
pub struct GarbageResponseError {
    pub reason: String,
    pub score: f32,
}

impl std::fmt::Display for GarbageResponseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "garbage response detected: {} (score: {:.2})",
            self.reason, self.score
        )
    }
}

impl std::error::Error for GarbageResponseError {}

/// Детектор мусорных ответов от LLM.
#[derive(Debug, Clone, Default)]
pub struct GarbageDetector;

impl GarbageDetector {
    pub fn validate(&self, response_text: &str) -> Result<(), GarbageResponseError> {
        validate_response(response_text)
    }
}

/// Проверяет текст ответа на признаки мусорного/некачественного содержимого.
pub fn validate_response(response_text: &str) -> Result<(), GarbageResponseError> {
    let trimmed = response_text.trim();
    if trimmed.is_empty() {
        return Ok(());
    }

    let checks = [
        check_word_repetition(trimmed),
        check_unmatched_brackets(trimmed),
        check_path_leakage(trimmed),
        check_token_flooding(trimmed),
        check_gibberish(trimmed),
    ];

    let mut max_score = 0.0f32;
    let mut primary_reason = String::new();

    for check in checks {
        if let Some(err) = check
            && err.score > max_score {
                max_score = err.score;
                primary_reason = err.reason;
            }
    }

    if max_score > 0.0 {
        Err(GarbageResponseError {
            reason: primary_reason,
            score: max_score,
        })
    } else {
        Ok(())
    }
}

fn check_word_repetition(text: &str) -> Option<GarbageResponseError> {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() < 4 {
        return None;
    }

    let mut max_repeat = 1usize;
    let mut current_repeat = 1usize;
    let mut repeated_word = "";

    for window in words.windows(2) {
        if window[0].eq_ignore_ascii_case(window[1]) {
            current_repeat += 1;
            if current_repeat > max_repeat {
                max_repeat = current_repeat;
                repeated_word = window[0];
            }
        } else {
            current_repeat = 1;
        }
    }

    if max_repeat > 3 {
        Some(GarbageResponseError {
            reason: format!(
                "word repetition detected: '{}' repeated {} times consecutively",
                repeated_word, max_repeat
            ),
            score: (max_repeat as f32).min(10.0) * 0.25,
        })
    } else {
        None
    }
}

fn check_unmatched_brackets(text: &str) -> Option<GarbageResponseError> {
    let mut stack: Vec<char> = Vec::new();
    let mut unmatched = 0usize;

    for ch in text.chars() {
        match ch {
            '(' | '[' | '{' => stack.push(ch),
            ')'
                if stack.pop() != Some('(') => {
                    unmatched += 1;
                }
            ']'
                if stack.pop() != Some('[') => {
                    unmatched += 1;
                }
            '}'
                if stack.pop() != Some('{') => {
                    unmatched += 1;
                }
            _ => {}
        }
    }
    unmatched += stack.len();

    let threshold = 3;
    if unmatched >= threshold {
        Some(GarbageResponseError {
            reason: format!(
                "unmatched brackets/braces/parentheses detected: {} unmatched",
                unmatched
            ),
            score: (unmatched as f32).min(10.0) * 0.3,
        })
    } else {
        None
    }
}

fn path_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)(?:/home/|/var/|/etc/|/usr/|/opt/|/tmp/|/root/|/dev/|/proc/|/sys/|C:\\|D:\\|E:\\|~/)[^\s]+|[^\s]+\.(?:rs|py|js|ts|json|yaml|toml|md|txt|log|ini|cfg|sh|bash|zsh|fish)(?:\s|$)",
        )
        .expect("path regex compilation failed")
    })
}

fn check_path_leakage(text: &str) -> Option<GarbageResponseError> {
    let re = path_regex();
    let matches: Vec<&str> = re.find_iter(text).map(|m| m.as_str()).collect();
    if !matches.is_empty() {
        let count = matches.len().min(5);
        let preview = matches[..count].join(", ");
        Some(GarbageResponseError {
            reason: format!("filesystem path leakage detected: {}", preview),
            score: (matches.len() as f32).min(10.0) * 0.5,
        })
    } else {
        None
    }
}

fn check_token_flooding(text: &str) -> Option<GarbageResponseError> {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() < 50 {
        return None;
    }

    let mut max_char_repeat = 1usize;
    let mut current_char_repeat = 1usize;

    for window in chars.windows(2) {
        if window[0] == window[1] && !window[0].is_whitespace() {
            current_char_repeat += 1;
            max_char_repeat = max_char_repeat.max(current_char_repeat);
        } else {
            current_char_repeat = 1;
        }
    }

    if max_char_repeat > 50 {
        return Some(GarbageResponseError {
            reason: format!(
                "character flooding detected: same char repeated {} times",
                max_char_repeat
            ),
            score: (max_char_repeat as f32).min(100.0) * 0.02,
        });
    }

    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() < 20 {
        return None;
    }

    let mut max_word_repeat = 1usize;
    let mut current_word_repeat = 1usize;

    for window in words.windows(2) {
        if window[0].eq_ignore_ascii_case(window[1]) {
            current_word_repeat += 1;
            max_word_repeat = max_word_repeat.max(current_word_repeat);
        } else {
            current_word_repeat = 1;
        }
    }

    if max_word_repeat > 20 {
        Some(GarbageResponseError {
            reason: format!(
                "word flooding detected: same word repeated {} times consecutively",
                max_word_repeat
            ),
            score: (max_word_repeat as f32).min(100.0) * 0.05,
        })
    } else {
        None
    }
}

fn check_gibberish(text: &str) -> Option<GarbageResponseError> {
    let total_chars = text.chars().filter(|c| !c.is_whitespace()).count();
    if total_chars == 0 {
        return None;
    }

    let non_alphanumeric = text
        .chars()
        .filter(|c| !c.is_whitespace() && !c.is_alphanumeric())
        .count();
    let ratio = non_alphanumeric as f32 / total_chars as f32;

    if ratio > 0.7 {
        Some(GarbageResponseError {
            reason: format!(
                "gibberish detected: {:.1}% non-alphanumeric characters",
                ratio * 100.0
            ),
            score: ratio,
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_response_is_valid() {
        assert!(validate_response("").is_ok());
        assert!(validate_response("   ").is_ok());
    }

    #[test]
    fn normal_text_is_valid() {
        assert!(validate_response("This is a normal response with various words.").is_ok());
        assert!(validate_response("The quick brown fox jumps over the lazy dog.").is_ok());
    }

    #[test]
    fn detects_consecutive_word_repetition() {
        let text = "hello hello hello hello world";
        let err = validate_response(text).unwrap_err();
        assert!(err.reason.contains("word repetition"));
        assert!(err.reason.contains("hello"));
        assert!(err.score > 0.0);
    }

    #[test]
    fn three_repeated_words_is_valid() {
        let text = "hello hello hello world";
        assert!(validate_response(text).is_ok());
    }

    #[test]
    fn detects_unmatched_brackets() {
        let text = "{{{{[[((something))]]}}}} extra {{{ unclosed";
        let err = validate_response(text).unwrap_err();
        assert!(err.reason.contains("unmatched brackets"));
        assert!(err.score > 0.0);
    }

    #[test]
    fn balanced_brackets_are_valid() {
        let text = "{ [ ( hello ) ] } and { [ ( world ) ] }";
        assert!(validate_response(text).is_ok());
    }

    #[test]
    fn detects_path_leakage() {
        let text = "The file is located at /home/user/project/src/main.rs";
        let err = validate_response(text).unwrap_err();
        assert!(err.reason.contains("path leakage"));
        assert!(err.reason.contains("/home/user"));
    }

    #[test]
    fn detects_windows_path_leakage() {
        let text = "Check C:\\Users\\admin\\Documents\\file.txt for details";
        let err = validate_response(text).unwrap_err();
        assert!(err.reason.contains("path leakage"));
        assert!(err.reason.contains("C:\\"));
    }

    #[test]
    fn detects_file_extension_leakage() {
        let text = "Open the config.yaml and settings.toml files";
        let err = validate_response(text).unwrap_err();
        assert!(err.reason.contains("path leakage"));
        assert!(err.reason.contains("config.yaml") || err.reason.contains("settings.toml"));
    }

    #[test]
    fn normal_references_without_paths_are_valid() {
        let text = "Please read the documentation carefully.";
        assert!(validate_response(text).is_ok());
    }

    #[test]
    fn detects_character_flooding() {
        let text = "a".repeat(60);
        let err = validate_response(&text).unwrap_err();
        assert!(err.reason.contains("character flooding"));
        assert!(err.reason.contains("60"));
    }

    #[test]
    fn detects_word_flooding() {
        let text = std::iter::repeat_n("spam", 25)
            .collect::<Vec<_>>()
            .join(" ");
        let err = validate_response(&text).unwrap_err();
        // 25 consecutive identical words trigger word-repetition first (score 2.5
        // beats word-flooding 1.25).  Either reason is valid garbage.
        assert!(
            err.reason.contains("word flooding") || err.reason.contains("word repetition"),
            "expected garbage detection reason, got: {}",
            err.reason
        );
        assert!(err.reason.contains("spam"));
    }

    #[test]
    fn moderate_repetition_is_valid() {
        let text = std::iter::repeat_n("ok", 3).collect::<Vec<_>>().join(" ");
        assert!(validate_response(&text).is_ok());
    }

    #[test]
    fn detects_gibberish_high_non_alphanumeric() {
        let text = "!@#$%^&*()_+-=[]{}|;':\",./<>?`~";
        let err = validate_response(text).unwrap_err();
        assert!(err.reason.contains("gibberish"));
        assert!(err.score > 0.7);
    }

    #[test]
    fn normal_punctuation_is_valid() {
        let text = "Hello, world! How are you? I'm fine, thanks.";
        assert!(validate_response(text).is_ok());
    }

    #[test]
    fn multiple_issues_report_highest_score() {
        let text = format!(
            "hello hello hello hello {} {}",
            "a".repeat(60),
            "/home/user/secret.txt"
        );
        let err = validate_response(&text).unwrap_err();
        assert!(err.score > 0.0);
        assert!(!err.reason.is_empty());
    }

    #[test]
    fn garbage_detector_wrapper_works() {
        let detector = GarbageDetector;
        assert!(detector.validate("normal text here").is_ok());
        let err = detector
            .validate("hello hello hello hello world")
            .unwrap_err();
        assert!(err.reason.contains("word repetition"));
    }

    #[test]
    fn json_response_with_paths_is_flagged() {
        let text = r#"{"choices":[{"message":{"content":"The file is at /etc/passwd"}}]}"#;
        let err = validate_response(text).unwrap_err();
        assert!(err.reason.contains("path leakage"));
    }

    #[test]
    fn json_response_with_repetition_is_flagged() {
        let text = r#"{"choices":[{"message":{"content":"repeat repeat repeat repeat repeat repeat"}}]}"#;
        let err = validate_response(text).unwrap_err();
        assert!(err.reason.contains("word repetition"));
    }

    #[test]
    fn valid_json_response_is_ok() {
        let text = r#"{"choices":[{"message":{"content":"This is a helpful response."}}]}"#;
        assert!(validate_response(text).is_ok());
    }
}
