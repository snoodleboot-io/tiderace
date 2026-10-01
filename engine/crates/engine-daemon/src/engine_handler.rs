use std::collections::HashSet;
use std::path::{Path, PathBuf};

use engine_core::cache::DirCache;
use engine_core::domain::{TestItem, TestResult};
use engine_core::exec::{ForkWorker, Worker};
use engine_core::exec::{Selection, WorkerStrategy};
use engine_core::runner::{
    ForkOptions, Learned, PhaseTimer, RunPlan, WorkerCount, DEFAULT_DEADLINE_MS,
};

use crate::config::DaemonConfig;
use crate::error::Result;
use crate::rpc::method::{RpcRequest, RpcResponse, RpcResult};
use crate::rpc::server::RpcHandler;
use crate::warm_image::{HeldImage, WarmImage};

/// Summary of an impact-aware run: which tests actually executed vs. were served from warm state.
/// Served results carry the recorded outcome and detail with a zero duration.
#[derive(Debug)]
pub struct ImpactSummary {
    pub results: Vec<TestResult>,
    pub ran: usize,
    pub cached: usize,
}

/// The live [`RpcHandler`]: turns RPC requests into real engine work over a **warm** wellspring
/// (design 08, ADR-E007). The `ForkWorker` is launched lazily on the first `Run` and **reused** across
/// requests, so the second run in a session pays no interpreter/import cost — the daemon's whole point.
pub struct EngineHandler {
    pub(crate) config: DaemonConfig,
    pub(crate) python: String,
    pub(crate) shim: PathBuf,
    pub(crate) root: PathBuf,
    pub(crate) worker: Option<ForkWorker>, // warm wellspring, kept alive across Run requests
    /// The warm **image** for full parallel runs (TID-84): a persistent pool parent holding the
    /// imported suite, from which every `RunFull` forks its workers. Dropped and relaunched when
    /// the tree's files change, so a stale module is never executed. See [`WarmImage`].
    pub(crate) warm: WarmImage,
    /// The last collection and the tree stamp it was taken under (TID-101). Collection is a walk
    /// of every test file, 52 ms of a 390 ms `-k` round trip on a 5,600-node suite, and the daemon
    /// already takes the stamp per run to validate the warm image; the same stamp validates the
    /// collection, which depends on exactly the files the stamp covers.
    pub(crate) collected: Option<(u64, Vec<TestItem>)>,
    /// Content-addressed result cache (ADR-E004, TID-7); see [`DaemonConfig::cache_dir`].
    pub(crate) cache: Option<DirCache>,
}

impl EngineHandler {
    /// A handler configured from the environment — see [`DaemonConfig::from_env`].
    pub fn new(
        python: impl Into<String>,
        shim: impl Into<PathBuf>,
        root: impl Into<PathBuf>,
    ) -> Self {
        Self::with_config(DaemonConfig::from_env(python, shim, root))
    }

    /// A handler with an explicit configuration: nothing here reads the environment.
    pub fn with_config(config: DaemonConfig) -> Self {
        let cache = config.cache_dir.clone().map(DirCache::new);
        Self {
            python: config.python.clone(),
            shim: config.shim.clone(),
            root: config.root.clone(),
            config,
            worker: None,
            cache,
            warm: WarmImage::none(),
            collected: None,
        }
    }

    /// What this handler was configured with.
    pub fn config(&self) -> &DaemonConfig {
        &self.config
    }

    /// Whether a warm image is currently held.
    pub fn is_warm(&self) -> bool {
        self.warm.is_held()
    }

    /// Launch the wellspring once; reuse it thereafter (warm). Runs tests no-fork + restore by default
    /// (the shim forks non-restorable modules for soundness) — the warm RPC `Run` path gets the same
    /// fast execution as the one-shot pool.
    pub(crate) fn worker(&mut self) -> Result<&mut ForkWorker> {
        if self.worker.is_none() {
            let w = ForkWorker::launch(&self.python, &self.shim, &self.root)?
                .with_deadline_ms(DEFAULT_DEADLINE_MS)
                .with_optimistic_no_fork(self.config.optimistic_no_fork);
            self.worker = Some(w);
        }
        Ok(self.worker.as_mut().expect("just launched"))
    }

    /// Run the requested tests (empty ⇒ all), returning full `TestResult`s (with the touched-file
    /// footprint coverage captured, used by impact-aware re-runs).
    pub(crate) fn run_items(&mut self, requested: &[String]) -> Result<Vec<TestResult>> {
        let all = self.collect()?;
        let items: Vec<TestItem> = if requested.is_empty() {
            all
        } else {
            all.into_iter()
                .filter(|it| requested.iter().any(|r| r == it.node_id.as_str()))
                .collect()
        };
        Ok(self.worker()?.run(&items)?)
    }

    pub(crate) fn run(&mut self, requested: &[String]) -> Result<Vec<RpcResult>> {
        Ok(self.run_items(requested)?.into_iter().map(to_rpc).collect())
    }

    /// The root this handler serves.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Run the requested tests across a **parallel pool** of wellsprings (one per core), not the single
    /// warm wellspring — the fix for sequential full runs. Tests run no-fork + restore by default; the
    /// shim forks non-restorable (opaque) modules for soundness. `trusted` node ids (recorded pure +
    /// unchanged, TID-1) run BARE no-fork, skipping the snapshot. That is ~90× cheaper per test only in
    /// the trivial-test microbenchmark; measured on real corpora it is ~3.4× where it applies, and on a
    /// suite built with module-level test doubles it applies to no test at all (TID-41).
    pub(crate) fn run_items_parallel(
        &mut self,
        requested: &[String],
        learned: &Learned,
        selection: Option<&Selection>,
        full_run: bool,
        collected: Option<Vec<TestItem>>,
    ) -> Result<Vec<TestResult>> {
        // The daemon's plan: the platform tier, every core, the engine's default deadline, the
        // ladder unless `TIDERACE_FORCE_FORK=1`, and the memory limit from the environment
        // (TID-106) — the plan itself reads nothing from it.
        let plan = RunPlan {
            strategy: WorkerStrategy::platform_default(),
            workers: WorkerCount::Default(engine_core::runner::default_workers()),
            deadline_ms: DEFAULT_DEADLINE_MS,
            fork: ForkOptions {
                ladder: self.config.optimistic_no_fork,
                ..ForkOptions::default()
            },
            memory_limit_mb: engine_core::runner::memory_limit_mb_from_env(),
            ..RunPlan::default()
        };
        let mut phase = PhaseTimer::start("tiderace-daemon", "run_items");
        // A caller that already collected the tree hands it over (TID-94): a full run collected
        // it for its candidates a moment ago, and collection is a walk of every test file.
        let all = match collected {
            Some(items) => items,
            None => self.collect()?,
        };
        phase.mark("collect items");
        let items: Vec<TestItem> = if requested.is_empty() {
            all
        } else {
            let wanted: HashSet<&str> = requested.iter().map(String::as_str).collect();
            all.into_iter()
                .filter(|it| wanted.contains(it.node_id.as_str()))
                .collect()
        };
        // Warm image (TID-84): this run's workers are forked off a persistent parent that
        // already holds the imported suite — when one is held and still describes the tree, or
        // when this is a full run, which launches it. When the ladder is off
        // (`TIDERACE_FORCE_FORK=1`) the one-shot pool runs as before.
        let mut held = if self.config.optimistic_no_fork {
            self.warm
                .take_for_run(&self.python, &self.shim, &self.root, full_run)?
        } else {
            HeldImage::none()
        };
        phase.mark("warm pool");
        // This run's selection (TID-90): a warm image's workers apply it after the fork; a
        // one-shot pool reads it the way `tiderace run` hands it over, from the environment,
        // set for this run alone so the next request (or a later image launch) sees none of it.
        let env_guard = if held.set_selection(selection.cloned()) {
            None
        } else {
            // SAFETY: the daemon serves one request at a time on this thread, and no other
            // thread reads the environment while a run is being set up.
            selection.map(|sel| unsafe { sel.apply_env() })
        };
        let out = engine_core::runner::run_parallel_warm_notes(
            &self.python,
            &self.shim,
            &self.root,
            items,
            &plan,
            learned,
            held.for_runner(),
        );
        self.warm.put_back(held); // back for the next run, whatever this one's outcome
        drop(env_guard);
        phase.mark("run_parallel");
        Ok(print_notes(out?))
    }
}

/// The run's notes — memory sizing, a cache not saved — go to the daemon's log, which is stderr.
fn print_notes(outcome: engine_core::runner::RunOutcome) -> Vec<TestResult> {
    for line in &outcome.notes.lines {
        eprintln!("tiderace: {line}");
    }
    outcome.results
}

pub(crate) fn to_rpc(r: TestResult) -> RpcResult {
    RpcResult {
        node_id: r.node_id.to_string(),
        outcome: r.outcome,
        duration_ms: r.duration_ms,
    }
}

impl EngineHandler {
    /// One request, one typed outcome; [`RpcHandler::handle`] turns the error into the wire's
    /// `Error { message }` once.
    fn dispatch(&mut self, request: RpcRequest) -> Result<RpcResponse> {
        Ok(match request {
            RpcRequest::Discover => RpcResponse::Discovered {
                node_ids: self
                    .collect()?
                    .iter()
                    .map(|i| i.node_id.to_string())
                    .collect(),
            },
            RpcRequest::Run { node_ids } => RpcResponse::Ran {
                results: self.run(&node_ids)?,
            },
            RpcRequest::RunFull {
                keyword,
                marker,
                strict_markers,
            } => RpcResponse::RanFull {
                results: self.run_full_results(Some(&Selection {
                    keyword,
                    marker,
                    strict_markers,
                }))?,
            },
            RpcRequest::Recycle => {
                self.worker = None; // drop the stale warm interpreter; next Run relaunches it
                RpcResponse::Ran {
                    results: self.run(&[])?,
                }
            }
            RpcRequest::Watch => RpcResponse::Watching,
            RpcRequest::Health => RpcResponse::Healthy {
                pid: self
                    .worker
                    .as_ref()
                    .and_then(ForkWorker::wellspring_pid)
                    .or_else(|| self.warm.pid())
                    .unwrap_or_else(std::process::id),
                warm: self.worker.is_some() || self.is_warm(),
            },
            RpcRequest::Shutdown => RpcResponse::ShuttingDown,
        })
    }
}

impl RpcHandler for EngineHandler {
    fn handle(&mut self, request: RpcRequest) -> RpcResponse {
        self.dispatch(request)
            .unwrap_or_else(|e| RpcResponse::Error {
                message: e.to_string(),
            })
    }
}
