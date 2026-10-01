//! One lane per thread, each draining the shared queue of units on its own worker (TID-52,
//! TID-118). Which lane runs which unit is decided when a lane is free rather than predicted
//! from weights a cold run does not have.

use std::sync::{Arc, Mutex};
use std::thread;

use crate::domain::{TestItem, TestResult};
use crate::error::{EngineError, Result};
use crate::exec::LaneSeed;

/// Start one thread per seed, drain `units` across them, and join. Every unit's results carry
/// the lane, the unit's rank in the schedule and its start and end (TID-78), and the lane's
/// peak resident size (TID-106). The first lane to fail fails the run — after every lane has
/// been joined, so no worker is left behind.
pub(crate) fn drain_all(
    seeds: Vec<Box<dyn LaneSeed>>,
    units: Vec<Vec<TestItem>>,
) -> Result<Vec<TestResult>> {
    let queue = Arc::new(Mutex::new(units));
    // The run's clock for the schedule stamps (TID-78): every unit's start and end is measured
    // from here, so a report can be drawn as one lane per worker.
    let run_started = std::time::Instant::now();
    // Units are numbered in the order they are taken — heaviest first, so unit 0 is the schedule's
    // first pick and the number reads as its rank.
    let unit_counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let handles: Vec<_> = seeds
        .into_iter()
        .enumerate()
        .map(|(lane, seed)| {
            let queue = queue.clone();
            let unit_counter = unit_counter.clone();
            thread::spawn(move || drain_one(lane, seed, &queue, &unit_counter, run_started))
        })
        .collect();

    let mut all = Vec::new();
    let mut first_err = None;
    for handle in handles {
        match handle.join() {
            Ok(Ok(results)) => all.extend(results),
            Ok(Err(e)) => {
                first_err.get_or_insert(e);
            }
            Err(_) => {
                first_err.get_or_insert_with(|| EngineError::Exec("worker thread panicked".into()));
            }
        }
    }
    match first_err {
        Some(e) => Err(e),
        None => Ok(all),
    }
}

/// One lane: start its worker once and reuse it across every unit it takes. Building a worker
/// per unit would trade the idle time the queue removes for a process launch per module.
fn drain_one(
    lane: usize,
    seed: Box<dyn LaneSeed>,
    queue: &Mutex<Vec<Vec<TestItem>>>,
    unit_counter: &std::sync::atomic::AtomicUsize,
    run_started: std::time::Instant,
) -> Result<Vec<TestResult>> {
    let mut worker = seed.start()?;
    let mut mine = Vec::new();
    // The worker process's peak resident size over the run, sampled after every unit
    // (TID-106); stamped on its results on the way out.
    let mut peak_rss: u64 = 0;
    let finish = |mine: Vec<TestResult>, peak_rss: u64| -> Vec<TestResult> {
        let mb = (peak_rss > 0).then_some(peak_rss >> 20);
        mine.into_iter()
            .map(|r| r.with_worker_peak_rss_mb(mb))
            .collect()
    };
    loop {
        let Some(unit) = queue.lock().expect("the work queue is not poisoned").pop() else {
            return Ok(finish(mine, peak_rss));
        };
        let unit_index = unit_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let started_ms = run_started.elapsed().as_millis() as u64;
        let results = worker.run(&unit)?;
        let ended_ms = run_started.elapsed().as_millis() as u64;
        mine.extend(
            results
                .into_iter()
                .map(|r| r.with_schedule(lane, unit_index, started_ms, ended_ms)),
        );
        if let Some(rss) = worker.pid().and_then(crate::runner::process_rss_bytes) {
            peak_rss = peak_rss.max(rss);
        }
        if worker.is_lost() {
            // Its last unit is reported; the queue drains on the other lanes (TID-93).
            return Ok(finish(mine, peak_rss));
        }
    }
}
