//! `-k` decided by the daemon for every candidate the state can vouch for (TID-102).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use engine_core::domain::{NodeId, Outcome, TestResult};
use engine_core::exec::KeywordExpr;
use engine_core::runner::RecordedOutcome;

use crate::state::plan::{PersistedState, TestRecord};

/// Whether `id` is `cand` itself or one of its runtime expansions — a parametrize case
/// (`cand[…]`) or an inherited method (`cand::…`) — and not a sibling sharing a prefix.
/// The candidates a `-k` run still sends to the workers (TID-102): every one the daemon cannot
/// vouch for, plus every one whose recorded keywords match `expr` — and, beside them, the
/// module-import skips the daemon replays from their records.
///
/// A candidate's records are its own and its expansions' — `cand[case]`, `cand::method` for an
/// inherited-methods class — grouped from the state in one pass. The daemon vouches for a
/// candidate when it has at least one record, every record carries the keywords the shim
/// matched against, every record has a recorded dependency footprint, and no dependency has
/// changed since (`current` knows its hash; `changed` does not list it) — the same test that
/// trusts a purity verdict. Anything less, and the workers judge it as before: a new test, a
/// module-import skip the shim never judged, a node whose file was edited, a footprint that
/// coverage never captured. The vouched-for candidates with no matching record are the ones
/// that stay behind.
///
/// A module that skipped at import is reported skipped under any `-k` — pytest's collection
/// skips it before `-k` is consulted, and so does the shim — so its records, which carry no
/// keywords, are replayed when the module itself is unchanged, rather than sent to a worker to
/// be imported and skipped again: 522 of pirn-core's 5,482 nodes, a third of the round trip.
pub(crate) fn keyword_prefilter(
    state: &PersistedState,
    changed: &BTreeSet<String>,
    current: &BTreeMap<String, String>,
    candidates: &[String],
    expr: &KeywordExpr,
) -> (Vec<String>, Vec<TestResult>) {
    let wanted: HashSet<&str> = candidates.iter().map(String::as_str).collect();
    // Which candidate a record answers for: itself, its parametrized parent, or its class.
    let owner = |id: &str| -> Option<String> {
        if wanted.contains(id) {
            return Some(id.to_string());
        }
        let bare = NodeId::bare_of(id);
        if wanted.contains(bare) {
            return Some(bare.to_string());
        }
        let class = NodeId::parent_of(bare)?;
        wanted.contains(class).then(|| class.to_string())
    };
    let mut records: HashMap<String, Vec<(&str, &TestRecord)>> = HashMap::new();
    for (id, rec) in &state.tests {
        if let Some(cand) = owner(id) {
            records.entry(cand).or_default().push((id, rec));
        }
    }
    let unchanged = |r: &TestRecord| {
        !r.deps.is_empty()
            && r.deps
                .iter()
                .all(|d| current.contains_key(d) && !changed.contains(d))
    };
    let judged = |recs: &[(&str, &TestRecord)]| {
        !recs.is_empty()
            && recs
                .iter()
                .all(|(_, r)| !r.keywords.is_empty() && unchanged(r))
    };
    let module_skip = |recs: &[(&str, &TestRecord)]| {
        !recs.is_empty()
            && recs.iter().all(|(_, r)| {
                r.skip_origin.is_some()
                    && r.outcome == RecordedOutcome::Ran(Outcome::Skipped)
                    && unchanged(r)
            })
    };
    let mut keep = Vec::new();
    let mut replayed = Vec::new();
    for cand in candidates {
        match records.get(cand.as_str()) {
            Some(recs) if judged(recs) => {
                if recs.iter().any(|(_, r)| expr.matches(&r.keywords)) {
                    keep.push(cand.clone());
                }
            }
            Some(recs) if module_skip(recs) => {
                replayed.extend(recs.iter().map(|(id, r)| {
                    TestResult::new(NodeId::new(*id), Outcome::Skipped, 0, r.detail.clone())
                        .with_skip_origin(r.skip_origin.clone())
                        .with_expanded(*id != cand.as_str())
                }));
            }
            _ => keep.push(cand.clone()),
        }
    }
    (keep, replayed)
}

#[cfg(test)]
mod tests {
    use super::keyword_prefilter;

    /// TID-102: the daemon keeps every candidate it cannot vouch for and every one whose recorded
    /// keywords match, and leaves behind only what it can vouch against.
    #[test]
    fn the_keyword_prefilter_keeps_the_unvouched_and_the_matching() {
        use crate::state::plan::{PersistedState, TestRecord};
        use engine_core::domain::Outcome;
        use engine_core::exec::KeywordExpr;
        use engine_core::runner::RecordedOutcome;
        use std::collections::{BTreeMap, BTreeSet};
        let rec = |kw: &[&str], deps: &[&str]| TestRecord {
            outcome: RecordedOutcome::Ran(Outcome::Passed),
            detail: String::new(),
            deps: deps.iter().map(|d| (*d).to_string()).collect(),
            pure: None,
            must_fork: false,
            keywords: kw.iter().map(|k| (*k).to_string()).collect(),
            skip_origin: None,
        };
        let mut state = PersistedState::default();
        let t = |id: &str, r: TestRecord| (id.to_string(), r);
        state.tests.extend([
            t("t.py::plain", rec(&["t.py", "plain"], &["t.py"])),
            t("t.py::par[1-a]", rec(&["t.py", "par[1-a]"], &["t.py"])),
            t("t.py::par[2-b]", rec(&["t.py", "par[2-b]"], &["t.py"])),
            t(
                "t.py::K::inherited",
                rec(&["t.py", "K", "inherited", "slow"], &["t.py", "base.py"]),
            ),
            t("s.py::unjudged", rec(&[], &["s.py"])), // a module-import skip: no keywords
            t("u.py::edited", rec(&["u.py", "edited"], &["u.py"])), // its dep changed
            t(
                "v.py::unknown_dep",
                rec(&["v.py", "unknown_dep"], &["never_hashed.py"]),
            ),
            // A module that skipped at import: no keywords, replayed while the module is unchanged.
            t(
                "w.py::never",
                TestRecord {
                    outcome: RecordedOutcome::Ran(Outcome::Skipped),
                    detail: "could not import 'nope'".into(),
                    deps: vec!["w.py".into()],
                    pure: None,
                    must_fork: false,
                    keywords: Vec::new(),
                    skip_origin: Some("w.py".into()),
                },
            ),
        ]);
        let current: BTreeMap<String, String> = ["t.py", "base.py", "s.py", "u.py", "w.py"]
            .into_iter()
            .map(|f| (f.to_string(), "h".to_string()))
            .collect();
        let changed: BTreeSet<String> = ["u.py".to_string()].into_iter().collect();
        let candidates: Vec<String> = [
            "t.py::plain",
            "t.py::par",
            "t.py::K",
            "s.py::unjudged",
            "u.py::edited",
            "v.py::unknown_dep",
            "new.py::fresh",
            "w.py::never",
        ]
        .into_iter()
        .map(str::to_string)
        .collect();
        let decide = |expr: &str| {
            keyword_prefilter(
                &state,
                &changed,
                &current,
                &candidates,
                &KeywordExpr::parse(expr).unwrap(),
            )
        };
        let keep = |expr: &str| decide(expr).0;
        // The module-import skip is replayed, matching or not, and never sent.
        let (_, replayed) = decide("zzz");
        assert_eq!(replayed.len(), 1);
        assert_eq!(replayed[0].node_id.as_str(), "w.py::never");
        assert_eq!(replayed[0].skip_origin.as_deref(), Some("w.py"));
        assert_eq!(replayed[0].detail, "could not import 'nope'");
        // The four the daemon cannot vouch for come through whatever the expression says.
        let always = [
            "s.py::unjudged",
            "u.py::edited",
            "v.py::unknown_dep",
            "new.py::fresh",
        ];
        assert_eq!(keep("zzz"), always);
        assert_eq!(
            keep("plain"),
            ["t.py::plain"]
                .into_iter()
                .chain(always)
                .collect::<Vec<_>>()
        );
        // One matching case keeps its whole parametrized candidate; the workers pick the case.
        assert_eq!(
            keep("2-b"),
            ["t.py::par"].into_iter().chain(always).collect::<Vec<_>>()
        );
        // An inherited method's record answers for its class, and a mark name is a keyword.
        assert_eq!(
            keep("slow"),
            ["t.py::K"].into_iter().chain(always).collect::<Vec<_>>()
        );
        assert_eq!(keep("not t.py"), always);
        assert_eq!(keep("t.py and not par").len(), 2 + always.len());
    }
}
