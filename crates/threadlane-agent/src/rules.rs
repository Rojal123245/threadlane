use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamRule {
    pub id: String,
    pub name: String,
    pub pattern: String,
    pub reminder: String,
}

#[derive(Debug, Clone)]
pub struct RuleMatch {
    pub rule_id: String,
    pub rule_name: String,
    pub matched_text: String,
    pub reminder: String,
}

pub struct StreamRuleMonitor {
    rules: Vec<(StreamRule, regex::Regex)>,
    accumulated_text: String,
}

impl StreamRuleMonitor {
    pub fn new(rules: Vec<StreamRule>) -> Self {
        let mut compiled = Vec::new();
        for rule in rules {
            if let Ok(re) = regex::Regex::new(&rule.pattern) {
                compiled.push((rule, re));
            }
        }
        Self {
            rules: compiled,
            accumulated_text: String::new(),
        }
    }

    pub fn push_chunk(&mut self, chunk: &str) -> Option<RuleMatch> {
        self.accumulated_text.push_str(chunk);
        for (rule, re) in &self.rules {
            if let Some(m) = re.find(&self.accumulated_text) {
                return Some(RuleMatch {
                    rule_id: rule.id.clone(),
                    rule_name: rule.name.clone(),
                    matched_text: m.as_str().to_string(),
                    reminder: rule.reminder.clone(),
                });
            }
        }
        None
    }

    pub fn reset(&mut self) {
        self.accumulated_text.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stream_rule_monitor_detects_pattern() {
        let rule = StreamRule {
            id: "box_leak".into(),
            name: "no-box-leak".into(),
            pattern: r"Box::leak\(.*\)".into(),
            reminder: "Do not use Box::leak in production code".into(),
        };

        let mut monitor = StreamRuleMonitor::new(vec![rule]);
        assert!(monitor.push_chunk("let x = ").is_none());
        assert!(monitor.push_chunk("Box::leak(").is_none());
        let mat = monitor.push_chunk("ptr);");
        assert!(mat.is_some());
        let m = mat.unwrap();
        assert_eq!(m.rule_id, "box_leak");
        assert_eq!(m.rule_name, "no-box-leak");
        assert_eq!(m.reminder, "Do not use Box::leak in production code");
    }
}
