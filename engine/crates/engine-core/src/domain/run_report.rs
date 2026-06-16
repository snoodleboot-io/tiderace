use super::{Outcome, TestResult};

/// Aggregate of one run's results, with tallies and the process exit code.
#[derive(Debug, Clone, Default)]
pub struct RunReport {
    pub results: Vec<TestResult>,
}

impl RunReport {
    pub fn new(results: Vec<TestResult>) -> Self {
        Self { results }
    }

    pub fn total(&self) -> usize {
        self.results.len()
    }

    pub fn tally(&self, outcome: Outcome) -> usize {
        self.results.iter().filter(|r| r.outcome == outcome).count()
    }

    /// 0 if no failing outcomes, else 1 (pytest-style exit code).
    pub fn exit_code(&self) -> i32 {
        if self.results.iter().any(|r| r.outcome.is_failure()) {
            1
        } else {
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::NodeId;

    fn result(name: &str, outcome: Outcome) -> TestResult {
        TestResult::new(NodeId::new(name), outcome, 0, "")
    }

    #[test]
    fn exit_code_zero_when_all_green() {
        let report = RunReport::new(vec![
            result("a", Outcome::Passed),
            result("b", Outcome::Skipped),
            result("c", Outcome::XFail),
        ]);
        assert_eq!(report.exit_code(), 0);
    }

    #[test]
    fn exit_code_one_on_any_failure() {
        let report = RunReport::new(vec![
            result("a", Outcome::Passed),
            result("b", Outcome::Failed),
        ]);
        assert_eq!(report.exit_code(), 1);
        assert_eq!(report.tally(Outcome::Passed), 1);
        assert_eq!(report.total(), 2);
    }
}
