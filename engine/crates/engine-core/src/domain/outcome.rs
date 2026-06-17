use serde::{Deserialize, Serialize};

/// The closed set of final test states — the engine's whole result alphabet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Passed,
    Failed,
    Skipped,
    XFail,
    XPass,
    Error,
}

impl Outcome {
    /// Outcomes that make a run "red" (non-zero exit). `XPass` strictness is a Phase-4 policy
    /// knob; the non-strict default (xpass is not a failure) is used here.
    pub fn is_failure(self) -> bool {
        matches!(self, Outcome::Failed | Outcome::Error)
    }

    /// Parse the wire token emitted by the shim. Unknown tokens map to `Error` (never panics).
    pub fn from_wire(s: &str) -> Self {
        match s {
            "passed" => Outcome::Passed,
            "failed" => Outcome::Failed,
            "skipped" => Outcome::Skipped,
            "xfail" => Outcome::XFail,
            "xpass" => Outcome::XPass,
            _ => Outcome::Error,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_set_is_failed_and_error() {
        assert!(Outcome::Failed.is_failure());
        assert!(Outcome::Error.is_failure());
        assert!(!Outcome::Passed.is_failure());
        assert!(!Outcome::Skipped.is_failure());
        assert!(!Outcome::XFail.is_failure());
    }

    #[test]
    fn unknown_wire_token_is_error() {
        assert_eq!(Outcome::from_wire("passed"), Outcome::Passed);
        assert_eq!(Outcome::from_wire("kaboom"), Outcome::Error);
    }
}
