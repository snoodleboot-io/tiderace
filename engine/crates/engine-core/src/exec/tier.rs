use std::fmt;
use std::path::Path;

use crate::domain::{TestItem, TestResult};
#[cfg(not(unix))]
use crate::error::EngineError;
use crate::error::Result;
use crate::exec::knobs::RunKnobs;
use crate::exec::process::ShimTarget;
use crate::exec::worker::Worker;
use crate::runner::{RunNotes, RunPlan};

/// Which isolation tier executes a batch (TID-17). Lives in `exec` because the tiers are its;
/// the runner knows it only as a name and a [`factory`](WorkerStrategy::factory).
///
/// The engine has shipped three for a while, but nothing outside the daemon could ask for one: the
/// CLI always launched a [`ForkWorker`](crate::exec::ForkWorker). Every measurement taken through it
/// therefore described a single configuration while being reported as "tiderace's performance".
/// Naming the tiers is what lets a benchmark say which one it measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WorkerStrategy {
    /// One warm wellspring, fork-per-test (COW isolation, ADR-E003). Unix only.
    #[default]
    Fork,
    /// The safe subset on a parallel sub-interpreter pool, the rest on the platform fallback
    /// (ADR-E015 / TID-11). Hybrid by necessity — see [`WorkerStrategy::is_hybrid`].
    SubInterp,
    /// No fork: snapshot/restore between tests, one process per batch (ADR-E008). Works everywhere.
    Subprocess,
}

impl WorkerStrategy {
    /// The tier used when the caller does not name one: fork where it exists, subprocess elsewhere.
    ///
    /// Windows has no `fork()`, so defaulting to [`Fork`](WorkerStrategy::Fork) there would fail at
    /// launch rather than at parse time — worse, it would fail per batch.
    pub fn platform_default() -> Self {
        if cfg!(unix) {
            Self::Fork
        } else {
            Self::Subprocess
        }
    }

    /// Whether this tier can run on this platform at all.
    pub fn is_available(self) -> bool {
        match self {
            Self::Fork => cfg!(unix),
            Self::SubInterp | Self::Subprocess => true,
        }
    }

    /// Whether the tier routes only part of the corpus to itself.
    ///
    /// [`SubInterp`](WorkerStrategy::SubInterp) is the only one: a sub-interpreter cannot load a
    /// single-phase C extension (numpy's `_multiarray_umath` is the canonical refusal), so the safe
    /// subset is probed and everything else falls back. Running an arbitrary corpus wholly through
    /// sub-interpreters is not a configuration that exists — asking for one would just fail on the
    /// first numpy import.
    pub fn is_hybrid(self) -> bool {
        matches!(self, Self::SubInterp)
    }

    /// The tier that carries whatever [`SubInterp`](WorkerStrategy::SubInterp) cannot.
    pub fn fallback(self) -> Self {
        if cfg!(unix) {
            Self::Fork
        } else {
            Self::Subprocess
        }
    }

    /// Parse a CLI spelling. Hyphens and underscores are interchangeable so `sub-interp`,
    /// `sub_interp` and `subinterp` all work rather than silently differing.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().replace(['-', '_'], "").as_str() {
            "fork" => Some(Self::Fork),
            "subinterp" | "subinterpreter" | "si" => Some(Self::SubInterp),
            "subprocess" | "nofork" => Some(Self::Subprocess),
            _ => None,
        }
    }

    /// Every spelling worth printing in a usage message.
    pub const NAMES: &'static [&'static str] = &["fork", "subinterp", "subprocess"];
}

impl fmt::Display for WorkerStrategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Fork => "fork",
            Self::SubInterp => "subinterp",
            Self::Subprocess => "subprocess",
        })
    }
}

impl WorkerStrategy {
    /// The factory that builds this tier's lanes for one run (TID-118). The runner asks it to
    /// [`claim`](TierFactory::claim) what it runs outside the lane loop, to
    /// [`prepare`](TierFactory::prepare) once the lane count is known, and for one
    /// [`lane`](TierFactory::lane) per thread; it never matches on the tier itself.
    ///
    /// `warm` is a daemon's already-imported image, which only the fork tier can use; it forces
    /// the fork tier, as the warm run path always has.
    pub fn factory<'a>(
        self,
        target: &ShimTarget,
        plan: &RunPlan,
        knobs: RunKnobs,
        warm: WarmImage<'a>,
    ) -> Result<Box<dyn TierFactory + 'a>> {
        match self {
            Self::Fork => {
                #[cfg(unix)]
                {
                    Ok(Box::new(crate::exec::tiers::fork_tier::ForkTier::new(
                        target.clone(),
                        plan,
                        knobs,
                        warm,
                    )))
                }
                #[cfg(not(unix))]
                {
                    let _ = (target, plan, knobs, warm);
                    Err(EngineError::Unavailable(
                        "fork is unavailable on this platform".to_string(),
                    ))
                }
            }
            Self::Subprocess => Ok(Box::new(
                crate::exec::tiers::subprocess::SubprocessTier::new(
                    target.clone(),
                    knobs.deadline_ms,
                ),
            )),
            Self::SubInterp => {
                // Hybrid: the safe subset on the pool, the rest on the platform fallback — which
                // never gets the warm image, as it never has.
                let fallback = self
                    .fallback()
                    .factory(target, plan, knobs, WarmImage::none())?;
                Ok(Box::new(crate::exec::tiers::subinterp::SubInterpTier::new(
                    target.clone(),
                    plan,
                    fallback,
                )))
            }
        }
    }
}

/// One run's view of a tier: what it runs itself, how many lanes it can field, and a seed for
/// each lane (TID-118). The runner drives every tier through this and nothing else.
pub trait TierFactory {
    /// Items this tier runs outside the lane loop — the sub-interpreter pool's safe subset — with
    /// their results; the rest are scheduled into lanes. The default claims nothing.
    fn claim(
        &mut self,
        items: Vec<TestItem>,
        notes: &mut RunNotes,
    ) -> Result<(Vec<TestResult>, Vec<TestItem>)> {
        let _ = notes;
        Ok((Vec::new(), items))
    }

    /// Once: the lane count the scheduler chose and the modules file every lane starts from.
    /// Returns the lane count to use — fewer when memory says so (TID-106). The default keeps it.
    fn prepare(&mut self, lanes: usize, modules: &Path, notes: &mut RunNotes) -> Result<usize> {
        let _ = (modules, notes);
        Ok(lanes)
    }

    /// The seed for lane `index`, started on that lane's own thread.
    fn lane(&mut self, index: usize, modules: &Path) -> Result<Box<dyn LaneSeed>>;
}

/// What a lane's thread turns into its worker. A seed rather than a worker so that a tier whose
/// workers are processes launches them in parallel, one per thread, as before.
pub trait LaneSeed: Send {
    fn start(self: Box<Self>) -> Result<Box<dyn Worker>>;
}

/// A daemon's already-imported image for this run's fork-tier workers to fork from (TID-84), or
/// none. A plain type on every platform, so the runner carries it without a `cfg`.
pub struct WarmImage<'a> {
    #[cfg(unix)]
    pool: Option<&'a mut crate::exec::WellspringPool>,
    #[cfg(not(unix))]
    _none: std::marker::PhantomData<&'a ()>,
}

impl<'a> WarmImage<'a> {
    /// No warm image: the run imports for itself.
    pub fn none() -> Self {
        Self {
            #[cfg(unix)]
            pool: None,
            #[cfg(not(unix))]
            _none: std::marker::PhantomData,
        }
    }

    /// Fork this run's workers off `pool`.
    #[cfg(unix)]
    pub fn of(pool: &'a mut crate::exec::WellspringPool) -> Self {
        Self { pool: Some(pool) }
    }

    /// Whether there is an image to fork from.
    pub fn is_some(&self) -> bool {
        #[cfg(unix)]
        {
            self.pool.is_some()
        }
        #[cfg(not(unix))]
        {
            false
        }
    }

    #[cfg(unix)]
    pub(crate) fn into_pool(self) -> Option<&'a mut crate::exec::WellspringPool> {
        self.pool
    }
}

#[cfg(test)]
mod tests {
    use super::WorkerStrategy;

    #[test]
    fn parses_every_advertised_name() {
        for name in WorkerStrategy::NAMES {
            assert!(
                WorkerStrategy::parse(name).is_some(),
                "advertised name {name:?} must parse"
            );
        }
    }

    #[test]
    fn spelling_variants_agree() {
        for s in ["subinterp", "sub-interp", "sub_interp", "SubInterp", "SI"] {
            assert_eq!(
                WorkerStrategy::parse(s),
                Some(WorkerStrategy::SubInterp),
                "{s:?} must resolve to the same tier"
            );
        }
        assert_eq!(
            WorkerStrategy::parse("no-fork"),
            Some(WorkerStrategy::Subprocess)
        );
    }

    #[test]
    fn unknown_name_is_rejected_rather_than_defaulted() {
        // Falling back to the default on a typo would run a different tier than the user asked for
        // and report the run as if nothing were wrong.
        assert_eq!(WorkerStrategy::parse("forkk"), None);
        assert_eq!(WorkerStrategy::parse(""), None);
    }

    #[test]
    fn display_round_trips_through_parse() {
        for s in [
            WorkerStrategy::Fork,
            WorkerStrategy::SubInterp,
            WorkerStrategy::Subprocess,
        ] {
            assert_eq!(WorkerStrategy::parse(&s.to_string()), Some(s));
        }
    }

    #[test]
    fn platform_default_is_available_here() {
        assert!(WorkerStrategy::platform_default().is_available());
        assert!(WorkerStrategy::default().is_available() || !cfg!(unix));
    }

    #[test]
    fn fork_is_the_only_platform_gated_tier() {
        assert_eq!(WorkerStrategy::Fork.is_available(), cfg!(unix));
        assert!(WorkerStrategy::Subprocess.is_available());
        assert!(WorkerStrategy::SubInterp.is_available());
    }

    #[test]
    fn only_subinterp_is_hybrid_and_its_fallback_is_runnable() {
        assert!(WorkerStrategy::SubInterp.is_hybrid());
        assert!(!WorkerStrategy::Fork.is_hybrid());
        assert!(!WorkerStrategy::Subprocess.is_hybrid());
        assert!(WorkerStrategy::SubInterp.fallback().is_available());
    }
}
