use std::fmt;

use serde::{Deserialize, Serialize};

/// The closed set of final test states — the engine's whole result alphabet.
///
/// The serde form *is* the wire token (`passed`, `xfail`, …): what the shim sends, what the
/// reports print, what the verdict store writes. A token nothing here names reads as `Error`,
/// so a shim that grew a new state cannot pass as anything else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Passed,
    Failed,
    Skipped,
    // The derive's own spelling (`x_fail`) is what the result cache wrote before TID-109; the
    // alias keeps those entries readable.
    #[serde(rename = "xfail", alias = "x_fail")]
    XFail,
    #[serde(rename = "xpass", alias = "x_pass")]
    XPass,
    #[serde(other)]
    Error,
}

impl Outcome {
    /// Outcomes that make a run "red" (non-zero exit). `XPass` strictness is a Phase-4 policy
    /// knob; the non-strict default (xpass is not a failure) is used here.
    pub fn is_failure(self) -> bool {
        matches!(self, Outcome::Failed | Outcome::Error)
    }

    /// The token, as a `&'static str` for the places that need one. Equal to the serde form —
    /// a unit test holds the two together.
    pub fn token(self) -> &'static str {
        match self {
            Outcome::Passed => "passed",
            Outcome::Failed => "failed",
            Outcome::Skipped => "skipped",
            Outcome::XFail => "xfail",
            Outcome::XPass => "xpass",
            Outcome::Error => "error",
        }
    }

    /// The outcome a token names — through serde, the one reader — with a token nothing here
    /// names reading as `Error`. For the places that hold a token as a string (the in-process
    /// transport's Python dict, a scripted test double).
    pub fn parse(token: &str) -> Self {
        use serde::Deserialize;
        Outcome::deserialize(
            serde::de::value::StrDeserializer::<serde::de::value::Error>::new(token),
        )
        .unwrap_or(Outcome::Error)
    }

    /// Every outcome, in report order.
    pub const ALL: [Outcome; 6] = [
        Outcome::Passed,
        Outcome::Failed,
        Outcome::Error,
        Outcome::Skipped,
        Outcome::XFail,
        Outcome::XPass,
    ];
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.token())
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
    fn the_token_is_the_serde_form_and_an_unknown_one_reads_as_error() {
        for o in Outcome::ALL {
            let wire = serde_json::to_string(&o).unwrap();
            assert_eq!(wire, format!("\"{}\"", o.token()));
            assert_eq!(serde_json::from_str::<Outcome>(&wire).unwrap(), o);
            assert_eq!(o.to_string(), o.token());
        }
        assert_eq!(
            serde_json::from_str::<Outcome>("\"kaboom\"").unwrap(),
            Outcome::Error
        );
    }
}
