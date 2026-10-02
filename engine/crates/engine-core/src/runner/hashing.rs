//! Content hashing for everything that remembers a file by its bytes — verdict records, the
//! sub-interpreter safe-set cache, the daemon's state file. One digest (the engine's
//! deterministic [`ClosureHasher`], no extra dependency), one hex spelling, one sentinel.

use std::path::Path;

use crate::cache::ClosureHasher;

/// The sentinel a missing or unreadable file hashes to: it never equals a real digest, so the
/// file always counts as changed.
pub const MISSING: &str = "missing";

/// The 32-byte digest of `bytes`.
pub fn digest(bytes: &[u8]) -> [u8; 32] {
    *ClosureHasher::new().feed(bytes).finish().as_bytes()
}

/// The hex of [`digest`]: 64 lowercase characters.
pub fn hash_bytes(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(64);
    for b in digest(bytes) {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// The hex content hash of `<root>/rel`, or `None` when the file is missing or unreadable.
pub fn hash_file(root: &Path, rel: &str) -> Option<String> {
    std::fs::read(root.join(rel)).ok().map(|b| hash_bytes(&b))
}

/// [`hash_file`], with [`MISSING`] for a file that cannot be read — the form the persisted
/// records use, so a path that disappears still compares as changed.
pub fn hash_file_or_missing(root: &Path, rel: &str) -> String {
    hash_file(root, rel).unwrap_or_else(|| MISSING.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_is_64_lowercase_chars_and_deterministic() {
        let h = hash_bytes(b"def test_a():\n    pass\n");
        assert_eq!(h.len(), 64);
        assert!(h
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert_eq!(h, hash_bytes(b"def test_a():\n    pass\n"));
        assert_ne!(h, hash_bytes(b"def test_b():\n    pass\n"));
    }

    #[test]
    fn a_missing_file_is_none_and_the_sentinel() {
        let dir = std::env::temp_dir();
        assert_eq!(hash_file(&dir, "no-such-file-tiderace.py"), None);
        assert_eq!(
            hash_file_or_missing(&dir, "no-such-file-tiderace.py"),
            MISSING
        );
    }
}
