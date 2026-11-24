/// Returns whether `s` is a valid `Gem::Version`: a dot-separated list whose
/// first segment is all digits and whose later segments are non-empty and
/// alphanumeric.
pub fn valid_gem_version(s: &str) -> bool {
    let mut segments = s.split('.');

    let Some(first) = segments.next() else {
        return false;
    };
    if first.is_empty() || !first.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }

    segments
        .all(|segment| !segment.is_empty() && segment.bytes().all(|b| b.is_ascii_alphanumeric()))
}

#[cfg(test)]
mod tests {
    use super::valid_gem_version;

    #[test]
    fn accepts_valid_versions() {
        for s in ["0", "1", "1.0", "1.2.0", "1.0.0.beta1", "10.20.30"] {
            assert!(valid_gem_version(s), "{s:?} should be valid");
        }
    }

    #[test]
    fn rejects_invalid_versions() {
        for s in [
            "",
            "v1.2.0",
            "1.2.0+build",
            "1..0",
            ".1",
            "1.",
            "1-2",
            "1.0 ",
        ] {
            assert!(!valid_gem_version(s), "{s:?} should be invalid");
        }
    }
}
