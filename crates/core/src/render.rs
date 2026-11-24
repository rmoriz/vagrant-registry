use crate::model::Catalog;
use crate::validate::{ValidationError, validate};

#[derive(Debug)]
pub enum RenderError {
    Invalid(Vec<ValidationError>),
    Json(serde_json::Error),
}

/// Returns `c` with `default_architecture` normalized: providers without an
/// architecture never carry the flag, and architected providers default to
/// `false` unless they already state `true` or `false`.
pub fn normalize(catalog: &Catalog) -> Catalog {
    let mut catalog = catalog.clone();

    for version in &mut catalog.versions {
        for provider in &mut version.providers {
            provider.default_architecture = if provider.architecture.is_empty() {
                None
            } else {
                Some(provider.default_architecture.unwrap_or(false))
            };
        }
    }

    catalog
}

/// Renders `catalog` as the canonical registry JSON: two-space pretty output
/// with a single trailing newline. Invalid catalogs render nothing.
pub fn to_json(catalog: &Catalog) -> Result<String, RenderError> {
    let errors = validate(catalog);
    if !errors.is_empty() {
        return Err(RenderError::Invalid(errors));
    }

    let mut json = serde_json::to_string_pretty(&normalize(catalog)).map_err(RenderError::Json)?;
    json.push('\n');
    Ok(json)
}

#[cfg(test)]
mod tests {
    use super::{RenderError, to_json};
    use crate::model::{Catalog, Provider, Version};

    const CHECKSUM: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn provider(architecture: &str, default: Option<bool>) -> Provider {
        Provider {
            name: "virtualbox".to_string(),
            url: "https://downloads.example.com/acme-base-1.0.0-vbox-amd64.box".to_string(),
            checksum: CHECKSUM.to_string(),
            checksum_type: "sha256".to_string(),
            architecture: architecture.to_string(),
            default_architecture: default,
        }
    }

    fn catalog(architecture: &str, default: Option<bool>) -> Catalog {
        Catalog {
            name: "acme/base".to_string(),
            description: "Acme base image".to_string(),
            versions: vec![Version {
                version: "1.0.0".to_string(),
                providers: vec![provider(architecture, default)],
            }],
        }
    }

    #[test]
    fn renders_two_spaces_and_trailing_newline() {
        let expected = format!(
            "{{\n  \"name\": \"acme/base\",\n  \"description\": \"Acme base image\",\n  \"versions\": [\n    {{\n      \"version\": \"1.0.0\",\n      \"providers\": [\n        {{\n          \"name\": \"virtualbox\",\n          \"url\": \"https://downloads.example.com/acme-base-1.0.0-vbox-amd64.box\",\n          \"checksum\": \"{CHECKSUM}\",\n          \"checksum_type\": \"sha256\",\n          \"architecture\": \"amd64\",\n          \"default_architecture\": true\n        }}\n      ]\n    }}\n  ]\n}}\n"
        );

        assert_eq!(to_json(&catalog("amd64", Some(true))).unwrap(), expected);
    }

    #[test]
    fn emits_false_when_architecture_has_no_default() {
        let json = to_json(&catalog("amd64", None)).unwrap();

        assert!(json.contains("\"default_architecture\": false"), "{json}");
    }

    #[test]
    fn emits_true_for_default_architecture() {
        let json = to_json(&catalog("amd64", Some(true))).unwrap();

        assert!(json.contains("\"default_architecture\": true"), "{json}");
    }

    #[test]
    fn omits_default_architecture_for_legacy_provider() {
        let json = to_json(&catalog("", None)).unwrap();

        assert!(!json.contains("architecture"), "{json}");
    }

    #[test]
    fn omits_empty_description_and_checksums() {
        let mut catalog = catalog("amd64", Some(true));
        catalog.description = String::new();
        let provider = &mut catalog.versions[0].providers[0];
        provider.checksum = String::new();
        provider.checksum_type = String::new();

        let json = to_json(&catalog).unwrap();

        assert!(!json.contains("description"), "{json}");
        assert!(!json.contains("checksum"), "{json}");
    }

    #[test]
    fn rejects_invalid_catalog() {
        let mut catalog = catalog("amd64", Some(true));
        catalog.name = "acme".to_string();

        match to_json(&catalog) {
            Err(RenderError::Invalid(errors)) => {
                assert!(errors.iter().any(|e| e.field == "name"), "{errors:?}");
            }
            other => panic!("expected RenderError::Invalid, got {other:?}"),
        }
    }
}
