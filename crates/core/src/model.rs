use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    pub versions: Vec<Version>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Version {
    pub version: String,
    pub providers: Vec<Provider>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provider {
    pub name: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub checksum: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub checksum_type: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub architecture: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_architecture: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::Catalog;

    #[test]
    fn parses_document_without_optional_fields() {
        let catalog: Catalog = serde_json::from_str(
            r#"{"name":"acme/base","versions":[{"version":"1.0.0","providers":[{"name":"virtualbox","url":"https://example.com/x.box"}]}]}"#,
        )
        .expect("optional fields are omitted");

        let provider = &catalog.versions[0].providers[0];
        assert!(catalog.description.is_empty());
        assert!(provider.checksum.is_empty());
        assert!(provider.checksum_type.is_empty());
        assert!(provider.architecture.is_empty());
        assert_eq!(provider.default_architecture, None);
    }

    #[test]
    fn rejects_unknown_fields() {
        let result =
            serde_json::from_str::<Catalog>(r#"{"name":"acme/base","versions":[],"extra":1}"#);
        assert!(result.is_err());
    }

    #[test]
    fn serialization_omits_empty_optional_fields() {
        let catalog = Catalog {
            name: "acme/base".to_string(),
            description: String::new(),
            versions: vec![],
        };

        assert_eq!(
            serde_json::to_string(&catalog).unwrap(),
            r#"{"name":"acme/base","versions":[]}"#
        );
    }
}
