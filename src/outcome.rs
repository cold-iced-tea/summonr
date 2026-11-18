//! Execution outcomes, independent of terminal presentation and transport.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Returned,
    Abstained,
    Exhausted,
    Cancelled,
    Invalid,
    Failed,
    Interrupted,
    Unverified,
}
impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Returned => "returned",
            Self::Abstained => "abstained",
            Self::Exhausted => "exhausted",
            Self::Cancelled => "cancelled",
            Self::Invalid => "invalid",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
            Self::Unverified => "unverified",
        }
    }
    pub fn exit_code(self) -> i32 {
        match self {
            Self::Returned | Self::Abstained => 0,
            Self::Exhausted => 10,
            Self::Cancelled
            | Self::Invalid
            | Self::Failed
            | Self::Interrupted
            | Self::Unverified => 11,
        }
    }
}
impl std::fmt::Display for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
impl std::str::FromStr for Outcome {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "returned" => Ok(Self::Returned),
            "abstained" => Ok(Self::Abstained),
            "exhausted" => Ok(Self::Exhausted),
            "cancelled" => Ok(Self::Cancelled),
            "invalid" => Ok(Self::Invalid),
            "failed" => Ok(Self::Failed),
            "interrupted" => Ok(Self::Interrupted),
            "unverified" => Ok(Self::Unverified),
            _ => Err(format!("unknown outcome `{s}`")),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wire_values_and_exit_codes_stay_compatible() {
        for o in [
            Outcome::Returned,
            Outcome::Abstained,
            Outcome::Exhausted,
            Outcome::Cancelled,
            Outcome::Invalid,
            Outcome::Failed,
            Outcome::Interrupted,
            Outcome::Unverified,
        ] {
            assert_eq!(o.as_str().parse::<Outcome>().unwrap(), o);
            assert_eq!(serde_json::to_value(o).unwrap(), o.as_str());
            assert_eq!(
                serde_json::from_value::<Outcome>(serde_json::json!(o.as_str())).unwrap(),
                o
            );
        }
        assert!("returnd".parse::<Outcome>().is_err());
        assert_eq!(Outcome::Abstained.exit_code(), 0);
        assert_eq!(Outcome::Exhausted.exit_code(), 10);
        assert_eq!(Outcome::Invalid.exit_code(), 11);
    }
}
