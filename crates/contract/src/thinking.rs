//! One reasoning setting (`docs/model-routing.md`, "Thinking").

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// A session's one reasoning setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingLevel {
    /// No reasoning.
    Off,
    /// The least reasoning.
    Minimal,
    /// Low reasoning.
    Low,
    /// Medium reasoning.
    Medium,
    /// High reasoning.
    High,
    /// Extra-high reasoning.
    Xhigh,
    /// The most reasoning.
    Max,
}

impl ThinkingLevel {
    /// The level's name, as typed and as the wire records it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }

    /// Every level's name, in declaration order.
    #[must_use]
    pub const fn names() -> &'static [&'static str] {
        &["off", "minimal", "low", "medium", "high", "xhigh", "max"]
    }
}

impl FromStr for ThinkingLevel {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "off" => Ok(Self::Off),
            "minimal" => Ok(Self::Minimal),
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            "xhigh" => Ok(Self::Xhigh),
            "max" => Ok(Self::Max),
            _ => Err(format!("unknown thinking level `{s}`")),
        }
    }
}

impl fmt::Display for ThinkingLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_each_level_through_serde() {
        for name in ThinkingLevel::names() {
            let level: ThinkingLevel = serde_json::from_value(serde_json::json!(name)).expect(name);
            assert_eq!(level.as_str(), *name);
            assert_eq!(serde_json::json!(level), serde_json::json!(name));
            assert_eq!(name.parse::<ThinkingLevel>().expect(name), level);
        }
    }

    #[test]
    fn rejects_unknown_and_wrong_case_names() {
        assert!("on".parse::<ThinkingLevel>().is_err());
        assert!("High".parse::<ThinkingLevel>().is_err());
        assert!("".parse::<ThinkingLevel>().is_err());
        assert!(serde_json::from_value::<ThinkingLevel>(serde_json::json!("High")).is_err());
    }
}
