use std::fmt;

/// Which isolation tier executes a batch (TID-17).
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
