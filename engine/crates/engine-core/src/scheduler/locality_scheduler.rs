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

    /// One unit per locality group, heaviest first (TID-52).
    ///
    /// Steps 1 and 2 above are unchanged — group by snapshot scope, order longest-first. Only step 3,
    /// the greedy assignment onto a fixed set of bins, is dropped: the runner drains these from a
    /// queue, so which worker runs which group is decided when a worker is free rather than
    /// predicted from weights a cold run does not have.
    ///
    /// Ordering by weight still matters even though the assignment is dynamic: a long unit started
    /// last *is* the tail of the run, so the heaviest goes out first.
    ///
    /// A group heavier than one perfect bin is sharded, for a different reason than the static plan
    /// splits one. There, an oversized group would define the makespan of whichever bin held it.
    /// Here, a corpus that is *one* module would otherwise be a single unit — one worker, and the
    /// other seven with nothing to take. Each shard still holds consecutive tests of the one module,
    /// so a shard's worker builds that module's snapshot once, exactly as a split group always has.
    fn units(&self, input: &ScheduleInput) -> Vec<WorkerBatch> {
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
        let mut groups: Vec<(&str, Group)> = groups_by_key.into_iter().collect();
        groups.sort_by(|(ka, a), (kb, b)| b.total_ms.cmp(&a.total_ms).then(ka.cmp(kb)));

        // One perfect bin. A unit at most this heavy means the queue can always keep every worker
        // busy; a unit heavier than this is the one thing a queue cannot schedule around.
        let cap = ((total_ms as f64) / (input.workers() as f64)).ceil() as u64;
        let mut units: Vec<WorkerBatch> = Vec::new();
        for (_key, group) in groups {
            let mut batch = WorkerBatch::new(units.len());
            for (node, dur) in group.items {
                // Shard on the way past the cap rather than before it, so a group that fits exactly
                // stays whole and a group of one enormous test is never split into nothing.
                if cap > 0 && !batch.is_empty() && batch.est_total_ms() + dur > cap {
                    units.push(std::mem::replace(
                        &mut batch,
                        WorkerBatch::new(units.len() + 1),
                    ));
                }
                batch.push(node, dur);
            }
            if !batch.is_empty() {
                units.push(batch);
            }
        }
        units
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
    fn units_are_per_module_heaviest_first_and_each_holds_one_module() {
        // Four modules that each fit inside a perfect bin, and three workers. `plan` would commit
        // them to three bins now; `units` hands out more than that, so which worker runs which is
        // decided when a worker is free (TID-52).
        let input = ScheduleInput::new(
            vec![
                t("light::a", "module:light", 1),
                t("heavy::a", "module:heavy", 20),
                t("heavy::b", "module:heavy", 10),
                t("middle::a", "module:middle", 12),
                t("small::a", "module:small", 5),
            ],
            3, // cap = ceil(48/3) = 16, so only `heavy` (30) is sharded
        );
        let units = LocalityScheduler::default().units(&input);
        assert_eq!(
            units
                .iter()
                .map(WorkerBatch::est_total_ms)
                .collect::<Vec<_>>(),
            vec![20, 10, 12, 5, 1],
            "heaviest group first, sharded at the cap — a 30ms module must not become a unit twice \
             the size of a perfect bin, which is the one thing a queue cannot schedule around"
        );
        for u in &units {
            let modules: std::collections::HashSet<_> = u
                .items()
                .iter()
                .map(|n| n.as_str().split("::").next().unwrap())
                .collect();
            assert_eq!(
                modules.len(),
                1,
                "a unit is one module — the snapshot is reused"
            );
        }
    }

    #[test]
    fn a_single_module_corpus_still_yields_a_unit_per_worker() {
        // The failure mode a queue introduces if a group is never sharded: one module is one unit,
        // one worker takes it, and the other seven have nothing to take. A corpus that is a single
        // large file is not exotic.
        let tests: Vec<_> = (0..80)
            .map(|i| t(&format!("m::t{i}"), "module:m", 1))
            .collect();
        let units = LocalityScheduler::default().units(&ScheduleInput::new(tests, 8));
        assert_eq!(
            units.len(),
            8,
            "eight shards for eight workers, not one unit: {units:?}"
        );
        assert!(
            units.iter().all(|u| u.items().len() == 10),
            "and evenly, since every test weighs the same here"
        );
    }

    #[test]
    fn units_hold_every_test_exactly_once() {
        // Whatever the ordering does, the queue built from these units is the whole corpus.
        let tests: Vec<_> = (0..20)
            .map(|i| {
                t(
                    &format!("m{}::t{i}", i % 4),
                    &format!("module:m{}", i % 4),
                    i,
                )
            })
            .collect();
        let units = LocalityScheduler::default().units(&ScheduleInput::new(tests, 3));
        let mut seen: Vec<String> = units
            .iter()
            .flat_map(|u| u.items().iter().map(|n| n.as_str().to_string()))
            .collect();
        seen.sort();
        seen.dedup();
        assert_eq!(
            seen.len(),
            20,
            "no test is dropped or duplicated by unit formation"
        );
    }

    #[test]
    fn a_partition_scheduler_keeps_its_static_plan_as_its_units() {
        // The default `units` is `plan`, so the round-robin baseline — which *is* a partition —
        // still yields one unit per worker and behaves exactly as it did.
        let tests: Vec<_> = (0..9)
            .map(|i| t(&format!("m{i}::t"), "module:m", 1))
            .collect();
        let input = ScheduleInput::new(tests, 3);
        assert_eq!(
            crate::scheduler::RoundRobinScheduler.units(&input),
            crate::scheduler::RoundRobinScheduler.plan(&input)
        );
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
