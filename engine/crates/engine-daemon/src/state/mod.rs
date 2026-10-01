//! The daemon's persisted knowledge of the tree: the impact plan over it, how a run's results
//! fold back into it, and the `-k` verdicts it can take itself (TID-119).

pub mod fold;
pub mod keyword_prefilter;
pub mod plan;
