//! From collected items to the queue of units the lanes drain (TID-118): weights from what
//! earlier runs recorded, the scheduler's partition, and the modules file every lane starts
//! from.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::domain::{NodeId, TestItem};
use crate::error::Result;
use crate::runner::{Learned, RunPlan, Sharding};
use crate::scheduler::{ScheduleInput, ScheduledTest};

/// The run's units, **reversed** so `pop` takes the heaviest first: handing out the heaviest
/// unit first is what keeps a long module off the end of the run — started last, it *is* the
/// tail. Empty units are dropped.
pub(crate) fn units(
    items: &[TestItem],
    plan: &RunPlan,
    learned: &Learned,
    workers: usize,
) -> Vec<Vec<TestItem>> {
    // node id -> item, to rebuild each unit's TestItems from the scheduler's NodeId batches.
    let mut by_node: HashMap<String, TestItem> = items
        .iter()
        .map(|i| (i.node_id.to_string(), i.clone()))
        .collect();
    // Weight each collected item by what it cost last time (TID-62), or 1 on a cold run. The cold
    // weight is why the assignment must not be static (TID-52): one-per-test says nothing about a
    // suite whose per-test cost spans four orders of magnitude. The recorded weight is what turns
    // the queue's order from "most tests first" into "most time first", which is what keeps a heavy
    // module off the tail of the run.
    let recorded = RecordedWeights::new(&learned.durations);
    let scheduled: Vec<ScheduledTest> = items
        .iter()
        .map(|i| {
            ScheduledTest::new(
                i.node_id.clone(),
                i.node_id.file().to_string(),
                recorded.weight_of(i.node_id.as_str()),
            )
        })
        .collect();
    let units = plan.scheduler.build().units(
        &ScheduleInput::new(scheduled, workers)
            .with_module_sharding(plan.sharding == Sharding::SplitModules),
    );
    units
        .iter()
        .rev()
        .map(|u| {
            u.items()
                .iter()
                .filter_map(|n| by_node.remove(n.as_str()))
                .collect::<Vec<TestItem>>()
        })
        .filter(|u| !u.is_empty())
        .collect()
}

/// Recorded durations, indexed so a *collected* item can be charged for every node it expands into.
///
/// The scheduler packs collected items — what the regex collector found — but durations are
/// recorded against the ids results *report*, and those differ whenever the engine expands a node
/// at runtime: a parametrized test reports one id per case (`mod.py::test_x[3-b]`), an inherited
/// class one per method (`mod.py::Class::test_y`). Charging the collected item the sum of its cases
/// is what makes a 40-case test weigh like 40 tests rather than one, which on pirn-agents is the
/// difference between 4,019 collected items and 4,657 reported nodes all weighing 1.
///
/// A `BTreeMap` so each lookup is one range scan from the item's id, not a pass over every record.
pub(crate) struct RecordedWeights<'a> {
    by_id: std::collections::BTreeMap<&'a str, u64>,
}

impl<'a> RecordedWeights<'a> {
    pub(crate) fn new(durations: &'a HashMap<NodeId, u64>) -> Self {
        Self {
            by_id: durations.iter().map(|(k, v)| (k.as_str(), *v)).collect(),
        }
    }

    /// The item's own recorded duration plus that of every node expanded from it; `1` when nothing
    /// was recorded, so a cold item still counts and a measured-0ms one still sorts.
    pub(crate) fn weight_of(&self, item: &str) -> u64 {
        let total: u64 = self
            .by_id
            .range(item..)
            .take_while(|(id, _)| id.starts_with(item))
            .filter(|(id, _)| {
                // `item` itself, or an expansion of it — never a sibling that merely shares a
                // prefix (`test_a` must not be charged for `test_ab`).
                id.len() == item.len() || matches!(id.as_bytes()[item.len()], b'[' | b':')
            })
            .map(|(_, ms)| *ms)
            .sum();
        total.max(1)
    }
}

/// The file naming the modules a run executes, handed to every worker's start-up (TID-75).
///
/// Written to a file rather than passed on the command line: a suite can name thousands of
/// modules, and one argument is capped well below that. Removed on drop, which is after every
/// lane has been joined: a worker reads it when it starts, and the last lane may start after the
/// first has already finished.
pub(crate) struct ModulesFile {
    pub(crate) path: PathBuf,
}

impl ModulesFile {
    pub(crate) fn write(items: &[TestItem]) -> Result<Self> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let mut modules: Vec<String> = items.iter().map(|i| i.node_id.file().to_string()).collect();
        modules.sort();
        modules.dedup();
        let path = std::env::temp_dir().join(format!(
            "tiderace-modules-{}-{}.txt",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&path, modules.join("\n") + "\n")?;
        Ok(Self { path })
    }
}

impl Drop for ModulesFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::RecordedWeights;
    use crate::domain::NodeId;
    use std::collections::HashMap;

    #[test]
    fn a_collected_item_is_charged_for_every_node_it_expanded_into() {
        let durations: HashMap<NodeId, u64> = [
            ("m.py::test_a", 5),
            ("m.py::test_a[1]", 100),
            ("m.py::test_a[2]", 200),
            ("m.py::test_ab", 1_000), // shares a prefix; is not an expansion
            ("m.py::Klass::test_x", 40),
            ("m.py::Klass::test_y", 60),
        ]
        .into_iter()
        .map(|(k, v)| (NodeId::new(k), v))
        .collect();
        let w = RecordedWeights::new(&durations);
        assert_eq!(w.weight_of("m.py::test_a"), 305, "own time plus both cases");
        assert_eq!(
            w.weight_of("m.py::test_ab"),
            1_000,
            "a sibling is not a case"
        );
        assert_eq!(
            w.weight_of("m.py::Klass"),
            100,
            "an inherited class is charged for the methods it expanded into"
        );
        assert_eq!(w.weight_of("m.py::test_unknown"), 1, "cold items weigh 1");
    }

    #[test]
    fn a_zero_recording_still_weighs_one() {
        let durations: HashMap<NodeId, u64> = [(NodeId::new("m.py::t"), 0)].into_iter().collect();
        assert_eq!(RecordedWeights::new(&durations).weight_of("m.py::t"), 1);
    }
}
