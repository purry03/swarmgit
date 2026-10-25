//! Git ref-name validation (spec section 5.3).

use crate::consts::{MAX_REFNAME_BYTES, REF_PREFIXES};

/// True when `name` is an allowed swarmgit ref name.
///
/// The rules are the `git check-ref-format` rules listed in the spec, plus the
/// `refs/heads/` or `refs/tags/` prefix restriction.
#[must_use]
pub fn is_valid_refname(name: &str) -> bool {
    if name.is_empty() || name.len() > MAX_REFNAME_BYTES {
        return false;
    }
    if !REF_PREFIXES.iter().any(|prefix| name.starts_with(prefix)) {
        return false;
    }
    if name.starts_with('/') || name.ends_with('/') || name.contains("//") {
        return false;
    }
    if name.contains("..") || name.contains("@{") {
        return false;
    }
    if name.ends_with('.') {
        return false;
    }
    if name.bytes().any(|byte| {
        byte < 0x20
            || byte == 0x7f
            || byte == b' '
            || matches!(byte, b'~' | b'^' | b':' | b'?' | b'*' | b'[' | b'\\')
    }) {
        return false;
    }
    for component in name.split('/') {
        if component.is_empty() || component.starts_with('.') || component.ends_with(".lock") {
            return false;
        }
    }
    true
}

/// True when `name` is a ref allowed in a manifest (prefix and length checks).
#[must_use]
pub fn has_allowed_prefix(name: &str) -> bool {
    REF_PREFIXES.iter().any(|prefix| name.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_normal_names() {
        for name in [
            "refs/heads/main",
            "refs/heads/feature/x",
            "refs/tags/v1.0.0",
            "refs/heads/über",
        ] {
            assert!(is_valid_refname(name), "{name}");
        }
    }

    #[test]
    fn rejects_bad_names() {
        for name in [
            "",
            "main",
            "refs/meta/x",
            "refs/heads/",
            "refs/heads//x",
            "refs/heads/../x",
            "refs/heads/x..y",
            "refs/heads/x@{y",
            "refs/heads/.hidden",
            "refs/heads/x.lock",
            "refs/heads/x.",
            "refs/heads/x y",
            "refs/heads/x~y",
            "refs/heads/x^y",
            "refs/heads/x:y",
            "refs/heads/x?y",
            "refs/heads/x*y",
            "refs/heads/x[y",
            "refs/heads/x\\y",
            "refs/heads/x\ty",
        ] {
            assert!(!is_valid_refname(name), "{name:?}");
        }
    }

    #[test]
    fn rejects_overlong_names() {
        let name = format!("refs/heads/{}", "a".repeat(MAX_REFNAME_BYTES));
        assert!(!is_valid_refname(&name));
    }
}
