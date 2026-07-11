use std::collections::BTreeMap;

use crate::scheduler::{ScheduleInput, Scheduler, WorkerBatch};

/// The production scheduler (ADR-E010): bin-packs with **two** objectives at once — snapshot locality
/// and makespan.
///
/// 1. **Group** tests by locality key, so a group reuses one per-worker snapshot.
/// 2. **Order** groups longest-processing-time-first (LPT) by total estimated duration.
/// 3. **Assign** each group whole onto the least-loaded worker — unless a group is far larger than the
///    average bin (`split_threshold` × avg), in which case it is split across workers (each shard still
///    keeps a slice of the group, so the snapshot is reused on each shard's worker rather than rebuilt
///    per scattered test as round-robin would force).
///
/// Pure, allocation-light Rust — it runs on every cold run and warm inner loop.
#[derive(Debug, Clone)]
pub struct LocalityScheduler {
    /// Split a group only when its duration exceeds this multiple of the average per-worker bin.
    split_threshold: f64,
}

impl Default for LocalityScheduler {
    fn default() -> Self {
        Self {
            split_threshold: 1.5,
        }
    }
}

impl LocalityScheduler {
    pub fn new(split_threshold: f64) -> Self {
        Self { split_threshold }
    }
}

/// A locality group: the tests sharing one snapshot scope, with their total estimated duration.
struct Group {
    items: Vec<(crate::domain::NodeId, u64)>,
    total_ms: u64,
}

impl Scheduler for LocalityScheduler {
    fn plan(&self, input: &ScheduleInput) -> Vec<WorkerBatch> {
        let n = input.workers();

        // 1. Group by locality key (BTreeMap keeps grouping deterministic).
        let mut groups_by_key: BTreeMap<&str, Group> = BTreeMap::new();
        let mut total_ms: u64 = 0;
        for t in input.tests() {
            total_ms += t.duration_ms();
            let g = groups_by_key.entry(t.locality_key()).or_insert(Group {
                items: Vec::new(),
                total_ms: 0,
            });
            g.items.push((t.node_id().clone(), t.duration_ms()));
            g.total_ms += t.duration_ms();
        }

        // 2. Order groups LPT (heaviest first); tie-break on key for determinism.
        let mut groups: Vec<(&str, Group)> = groups_by_key.into_iter().collect();
        groups.sort_by(|(ka, a), (kb, b)| b.total_ms.cmp(&a.total_ms).then(ka.cmp(kb)));

        // 3. Greedy assignment onto the least-loaded worker, splitting only oversized groups.
        let avg_bin = (total_ms as f64) / (n as f64);
        let split_cap = (avg_bin * self.split_threshold) as u64;
        let mut batches: Vec<WorkerBatch> = (0..n).map(WorkerBatch::new).collect();

        for (_key, group) in groups {
            if group.total_ms > split_cap && group.items.len() > 1 && split_cap > 0 {
                // Split: drop items (heaviest first) each onto the current least-loaded worker. The
                // group still clusters — each worker that gets a shard reuses the snapshot once.
                let mut items = group.items;
                items.sort_by_key(|(_, dur)| std::cmp::Reverse(*dur));
                for (node, dur) in items {
                    let w = least_loaded(&batches);
                    batches[w].push(node, dur);
                }
            } else {
                let w = least_loaded(&batches);
                for (node, dur) in group.items {
                    batches[w].push(node, dur);
                }
            }
        }

        batches.retain(|b| !b.is_empty());
        batches
    }
}

/// Index of the worker with the smallest current bin load (lowest index breaks ties — deterministic).
fn least_loaded(batches: &[WorkerBatch]) -> usize {
    batches
        .iter()
        .enumerate()
        .min_by_key(|(i, b)| (b.est_total_ms(), *i))
        .map(|(i, _)| i)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::NodeId;
    use crate::scheduler::{makespan, ScheduledTest};

    fn t(node: &str, key: &str, ms: u64) -> ScheduledTest {
        ScheduledTest::new(NodeId::new(node), key, ms)
    }

    #[test]
    fn co_locates_a_scope_on_one_worker() {
        // Two modules, each cheap+whole → each should land entirely on a single worker (snapshot ×1).
        let input = ScheduleInput::new(
            vec![
                t("m0::a", "module:m0", 10),
                t("m0::b", "module:m0", 10),
                t("m1::a", "module:m1", 10),
                t("m1::b", "module:m1", 10),
            ],
            2,
        );
        let batches = LocalityScheduler::default().plan(&input);
        for b in &batches {
            // every item in a batch shares the module prefix ⇒ locality preserved
            let prefixes: std::collections::HashSet<_> = b
                .items()
                .iter()
                .map(|n| n.as_str().split("::").next().unwrap())
                .collect();
            assert_eq!(
                prefixes.len(),
                1,
                "a batch must hold one module (snapshot reuse)"
            );
        }
    }

    #[test]
    fn beats_round_robin_makespan_on_uneven_durations() {
        use crate::scheduler::RoundRobinScheduler;
        // One heavy module + several light ones — duration-blind round-robin imbalances.
        let mut tests = vec![
            t("big::a", "module:big", 100),
            t("big::b", "module:big", 100),
        ];
        for i in 0..6 {
            tests.push(t(&format!("s{i}::x"), &format!("module:s{i}"), 10));
        }
        let input = ScheduleInput::new(tests, 4);

        let lpt = makespan(&LocalityScheduler::default().plan(&input));
        let rr = makespan(&RoundRobinScheduler.plan(&input));
        assert!(
            lpt <= rr,
            "LocalityScheduler makespan {lpt} must not exceed round-robin {rr}"
        );
    }

    #[test]
    fn splits_a_dominant_group_to_avoid_idle_workers() {
        // One module dwarfs total work across 4 workers → it must be split, not left whole on one.
        let mut tests = Vec::new();
        for i in 0..8 {
            tests.push(t(&format!("huge::t{i}"), "module:huge", 100));
        }
        tests.push(t("tiny::a", "module:tiny", 5));
        let input = ScheduleInput::new(tests, 4);
        let batches = LocalityScheduler::default().plan(&input);
        let huge_workers: std::collections::HashSet<_> = batches
            .iter()
            .filter(|b| b.items().iter().any(|n| n.as_str().starts_with("huge::")))
            .map(|b| b.worker())
            .collect();
        assert!(
            huge_workers.len() > 1,
            "a dominant group must be split across workers"
        );
    }

    #[test]
    fn empty_input_yields_no_batches() {
        let batches = LocalityScheduler::default().plan(&ScheduleInput::new(vec![], 4));
        assert!(batches.is_empty());
    }
}
