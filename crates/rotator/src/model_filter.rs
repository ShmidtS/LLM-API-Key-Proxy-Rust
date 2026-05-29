use regex::Regex;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelFilterStatus {
    Normal,
    Ignored,
    Whitelisted,
}

#[derive(Debug, Clone)]
pub struct ModelFilterRule {
    pattern: String,
    regex: Option<Regex>,
    wildcard: Option<Regex>,
}

impl ModelFilterRule {
    fn new(pattern: &str) -> Option<Self> {
        let pattern = pattern.trim();
        if pattern.is_empty() {
            return None;
        }

        Some(Self {
            pattern: pattern.to_owned(),
            regex: should_compile_as_regex(pattern)
                .then(|| Regex::new(pattern).ok())
                .flatten(),
            wildcard: glob_to_regex(pattern).and_then(|pattern| Regex::new(&pattern).ok()),
        })
    }

    fn matches(&self, model_id: &str) -> bool {
        let provider_model_name = model_id.split_once('/').map_or(model_id, |(_, name)| name);
        self.matches_one(model_id) || self.matches_one(provider_model_name)
    }

    fn matches_one(&self, value: &str) -> bool {
        self.regex
            .as_ref()
            .is_some_and(|regex| regex.is_match(value))
            || self
                .wildcard
                .as_ref()
                .is_some_and(|regex| regex.is_match(value))
    }

    pub fn pattern(&self) -> &str {
        &self.pattern
    }
}

#[derive(Debug, Clone, Default)]
pub struct ModelFilterEngine {
    global_allowlist: Vec<ModelFilterRule>,
    global_denylist: Vec<ModelFilterRule>,
    provider_ignore: HashMap<String, Vec<ModelFilterRule>>,
    provider_whitelist: HashMap<String, Vec<ModelFilterRule>>,
}

impl ModelFilterEngine {
    pub fn from_env<'a>(providers: impl IntoIterator<Item = &'a str>) -> Self {
        let mut engine = Self {
            global_allowlist: parse_rules_env("MODEL_ALLOWLIST"),
            global_denylist: parse_rules_env("MODEL_DENYLIST"),
            provider_ignore: HashMap::new(),
            provider_whitelist: HashMap::new(),
        };

        for provider in providers {
            let provider_key = provider.to_ascii_uppercase();
            let ignore_rules = parse_rules_env(&format!("IGNORE_MODELS_{provider_key}"));
            let whitelist_rules = parse_rules_env(&format!("WHITELIST_MODELS_{provider_key}"));

            if !ignore_rules.is_empty() {
                engine
                    .provider_ignore
                    .insert(provider.to_owned(), ignore_rules);
            }
            if !whitelist_rules.is_empty() {
                engine
                    .provider_whitelist
                    .insert(provider.to_owned(), whitelist_rules);
            }
        }

        engine
    }

    pub fn status(&self, provider: Option<&str>, model_id: &str) -> ModelFilterStatus {
        if provider
            .and_then(|provider| self.provider_whitelist.get(provider))
            .is_some_and(|rules| rules.iter().any(|rule| rule.matches(model_id)))
        {
            return ModelFilterStatus::Whitelisted;
        }

        if !self.global_allowlist.is_empty()
            && !self
                .global_allowlist
                .iter()
                .any(|rule| rule.matches(model_id))
        {
            return ModelFilterStatus::Ignored;
        }

        if self
            .global_denylist
            .iter()
            .any(|rule| rule.matches(model_id))
        {
            return ModelFilterStatus::Ignored;
        }

        if provider
            .and_then(|provider| self.provider_ignore.get(provider))
            .is_some_and(|rules| rules.iter().any(|rule| rule.matches(model_id)))
        {
            return ModelFilterStatus::Ignored;
        }

        ModelFilterStatus::Normal
    }

    pub fn is_allowed(&self, provider: Option<&str>, model_id: &str) -> bool {
        self.status(provider, model_id) != ModelFilterStatus::Ignored
    }
}

fn parse_rules_env(key: &str) -> Vec<ModelFilterRule> {
    std::env::var(key)
        .ok()
        .map(|value| parse_rules(&value))
        .unwrap_or_default()
}

fn parse_rules(value: &str) -> Vec<ModelFilterRule> {
    value.split(',').filter_map(ModelFilterRule::new).collect()
}

fn should_compile_as_regex(pattern: &str) -> bool {
    (pattern.starts_with('^') || pattern.ends_with('$') || pattern.contains(".*"))
        && !pattern.contains('?')
        && !pattern.contains('[')
}

fn glob_to_regex(pattern: &str) -> Option<String> {
    let mut regex = String::from("^");
    let mut chars = pattern.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '*' => regex.push_str(".*"),
            '?' => regex.push('.'),
            '[' => {
                let mut class = String::new();
                let mut closed = false;
                for class_ch in chars.by_ref() {
                    if class_ch == ']' {
                        closed = true;
                        break;
                    }
                    class.push(class_ch);
                }

                if closed {
                    regex.push('[');
                    if let Some(rest) = class.strip_prefix('!') {
                        regex.push('^');
                        push_regex_class(&mut regex, rest);
                    } else {
                        push_regex_class(&mut regex, &class);
                    }
                    regex.push(']');
                } else {
                    regex.push_str("\\[");
                    regex.push_str(&regex::escape(&class));
                }
            }
            _ => regex.push_str(&regex::escape(&ch.to_string())),
        }
    }

    regex.push('$');
    Some(regex)
}

fn push_regex_class(regex: &mut String, class: &str) {
    for ch in class.chars() {
        match ch {
            '\\' => regex.push_str("\\\\"),
            '^' => regex.push_str("\\^"),
            _ => regex.push(ch),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_rules_match_full_id_and_provider_model_name() {
        let rule = ModelFilterRule::new("gpt-4*").unwrap();

        assert!(rule.matches("gpt-4o-mini"));
        assert!(rule.matches("openai/gpt-4o-mini"));
        assert!(!rule.matches("gemini-2.5-flash"));
    }

    #[test]
    fn wildcard_rules_support_question_mark_and_character_sets() {
        let question = ModelFilterRule::new("gpt-?").unwrap();
        let class = ModelFilterRule::new("gpt-[45]*").unwrap();

        assert!(question.matches("gpt-4"));
        assert!(!question.matches("gpt-40"));
        assert!(class.matches("gpt-5-preview"));
        assert!(!class.matches("gpt-3.5"));
    }

    #[test]
    fn regex_rules_remain_supported_for_existing_env_filters() {
        let rule = ModelFilterRule::new("^claude-3.*").unwrap();

        assert!(rule.matches("claude-3-5-sonnet-20241022"));
        assert!(!rule.matches("claude-2.1"));
    }

    #[test]
    fn whitelist_has_priority_over_ignore() {
        let mut engine = ModelFilterEngine::default();
        engine.provider_whitelist.insert(
            "openai".to_owned(),
            vec![ModelFilterRule::new("gpt-4o").unwrap()],
        );
        engine.provider_ignore.insert(
            "openai".to_owned(),
            vec![ModelFilterRule::new("gpt-4*").unwrap()],
        );

        assert!(engine.is_allowed(Some("openai"), "gpt-4o"));
        assert!(!engine.is_allowed(Some("openai"), "gpt-4-turbo"));
    }

    #[test]
    fn global_allowlist_stays_restrictive() {
        let engine = ModelFilterEngine {
            global_allowlist: vec![ModelFilterRule::new("^gpt-4.*").unwrap()],
            ..Default::default()
        };

        assert!(engine.is_allowed(None, "gpt-4o-mini"));
        assert!(!engine.is_allowed(None, "gemini-2.5-flash"));
    }
}
