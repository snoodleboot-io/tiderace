use std::fmt;

use crate::scheduler::{LocalityScheduler, RoundRobinScheduler, Scheduler};

/// Which scheduler partitions the corpus across workers (TID-17).
///
/// Both have shipped since Phase 6; neither was selectable. Keeping round-robin reachable matters
/// because it is the baseline [`Locality`](SchedulerKind::Locality) is supposed to beat — a claim
/// nobody could check from the CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SchedulerKind {
    /// Duration-aware, scope-locality bin-packing (ADR-E010): a module's tests co-locate on one
    /// worker so its snapshot is built once rather than on every worker they scatter to.
    #[default]
    Locality,
    /// Locality-blind cyclic dealing — the makespan baseline, and useful when you want to know
    /// whether a result depends on the packing at all.
    RoundRobin,
}

impl SchedulerKind {
    /// Build the scheduler this kind names.
    pub fn build(self) -> Box<dyn Scheduler> {
        match self {
            Self::Locality => Box::new(LocalityScheduler::default()),
            Self::RoundRobin => Box::new(RoundRobinScheduler),
        }
    }

    /// Parse a CLI spelling; hyphens and underscores are interchangeable.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().replace(['-', '_'], "").as_str() {
            "locality" => Some(Self::Locality),
            "roundrobin" | "rr" => Some(Self::RoundRobin),
            _ => None,
        }
    }

    /// Every spelling worth printing in a usage message.
    pub const NAMES: &'static [&'static str] = &["locality", "round-robin"];
}

impl fmt::Display for SchedulerKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Locality => "locality",
            Self::RoundRobin => "round-robin",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::SchedulerKind;
    use crate::domain::NodeId;
    use crate::scheduler::{ScheduleInput, ScheduledTest};

    #[test]
    fn parses_every_advertised_name() {
        for name in SchedulerKind::NAMES {
            assert!(
                SchedulerKind::parse(name).is_some(),
                "advertised name {name:?} must parse"
            );
        }
    }

    #[test]
    fn spelling_variants_agree() {
        for s in [
            "round-robin",
            "round_robin",
            "roundrobin",
            "RoundRobin",
            "rr",
        ] {
            assert_eq!(
                SchedulerKind::parse(s),
                Some(SchedulerKind::RoundRobin),
                "{s:?} must resolve to the same scheduler"
            );
        }
    }

    #[test]
    fn unknown_name_is_rejected_rather_than_defaulted() {
        assert_eq!(SchedulerKind::parse("locallity"), None);
        assert_eq!(SchedulerKind::parse(""), None);
    }

    #[test]
    fn display_round_trips_through_parse() {
        for k in [SchedulerKind::Locality, SchedulerKind::RoundRobin] {
            assert_eq!(SchedulerKind::parse(&k.to_string()), Some(k));
        }
    }

    /// Both kinds must actually build and place every test — a scheduler that silently dropped items
    /// would lose tests rather than fail, which is the worst way for this to break.
    #[test]
    fn both_kinds_schedule_every_test() {
        let tests: Vec<ScheduledTest> = (0..12)
            .map(|i| {
                ScheduledTest::new(
                    NodeId::new(format!("m{}.py::t{i}", i % 3)),
                    format!("m{}.py", i % 3),
                    1,
                )
            })
            .collect();
        let input = ScheduleInput::new(tests, 4);
        for kind in [SchedulerKind::Locality, SchedulerKind::RoundRobin] {
            let batches = kind.build().plan(&input);
            let placed: usize = batches.iter().map(|b| b.items().len()).sum();
            assert_eq!(placed, 12, "{kind} must place every test");
        }
    }
}
