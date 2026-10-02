//! `ClosureHash` (W14) — the `fixture_closure` term of the ADR-E004 content-addressed cache key.
//!
//! Hashes the post-override, post-parametrization transitive fixture closure of a test. Phase 5
//! consumes it. The **type** (a 32-byte digest newtype + accessors) is frozen here at the contract
//! step; the **builder** ([`ClosureHasher`]) that walks the closure and computes the digest is
//! implemented by Lane FX-graph (subagent fx-hash).
//!
//! **Digest construction (understand-before-applying).** The contract freezes a 32-byte digest but
//! not the algorithm. The engine workspace has **no cryptographic-hash dependency** (only serde /
//! thiserror / regex), and adding one (`sha2`/`blake3`) needs network + a dependency-approval flag
//! we deliberately avoid here. Phase 3 only requires the term be **stable and deterministic** (equal
//! closures ⇒ equal hash; any change to a fixture's identity, dep set, or param selection ⇒ a
//! different hash) — it is content-addressing, not an adversarial security boundary. So we build the
//! 32 bytes from four interleaved FNV-1a lanes seeded with distinct constants and folded with a
//! length-prefixed, separator-delimited byte feed (so `["ab","c"]` and `["a","bc"]` differ). This is
//! self-contained, allocation-light, and fully deterministic across runs/platforms.
//!
//! **Flag for a later phase:** when the ADR-E004 cache store lands (Phase 5) and a collision-
//! resistant guarantee is wanted, swap [`ClosureHasher`]'s mixing for a real cryptographic hash —
//! the frozen `ClosureHash` shape ([u8; 32]) is unchanged, so only this builder moves.

use serde::{Deserialize, Serialize};

/// Four FNV-1a offset-basis seeds (one per output lane). Distinct constants so identical input fed
/// to each lane still yields four different 8-byte chunks.
const LANE_SEEDS: [u64; 4] = [
    0xcbf2_9ce4_8422_2325,
    0x100_0000_01b3_9e37,
    0x84222325_cbf29ce4,
    0x9e3779b97f4a7c15,
];
/// FNV-1a 64-bit prime.
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// A content hash over a test's resolved fixture closure. Distinct per parametrization variant, so
/// parameter variants cache independently (design 04 §8).
///
/// Stored as a fixed 32-byte digest (newtype) — fully defined pure data; equality/hash derive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ClosureHash([u8; 32]);

impl ClosureHash {
    /// Wrap a precomputed 32-byte digest (the form the cache key consumes).
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The raw digest bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Lowercase hex rendering (the form embedded in cache keys / diagnostics).
    pub fn to_hex(&self) -> String {
        let mut s = String::with_capacity(64);
        for b in &self.0 {
            // Two hex chars per byte; no allocation per byte.
            s.push(char::from_digit((b >> 4) as u32, 16).unwrap_or('0'));
            s.push(char::from_digit((b & 0x0f) as u32, 16).unwrap_or('0'));
        }
        s
    }
}

/// Deterministic builder for a [`ClosureHash`] (W14, the `fixture_closure` cache-key term).
///
/// Feed it the closure's defining material in a **fixed order** (the resolver feeds: each fixture's
/// name, scope, dep names, autouse/yield/reinit flags, in topo order, then the selected param id +
/// index per parametrized instance). Identical material ⇒ identical digest; any change ⇒ a different
/// digest. See the module-level note for why this is a self-contained (non-crypto) construction.
#[derive(Debug, Clone)]
pub struct ClosureHasher {
    /// Running FNV-1a state for each of the four output lanes.
    lanes: [u64; 4],
    /// Bytes folded so far — feeds an avalanche step so order/length differences propagate widely.
    count: u64,
}

impl Default for ClosureHasher {
    fn default() -> Self {
        Self::new()
    }
}

impl ClosureHasher {
    /// A fresh hasher seeded with the four lane bases.
    pub fn new() -> Self {
        Self {
            lanes: LANE_SEEDS,
            count: 0,
        }
    }

    /// Fold one byte into every lane (FNV-1a: xor-then-multiply), per-lane rotated by the running
    /// count so the same byte affects each lane differently and position matters.
    fn fold_byte(&mut self, byte: u8) {
        for (i, lane) in self.lanes.iter_mut().enumerate() {
            let mixed = byte ^ (self.count.rotate_left((i as u32) * 7) as u8);
            *lane ^= u64::from(mixed);
            *lane = lane.wrapping_mul(FNV_PRIME);
        }
        self.count = self.count.wrapping_add(1);
    }

    /// Feed a length-prefixed, separator-terminated field so concatenation is unambiguous
    /// (`"ab" + "c"` cannot collide with `"a" + "bc"`). The 8-byte little-endian length prefix is
    /// folded first, then the bytes, then a `0xFF` field terminator.
    pub fn feed(&mut self, bytes: &[u8]) -> &mut Self {
        for b in (bytes.len() as u64).to_le_bytes() {
            self.fold_byte(b);
        }
        for &b in bytes {
            self.fold_byte(b);
        }
        self.fold_byte(0xFF);
        self
    }

    /// Convenience: feed a string field.
    pub fn feed_str(&mut self, s: &str) -> &mut Self {
        self.feed(s.as_bytes())
    }

    /// Convenience: feed a `u64` field (little-endian).
    pub fn feed_u64(&mut self, n: u64) -> &mut Self {
        self.feed(&n.to_le_bytes())
    }

    /// Finalize into a 32-byte digest. Each lane is avalanche-mixed (splitmix64 finalizer) before it
    /// is emitted, so low-entropy inputs still spread across all 32 bytes.
    pub fn finish(&self) -> ClosureHash {
        let mut out = [0u8; 32];
        for (i, &lane) in self.lanes.iter().enumerate() {
            let mut z = lane.wrapping_add(self.count.wrapping_mul(FNV_PRIME));
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^= z >> 31;
            out[i * 8..i * 8 + 8].copy_from_slice(&z.to_le_bytes());
        }
        ClosureHash::from_bytes(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_hasher_is_deterministic() {
        // empty: two empty hashers agree.
        assert_eq!(ClosureHasher::new().finish(), ClosureHasher::new().finish());
    }

    #[test]
    fn same_input_same_hash() {
        // happy: identical feeds ⇒ identical digest.
        let mut a = ClosureHasher::new();
        a.feed_str("db").feed_u64(4).feed_str("seeded");
        let mut b = ClosureHasher::new();
        b.feed_str("db").feed_u64(4).feed_str("seeded");
        assert_eq!(a.finish(), b.finish());
    }

    #[test]
    fn different_input_different_hash() {
        // happy: any change to material ⇒ a different digest.
        let mut a = ClosureHasher::new();
        a.feed_str("db");
        let mut b = ClosureHasher::new();
        b.feed_str("dc");
        assert_ne!(a.finish(), b.finish());
    }

    #[test]
    fn order_sensitive() {
        // ordering: field order matters (closure topo order is load-bearing).
        let mut a = ClosureHasher::new();
        a.feed_str("a").feed_str("b");
        let mut b = ClosureHasher::new();
        b.feed_str("b").feed_str("a");
        assert_ne!(a.finish(), b.finish());
    }

    #[test]
    fn length_prefix_disambiguates_concatenation() {
        // adversarial: ["ab","c"] must not collide with ["a","bc"].
        let mut a = ClosureHasher::new();
        a.feed_str("ab").feed_str("c");
        let mut b = ClosureHasher::new();
        b.feed_str("a").feed_str("bc");
        assert_ne!(a.finish(), b.finish());
    }

    #[test]
    fn distinct_param_index_changes_hash() {
        // boundary: same id, different index (param variant) ⇒ distinct hash.
        let mut a = ClosureHasher::new();
        a.feed_str("p").feed_u64(0);
        let mut b = ClosureHasher::new();
        b.feed_str("p").feed_u64(1);
        assert_ne!(a.finish(), b.finish());
    }

    #[test]
    fn hex_roundtrips_length() {
        let h = ClosureHasher::new().finish();
        assert_eq!(h.to_hex().len(), 64);
        assert_eq!(
            h.as_bytes(),
            ClosureHash::from_bytes(*h.as_bytes()).as_bytes()
        );
    }
}
