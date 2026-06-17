use serde::{Deserialize, Serialize};

/// Fixture/lifecycle scope, ordered narrowest → widest. Snapshot (watermark) layering and
/// scheduling locality key off this ordering (design docs 04–06).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Function,
    Class,
    Module,
    Package,
    Session,
}

impl Scope {
    /// Narrowest = 0 … widest = 4. `a.rank() < b.rank()` ⇒ `a` is narrower (shorter-lived).
    pub fn rank(self) -> u8 {
        match self {
            Scope::Function => 0,
            Scope::Class => 1,
            Scope::Module => 2,
            Scope::Package => 3,
            Scope::Session => 4,
        }
    }

    /// True if `self` outlives (is wider than) `other`.
    pub fn outlives(self, other: Scope) -> bool {
        self.rank() > other.rank()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranks_are_narrow_to_wide() {
        assert!(Scope::Function.rank() < Scope::Module.rank());
        assert!(Scope::Module.rank() < Scope::Session.rank());
    }

    #[test]
    fn outlives_follows_rank() {
        assert!(Scope::Session.outlives(Scope::Function));
        assert!(!Scope::Function.outlives(Scope::Class));
    }

    #[test]
    fn derived_ord_matches_rank() {
        let mut scopes = [Scope::Session, Scope::Function, Scope::Module];
        scopes.sort();
        assert_eq!(scopes, [Scope::Function, Scope::Module, Scope::Session]);
    }
}
