use std::collections::{HashMap, HashSet};

use url::Url;

use crate::model::Catalog;
use crate::version::valid_gem_version;

const MAX_DESCRIPTION_BYTES: usize = 4096;
const MAX_PROVIDERS: usize = 100;

const CHECKSUM_TYPES: [(&str, usize); 5] = [
    ("md5", 32),
    ("sha1", 40),
    ("sha256", 64),
    ("sha384", 96),
    ("sha512", 128),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    pub field: String,
    pub message: String,
}

fn error(field: impl Into<String>, message: impl Into<String>) -> ValidationError {
    ValidationError {
        field: field.into(),
        message: message.into(),
    }
}

/// Returns every validation failure for `c`, so a caller can report the whole
/// document at once.
pub fn validate(catalog: &Catalog) -> Vec<ValidationError> {
    let mut errors = Vec::new();

    if !valid_box_name(&catalog.name) {
        errors.push(error(
            "name",
            "must be org/box using [A-Za-z0-9._-], without a . or .. segment",
        ));
    }

    if catalog.description.len() > MAX_DESCRIPTION_BYTES {
        errors.push(error(
            "description",
            format!("must be at most {MAX_DESCRIPTION_BYTES} bytes"),
        ));
    }

    if catalog.versions.is_empty() {
        errors.push(error("versions", "must contain at least one version"));
    }

    let mut seen_versions = HashSet::new();

    for (version_index, version) in catalog.versions.iter().enumerate() {
        let version_field = format!("versions[{version_index}]");
        let providers_field = format!("{version_field}.providers");

        if !seen_versions.insert(version.version.as_str()) {
            errors.push(error(&version_field, "duplicate version"));
        }

        if !valid_gem_version(&version.version) {
            errors.push(error(
                format!("{version_field}.version"),
                "must be a valid Gem::Version",
            ));
        }

        if version.providers.is_empty() {
            errors.push(error(
                &providers_field,
                "must contain at least one provider",
            ));
        } else if version.providers.len() > MAX_PROVIDERS {
            errors.push(error(
                &providers_field,
                format!("must contain at most {MAX_PROVIDERS} providers"),
            ));
        }

        let mut seen_identities = HashSet::new();
        let mut default_counts: HashMap<&str, usize> = HashMap::new();

        for (provider_index, provider) in version.providers.iter().enumerate() {
            let provider_field = format!("{providers_field}[{provider_index}]");

            if !valid_provider_name(&provider.name) {
                errors.push(error(
                    format!("{provider_field}.name"),
                    "must match [a-z][a-z0-9_]*",
                ));
            }

            validate_url(&provider.url, &format!("{provider_field}.url"), &mut errors);
            validate_checksum(provider, &provider_field, &mut errors);

            if !provider.architecture.is_empty() && !valid_architecture(&provider.architecture) {
                errors.push(error(
                    format!("{provider_field}.architecture"),
                    "must match [A-Za-z0-9_]+",
                ));
            }

            if !seen_identities.insert((provider.name.as_str(), provider.architecture.as_str())) {
                errors.push(error(
                    &provider_field,
                    "duplicate provider name and architecture in this version",
                ));
            }

            if provider.default_architecture == Some(true) {
                if provider.architecture.is_empty() {
                    errors.push(error(
                        format!("{provider_field}.default_architecture"),
                        "may only be true when architecture is set",
                    ));
                } else {
                    let count = default_counts.entry(provider.name.as_str()).or_insert(0);
                    *count += 1;
                    if *count > 1 {
                        errors.push(error(
                            format!("{provider_field}.default_architecture"),
                            "more than one default provider for this name",
                        ));
                    }
                }
            }
        }
    }

    errors
}

/// Returns whether `s` is a valid org/box name: exactly one slash, two
/// non-empty path-safe segments, and neither segment `.` or `..`.
pub fn valid_box_name(s: &str) -> bool {
    let mut segments = s.split('/');
    let (Some(org), Some(box_name), None) = (segments.next(), segments.next(), segments.next())
    else {
        return false;
    };

    valid_name_segment(org) && valid_name_segment(box_name)
}

/// Returns whether `s` is a valid provider name: `[a-z][a-z0-9_]*`.
pub fn valid_provider_name(s: &str) -> bool {
    let mut bytes = s.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };

    first.is_ascii_lowercase()
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// Returns whether `s` is a valid architecture label: `[A-Za-z0-9_]+`.
pub fn valid_architecture(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn valid_name_segment(segment: &str) -> bool {
    if segment.is_empty() || segment == "." || segment == ".." {
        return false;
    }

    let mut bytes = segment.bytes();
    let first = bytes.next().expect("segment is not empty");
    first.is_ascii_alphanumeric()
        && bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn validate_url(url: &str, field: &str, errors: &mut Vec<ValidationError>) {
    match Url::parse(url) {
        Ok(parsed) => {
            if !matches!(parsed.scheme(), "http" | "https") {
                errors.push(error(field, "must use the http or https scheme"));
            }
            if parsed.host_str().is_none_or(str::is_empty) {
                errors.push(error(field, "must have a host"));
            }
            if !parsed.username().is_empty() || parsed.password().is_some() {
                errors.push(error(field, "must not contain userinfo"));
            }
        }
        Err(_) => errors.push(error(field, "must be an absolute URL")),
    }
}

fn validate_checksum(
    provider: &crate::model::Provider,
    provider_field: &str,
    errors: &mut Vec<ValidationError>,
) {
    let checksum_field = format!("{provider_field}.checksum");
    let type_field = format!("{provider_field}.checksum_type");

    match (
        provider.checksum.is_empty(),
        provider.checksum_type.is_empty(),
    ) {
        (true, true) => {}
        (false, true) => errors.push(error(type_field, "must be set when checksum is set")),
        (true, false) => errors.push(error(
            checksum_field,
            "must be set when checksum_type is set",
        )),
        (false, false) => {
            let Some((_, expected_length)) = CHECKSUM_TYPES
                .iter()
                .find(|(name, _)| *name == provider.checksum_type)
            else {
                errors.push(error(
                    type_field,
                    "must be one of md5, sha1, sha256, sha384 or sha512",
                ));
                return;
            };

            if provider.checksum.len() != *expected_length {
                errors.push(error(
                    &checksum_field,
                    format!(
                        "must be {expected_length} hex characters for {}",
                        provider.checksum_type
                    ),
                ));
            } else if !provider.checksum.bytes().all(|b| b.is_ascii_hexdigit()) {
                errors.push(error(&checksum_field, "must be hexadecimal"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ValidationError, valid_architecture, valid_box_name, valid_provider_name, validate,
    };
    use crate::model::{Catalog, Provider, Version};

    fn provider() -> Provider {
        Provider {
            name: "virtualbox".to_string(),
            url: "https://downloads.example.com/acme-base-1.0.0-vbox-amd64.box".to_string(),
            checksum: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                .to_string(),
            checksum_type: "sha256".to_string(),
            architecture: "amd64".to_string(),
            default_architecture: Some(true),
        }
    }

    fn base() -> Catalog {
        Catalog {
            name: "acme/base".to_string(),
            description: "Acme base image".to_string(),
            versions: vec![Version {
                version: "1.0.0".to_string(),
                providers: vec![provider()],
            }],
        }
    }

    fn has_field(errors: &[ValidationError], predicate: impl Fn(&str) -> bool) -> bool {
        errors.iter().any(|e| predicate(&e.field))
    }

    fn has_named(errors: &[ValidationError], field: &str) -> bool {
        has_field(errors, |f| f == field)
    }

    fn has_suffix(errors: &[ValidationError], suffix: &str) -> bool {
        has_field(errors, |f| f.ends_with(suffix))
    }

    #[test]
    fn accepts_valid_catalog() {
        assert!(validate(&base()).is_empty());
    }

    #[test]
    fn aggregates_all_errors() {
        let mut catalog = base();
        catalog.name = "acme".to_string();
        catalog.versions[0].version = "v1".to_string();
        catalog.versions[0].providers[0].url = "ftp://downloads.example.com/x.box".to_string();

        let errors = validate(&catalog);

        assert!(errors.len() >= 3, "{errors:?}");
        assert!(has_named(&errors, "name"));
        assert!(has_suffix(&errors, ".version"));
        assert!(has_suffix(&errors, ".url"));
    }

    #[test]
    fn rejects_bad_names() {
        for name in [
            "acme",
            "acme/base/x",
            "/base",
            "acme/",
            ".acme/base",
            "acme/..",
            "ac me/base",
        ] {
            assert!(!valid_box_name(name), "{name:?} should be invalid");
            let mut catalog = base();
            catalog.name = name.to_string();
            assert!(
                has_named(&validate(&catalog), "name"),
                "{name:?} should be reported"
            );
        }

        assert!(valid_box_name("acme/base"));
        assert!(valid_box_name("acme/a..b"));
    }

    #[test]
    fn rejects_long_description_and_duplicate_or_empty_versions() {
        let mut long = base();
        long.description = "a".repeat(4097);
        assert!(has_named(&validate(&long), "description"));

        let mut at_limit = base();
        at_limit.description = "a".repeat(4096);
        assert!(validate(&at_limit).is_empty());

        let mut duplicate = base();
        duplicate.versions.push(duplicate.versions[0].clone());
        let errors = validate(&duplicate);
        assert!(
            errors
                .iter()
                .any(|e| e.message.contains("duplicate version")),
            "{errors:?}"
        );

        let mut empty = base();
        empty.versions.clear();
        assert!(has_named(&validate(&empty), "versions"));
    }

    #[test]
    fn rejects_empty_and_oversized_provider_lists() {
        let mut empty = base();
        empty.versions[0].providers.clear();
        assert!(has_suffix(&validate(&empty), ".providers"));

        let mut oversized = base();
        oversized.versions[0].providers = (0..101)
            .map(|i| Provider {
                name: format!("p{i}"),
                url: format!("https://downloads.example.com/{i}.box"),
                checksum: String::new(),
                checksum_type: String::new(),
                architecture: String::new(),
                default_architecture: None,
            })
            .collect();
        assert!(has_suffix(&validate(&oversized), ".providers"));
    }

    #[test]
    fn rejects_bad_provider_name_and_url_userinfo() {
        let mut bad_name = base();
        bad_name.versions[0].providers[0].name = "VirtualBox".to_string();
        assert!(!valid_provider_name("VirtualBox"));
        assert!(has_suffix(&validate(&bad_name), ".name"));

        for url in [
            "https://u:p@h/x.box",
            "ftp://downloads.example.com/x.box",
            "https://",
            "https://:8080/x.box",
            "//downloads.example.com/x.box",
        ] {
            let mut catalog = base();
            catalog.versions[0].providers[0].url = url.to_string();
            assert!(
                has_suffix(&validate(&catalog), ".url"),
                "{url:?} should be reported"
            );
        }

        let mut host_from_triple_slash = base();
        host_from_triple_slash.versions[0].providers[0].url = "https:///x.box".to_string();
        assert!(validate(&host_from_triple_slash).is_empty());
    }

    #[test]
    fn rejects_half_and_mismatched_checksums() {
        let mut without_type = base();
        without_type.versions[0].providers[0].checksum_type = String::new();
        assert!(has_suffix(&validate(&without_type), ".checksum_type"));

        let mut without_checksum = base();
        without_checksum.versions[0].providers[0].checksum = String::new();
        assert!(has_suffix(&validate(&without_checksum), ".checksum"));

        let mut wrong_length = base();
        wrong_length.versions[0].providers[0].checksum = "a".repeat(40);
        assert!(has_suffix(&validate(&wrong_length), ".checksum"));

        let mut non_hex = base();
        non_hex.versions[0].providers[0].checksum = "z".repeat(64);
        assert!(has_suffix(&validate(&non_hex), ".checksum"));

        let mut mismatched_type = base();
        mismatched_type.versions[0].providers[0].checksum_type = "sha512".to_string();
        assert!(has_suffix(&validate(&mismatched_type), ".checksum"));

        let mut unknown_type = base();
        unknown_type.versions[0].providers[0].checksum_type = "crc32".to_string();
        assert!(has_suffix(&validate(&unknown_type), ".checksum_type"));
    }

    #[test]
    fn rejects_bad_architecture_and_default_without_architecture() {
        assert!(!valid_architecture("amd 64"));
        let mut bad = base();
        bad.versions[0].providers[0].architecture = "amd 64".to_string();
        assert!(has_suffix(&validate(&bad), ".architecture"));

        let mut legacy_with_default = base();
        legacy_with_default.versions[0].providers[0].architecture = String::new();
        assert!(has_suffix(
            &validate(&legacy_with_default),
            ".default_architecture"
        ));
    }

    #[test]
    fn rejects_duplicate_provider_identity() {
        let mut catalog = base();
        let mut second = provider();
        second.default_architecture = Some(false);
        catalog.versions[0].providers.push(second);
        let errors = validate(&catalog);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].field.starts_with("versions[0].providers[1]"));
    }

    #[test]
    fn rejects_two_defaults_for_one_provider_name() {
        let mut catalog = base();
        let mut second = provider();
        second.architecture = "arm64".to_string();
        catalog.versions[0].providers.push(second);

        let errors = validate(&catalog);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].field.ends_with(".default_architecture"));
    }

    #[test]
    fn accepts_legacy_provider_without_architecture_or_default() {
        let mut catalog = base();
        let provider = &mut catalog.versions[0].providers[0];
        provider.checksum = String::new();
        provider.checksum_type = String::new();
        provider.architecture = String::new();
        provider.default_architecture = None;

        assert!(validate(&catalog).is_empty());
    }
}
