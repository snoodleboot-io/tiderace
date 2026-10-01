//! Memory-aware worker count, and each worker's peak memory (TID-106).
//!
//! The default worker count was the CPU count, chosen without looking at memory. On pirn-data
//! that put eight Spark JVMs on a box that serial pytest serves with one — 6.5 GB where pytest
//! needs 831 MB. The fork pool shares its imported image copy-on-write, so what each worker adds
//! is its *private* growth: the fixtures, caches and data its tests build. Measured across the
//! 1 October pass, that growth came to about half the image's resident size per worker on the
//! monorepo suites. That ratio is the estimate here, floored so a tiny image still budgets
//! something real, and it bounds the pool from the memory available once the image is up.
//!
//! An explicit `--workers` is honoured as given: a count the user chose is theirs to pay for. An
//! explicit memory limit (`--memory-limit`, `TIDERACE_MEMORY_LIMIT_MB`) caps the pool whatever
//! the count — the one knob that makes a suite like pirn-data fit a small box. Where the platform
//! reports neither available memory nor a process's resident size (Windows, macOS today), nothing
//! is capped and nothing is reported.

use std::path::Path;

/// The least any worker is assumed to add, so an image of a few megabytes does not budget
/// hundreds of workers.
pub const MIN_PER_WORKER_BYTES: u64 = 64 << 20;

/// What a run's workers may occupy together, as a share of what is available once the image is
/// resident: the rest is the user's other work and the kernel's page cache.
const AVAILABLE_SHARE_PERCENT: u64 = 80;

/// The outcome of sizing: the worker count to use, and why, when it is fewer than asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemorySizing {
    pub workers: usize,
    pub note: Option<String>,
}

/// Memory not yet in use, in bytes — `MemAvailable` on Linux. `None` where unknown.
pub fn available_memory_bytes() -> Option<u64> {
    meminfo_field(Path::new("/proc/meminfo"), "MemAvailable:")
}

/// A process's resident set, in bytes — `VmRSS` on Linux. `None` where unknown, or once the
/// process is gone.
pub fn process_rss_bytes(pid: u32) -> Option<u64> {
    meminfo_field(
        &Path::new("/proc").join(pid.to_string()).join("status"),
        "VmRSS:",
    )
}

fn meminfo_field(path: &Path, field: &str) -> Option<u64> {
    let text = std::fs::read_to_string(path).ok()?;
    let line = text.lines().find(|l| l.starts_with(field))?;
    let kb: u64 = line[field.len()..]
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    Some(kb * 1024)
}

/// How many workers to run: `requested`, or fewer when memory says so.
///
/// * `explicit` — the count was the user's (`--workers`): honoured unless a `limit` is set too.
/// * `image_rss` — the imported image's resident size, the basis of the per-worker estimate.
/// * `available` — memory not yet in use; the budget is a share of it, less the image itself.
/// * `limit` — an explicit total for the workers, in bytes; overrides the available-memory budget.
pub fn workers_by_memory(
    requested: usize,
    explicit: bool,
    image_rss: Option<u64>,
    available: Option<u64>,
    limit: Option<u64>,
) -> MemorySizing {
    let requested = requested.max(1);
    let per_worker = image_rss.map_or(MIN_PER_WORKER_BYTES, |rss| {
        (rss / 2).max(MIN_PER_WORKER_BYTES)
    });
    let budget = match (limit, explicit, available) {
        (Some(limit), _, _) => limit,
        (None, true, _) | (None, false, None) => {
            return MemorySizing {
                workers: requested,
                note: None,
            }
        }
        (None, false, Some(available)) => {
            (available * AVAILABLE_SHARE_PERCENT / 100).saturating_sub(image_rss.unwrap_or(0))
        }
    };
    let by_memory = usize::try_from((budget / per_worker).max(1)).unwrap_or(usize::MAX);
    if by_memory >= requested {
        return MemorySizing {
            workers: requested,
            note: None,
        };
    }
    let mb = |b: u64| b >> 20;
    let why = match limit {
        Some(limit) => format!("a memory limit of {} MB", mb(limit)),
        None => format!(
            "{} MB available with the image at {} MB",
            mb(available.unwrap_or(0)),
            mb(image_rss.unwrap_or(0))
        ),
    };
    MemorySizing {
        workers: by_memory,
        note: Some(format!(
            "workers capped at {by_memory} of {requested} by memory: {why}, {} MB assumed per worker \
             (half the image, at least 64 MB); --workers sets a count outright",
            mb(per_worker)
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::{workers_by_memory, MIN_PER_WORKER_BYTES};

    const MB: u64 = 1 << 20;

    #[test]
    fn plenty_of_memory_leaves_the_count_alone() {
        let s = workers_by_memory(8, false, Some(470 * MB), Some(12_000 * MB), None);
        assert_eq!((s.workers, s.note), (8, None));
    }

    #[test]
    fn a_large_image_on_a_small_box_caps_the_pool() {
        // 2 GB image, 6 GB available: 80% is 4.8 GB, less the image is 2.8 GB, at 1 GB a worker.
        let s = workers_by_memory(8, false, Some(2048 * MB), Some(6144 * MB), None);
        assert_eq!(s.workers, 2);
        assert!(s.note.as_deref().unwrap().contains("capped at 2 of 8"));
    }

    #[test]
    fn an_explicit_count_is_honoured_without_a_limit() {
        let s = workers_by_memory(8, true, Some(2048 * MB), Some(6144 * MB), None);
        assert_eq!((s.workers, s.note), (8, None));
    }

    #[test]
    fn a_limit_caps_even_an_explicit_count() {
        let s = workers_by_memory(8, true, Some(470 * MB), Some(12_000 * MB), Some(600 * MB));
        assert_eq!(s.workers, 2, "{:?}", s.note); // 600 MB at 235 MB a worker
        assert!(s
            .note
            .as_deref()
            .unwrap()
            .contains("memory limit of 600 MB"));
    }

    #[test]
    fn never_below_one_worker() {
        let s = workers_by_memory(8, false, Some(4096 * MB), Some(1024 * MB), None);
        assert_eq!(s.workers, 1);
        let s = workers_by_memory(4, true, None, None, Some(1));
        assert_eq!(s.workers, 1);
    }

    #[test]
    fn a_tiny_image_still_budgets_the_floor_per_worker() {
        // 4 MB image: the estimate is the 64 MB floor, not 2 MB.
        let s = workers_by_memory(64, false, Some(4 * MB), Some(1024 * MB), None);
        assert_eq!(s.workers, (1024 * 80 / 100 - 4) / 64);
        assert_eq!(MIN_PER_WORKER_BYTES, 64 * MB);
    }

    #[test]
    fn unknown_memory_caps_nothing() {
        let s = workers_by_memory(8, false, None, None, None);
        assert_eq!((s.workers, s.note), (8, None));
    }
}
