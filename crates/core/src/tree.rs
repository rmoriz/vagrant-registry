use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::model::Catalog;
use crate::render::{RenderError, to_json};
use crate::validate::{ValidationError, validate};

/// Largest rendered document accepted by [`load_dir`].
pub const MAX_DOCUMENT_BYTES: usize = 1_048_576;
/// Largest rendered total across the whole tree accepted by [`load_dir`].
pub const MAX_TOTAL_BYTES: usize = 16_777_216;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadError {
    pub file: String,
    pub errors: Vec<ValidationError>,
}

impl LoadError {
    fn single(
        file: impl Into<String>,
        field: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            file: file.into(),
            errors: vec![error(field, message)],
        }
    }
}

/// Loads every `*.json` file below `root` into a validated catalog, keyed by
/// its path without the extension, and returns them sorted by name.
///
/// All failures are collected; a non-empty error list means no catalog was
/// accepted. Cap checks run on the rendered bytes, per document and in total.
pub fn load_dir(root: &Path) -> Result<Vec<(String, Catalog)>, Vec<LoadError>> {
    let mut files = Vec::new();
    let mut errors = Vec::new();
    let root_readable = collect_files(root, root, &mut files, &mut errors);
    files.sort();

    let mut entries = Vec::new();
    let mut seen: HashMap<String, String> = HashMap::new();
    let mut total_bytes = 0usize;

    for path in &files {
        let file = relative_display(root, path);
        let expected = expected_name(&file);
        let mut file_errors = Vec::new();

        let catalog = match fs::read(path) {
            Ok(bytes) => match serde_json::from_slice::<Catalog>(&bytes) {
                Ok(catalog) => Some(catalog),
                Err(err) => {
                    file_errors.push(error("json", err.to_string()));
                    None
                }
            },
            Err(err) => {
                file_errors.push(error("io", err.to_string()));
                None
            }
        };

        if let Some(catalog) = catalog {
            if let Some(previous) = seen.insert(catalog.name.clone(), file.clone()) {
                file_errors.push(error(
                    "name",
                    format!("duplicate name, also defined in {previous:?}"),
                ));
            }

            if catalog.name != expected {
                file_errors.push(error(
                    "name",
                    format!("must match the file path {expected:?}"),
                ));
            }

            file_errors.extend(validate(&catalog));

            match to_json(&catalog) {
                Ok(json) => {
                    total_bytes += json.len();
                    if json.len() > MAX_DOCUMENT_BYTES {
                        file_errors.push(error(
                            "rendered_bytes",
                            format!(
                                "must be at most {MAX_DOCUMENT_BYTES} bytes, rendered {}",
                                json.len()
                            ),
                        ));
                    }
                }
                Err(RenderError::Invalid(invalid)) => {
                    for invalid_error in invalid {
                        if !file_errors.contains(&invalid_error) {
                            file_errors.push(invalid_error);
                        }
                    }
                }
                Err(RenderError::Json(err)) => file_errors.push(error("json", err.to_string())),
            }

            if file_errors.is_empty() {
                entries.push((expected, catalog));
            }
        }

        if !file_errors.is_empty() {
            errors.push(LoadError {
                file,
                errors: file_errors,
            });
        }
    }

    if files.is_empty() && root_readable {
        errors.push(LoadError::single(
            root.display().to_string(),
            "data",
            "must contain at least one .json catalog file",
        ));
    }

    if total_bytes > MAX_TOTAL_BYTES {
        errors.push(LoadError::single(
            root.display().to_string(),
            "total_bytes",
            format!("must be at most {MAX_TOTAL_BYTES} bytes, rendered {total_bytes}"),
        ));
    }

    if !errors.is_empty() {
        errors.sort_by(|left, right| left.file.cmp(&right.file));
        return Err(errors);
    }

    entries.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(entries)
}

/// Renders `catalogs` as the Rust source of the `CATALOGS` byte table and the
/// `INDEX` summary table, both sorted by name.
pub fn to_table(catalogs: &[(String, Catalog)]) -> Result<String, Vec<LoadError>> {
    let mut sorted: Vec<&(String, Catalog)> = catalogs.iter().collect();
    sorted.sort_by(|left, right| left.0.cmp(&right.0));

    let mut errors = Vec::new();
    let mut literals = Vec::new();
    let mut index = Vec::new();

    for (name, catalog) in sorted {
        match to_json(catalog) {
            Ok(json) => {
                literals.push(format!(
                    "(\"{}\", b\"{}\")",
                    escape_str(name),
                    escape_bytes(json.as_bytes())
                ));
                index.push(format!(
                    "(\"{}\", \"{}\", {})",
                    escape_str(name),
                    escape_str(&catalog.description),
                    catalog.versions.len()
                ));
            }
            Err(RenderError::Invalid(invalid)) => errors.push(LoadError {
                file: name.clone(),
                errors: invalid,
            }),
            Err(RenderError::Json(err)) => {
                errors.push(LoadError::single(name.clone(), "json", err.to_string()));
            }
        }
    }

    if !errors.is_empty() {
        errors.sort_by(|left, right| left.file.cmp(&right.file));
        return Err(errors);
    }

    Ok(format!(
        "pub static CATALOGS: &[(&str, &[u8])] = &[{}];\npub static INDEX: &[(&str, &str, usize)] = &[{}];\n",
        literals.join(", "),
        index.join(", ")
    ))
}

fn error(field: impl Into<String>, message: impl Into<String>) -> ValidationError {
    ValidationError {
        field: field.into(),
        message: message.into(),
    }
}

/// Collects `*.json` files below `dir` and returns whether `dir` itself was
/// readable. Unreadable subdirectories are recorded as errors and do not stop
/// the walk.
fn collect_files(
    root: &Path,
    dir: &Path,
    files: &mut Vec<PathBuf>,
    errors: &mut Vec<LoadError>,
) -> bool {
    let dir_display = relative_display(root, dir);

    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) => {
            errors.push(LoadError::single(dir_display, "io", err.to_string()));
            return false;
        }
    };

    for entry in entries {
        match entry {
            Ok(entry) => {
                let path = entry.path();
                if path.is_dir() {
                    collect_files(root, &path, files, errors);
                } else if is_json(&path) {
                    files.push(path);
                }
            }
            Err(err) => errors.push(LoadError::single(&dir_display, "io", err.to_string())),
        }
    }

    true
}

fn is_json(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
}

fn relative_display(root: &Path, path: &Path) -> String {
    match path.strip_prefix(root) {
        Ok(relative) if !relative.as_os_str().is_empty() => {
            relative.to_string_lossy().replace('\\', "/")
        }
        _ => path.display().to_string(),
    }
}

fn expected_name(file: &str) -> String {
    debug_assert!(
        file.len() >= 5 && file.is_char_boundary(file.len() - 5),
        "{file:?} must end with a .json suffix"
    );

    match file.len().checked_sub(5) {
        Some(index) if file[index..].eq_ignore_ascii_case(".json") => file[..index].to_string(),
        _ => file.to_string(),
    }
}

fn escape_bytes(bytes: &[u8]) -> String {
    let mut escaped = String::with_capacity(bytes.len());

    for &byte in bytes {
        match byte {
            b'\\' => escaped.push_str("\\\\"),
            b'"' => escaped.push_str("\\\""),
            b'\n' => escaped.push_str("\\n"),
            b'\r' => escaped.push_str("\\r"),
            b'\t' => escaped.push_str("\\t"),
            0x20..=0x7e => escaped.push(byte as char),
            _ => escaped.push_str(&format!("\\x{byte:02x}")),
        }
    }

    escaped
}

fn escape_str(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());

    for character in text.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character if character.is_control() => {
                escaped.push_str(&format!("\\u{{{:x}}}", u32::from(character)));
            }
            character => escaped.push(character),
        }
    }

    escaped
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    use tempfile::TempDir;

    use super::{LoadError, MAX_DOCUMENT_BYTES, MAX_TOTAL_BYTES, load_dir, to_table};
    use crate::model::{Catalog, Provider, Version};
    use crate::render::to_json;

    const CHECKSUM: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn write(root: &Path, relative: &str, contents: &str) {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }

    fn write_catalog(root: &Path, relative: &str, catalog: &Catalog) {
        write(root, relative, &serde_json::to_string(catalog).unwrap());
    }

    fn document(name: &str, description: &str) -> String {
        let name = serde_json::to_string(name).unwrap();
        let description = serde_json::to_string(description).unwrap();
        format!(
            r#"{{"name":{name},"description":{description},"versions":[{{"version":"1.0.0","providers":[{{"name":"virtualbox","url":"https://downloads.example.com/acme-base-1.0.0-vbox-amd64.box","checksum":"{CHECKSUM}","checksum_type":"sha256","architecture":"amd64","default_architecture":true}}]}}]}}"#
        )
    }

    fn valid_document(name: &str) -> String {
        document(name, "Acme base image")
    }

    fn load_errors(root: &Path) -> Vec<LoadError> {
        load_dir(root).expect_err("loading must fail")
    }

    fn fields(errors: &[LoadError]) -> Vec<&str> {
        errors
            .iter()
            .flat_map(|load_error| load_error.errors.iter().map(|error| error.field.as_str()))
            .collect()
    }

    fn fixture(name: &str, versions: usize) -> Catalog {
        Catalog {
            name: name.to_string(),
            description: "Acme base image".to_string(),
            versions: (0..versions)
                .map(|version| Version {
                    version: format!("1.0.{version}"),
                    providers: (0..100)
                        .map(|provider| Provider {
                            name: format!("p{provider}"),
                            url: format!(
                                "https://downloads.example.com/{}/{provider}.box",
                                "x".repeat(160)
                            ),
                            checksum: CHECKSUM.to_string(),
                            checksum_type: "sha256".to_string(),
                            architecture: format!("a{provider}"),
                            default_architecture: Some(provider == 0),
                        })
                        .collect(),
                })
                .collect(),
        }
    }

    fn rendered_len(catalog: &Catalog) -> usize {
        to_json(catalog).unwrap().len()
    }

    /// Smallest power-of-two version count whose rendering exceeds `limit`.
    fn fixture_above(limit: usize) -> Catalog {
        let mut versions = 1;
        loop {
            let catalog = fixture("acme/base", versions);
            if rendered_len(&catalog) > limit {
                return catalog;
            }
            versions *= 2;
        }
    }

    /// Largest power-of-two version count whose rendering stays below `limit`.
    fn fixture_below(limit: usize) -> Catalog {
        let mut versions = 1;
        loop {
            let next = versions * 2;
            if rendered_len(&fixture("acme/base", next)) > limit {
                return fixture("acme/base", versions);
            }
            versions = next;
        }
    }

    /// Pads one provider URL so the rendering is exactly `MAX_DOCUMENT_BYTES`.
    fn pad_to_cap(mut catalog: Catalog) -> Catalog {
        let delta = MAX_DOCUMENT_BYTES - rendered_len(&catalog);
        catalog.versions[0].providers[0]
            .url
            .push_str(&"x".repeat(delta));
        assert_eq!(rendered_len(&catalog), MAX_DOCUMENT_BYTES);
        catalog
    }

    #[test]
    fn rejects_empty_root_with_data_field() {
        let dir = TempDir::new().unwrap();

        let errors = load_errors(dir.path());

        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(fields(&errors), ["data"]);
        assert_eq!(errors[0].file, dir.path().display().to_string());
    }

    #[test]
    fn reports_missing_root_as_one_io_error() {
        let dir = TempDir::new().unwrap();
        let missing = dir.path().join("missing");

        let errors = load_errors(&missing);

        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(fields(&errors), ["io"]);
        assert_eq!(errors[0].file, missing.display().to_string());
    }

    #[test]
    fn ignores_files_with_other_extensions() {
        let dir = TempDir::new().unwrap();
        write(dir.path(), "acme/base.txt", "not json at all");
        write(dir.path(), "acme/notes.md", "# notes");

        let errors = load_errors(dir.path());

        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(fields(&errors), ["data"]);
    }

    #[test]
    fn reports_malformed_json_naming_the_file() {
        let dir = TempDir::new().unwrap();
        write(dir.path(), "acme/base.json", "{not json");

        let errors = load_errors(dir.path());

        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].file, "acme/base.json");
        assert_eq!(fields(&errors), ["json"]);
    }

    #[test]
    fn rejects_numeric_version() {
        let dir = TempDir::new().unwrap();
        write(
            dir.path(),
            "acme/base.json",
            r#"{"name":"acme/base","versions":[{"version":1.0,"providers":[]}]}"#,
        );

        let errors = load_errors(dir.path());

        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].file, "acme/base.json");
        assert_eq!(fields(&errors), ["json"]);
    }

    #[test]
    fn rejects_unknown_top_level_field() {
        let dir = TempDir::new().unwrap();
        write(
            dir.path(),
            "acme/base.json",
            r#"{"name":"acme/base","versions":[],"extra":1}"#,
        );

        let errors = load_errors(dir.path());

        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].file, "acme/base.json");
        assert_eq!(fields(&errors), ["json"]);
    }

    #[test]
    fn rejects_name_that_does_not_match_the_path() {
        let dir = TempDir::new().unwrap();
        write(dir.path(), "acme/base.json", &valid_document("acme/other"));

        let errors = load_errors(dir.path());

        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].file, "acme/base.json");
        assert_eq!(fields(&errors), ["name"]);
    }

    #[test]
    fn rejects_duplicate_names() {
        // A path yields exactly one name, so a duplicate only arises when a
        // second file claims a name that already belongs to another path.
        let dir = TempDir::new().unwrap();
        write(dir.path(), "acme/base.json", &valid_document("acme/base"));
        write(dir.path(), "acme/copy.json", &valid_document("acme/base"));

        let errors = load_errors(dir.path());

        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].file, "acme/copy.json");
        assert_eq!(fields(&errors), ["name", "name"]);
        assert!(
            errors[0]
                .errors
                .iter()
                .any(|error| error.message.contains("duplicate")),
            "{errors:?}"
        );
    }

    #[test]
    fn rejects_dot_and_dotdot_segments() {
        let dir = TempDir::new().unwrap();
        write(dir.path(), "acme/..json", &valid_document("acme/."));
        write(dir.path(), "acme/...json", &valid_document("acme/.."));

        let errors = load_errors(dir.path());

        assert_eq!(errors.len(), 2, "{errors:?}");
        assert_eq!(fields(&errors), ["name", "name"]);
        assert_eq!(errors[0].file, "acme/...json");
        assert_eq!(errors[1].file, "acme/..json");
    }

    #[test]
    fn sorts_errors_by_file() {
        let dir = TempDir::new().unwrap();
        write(dir.path(), "acme/zeta.json", "{");
        write(dir.path(), "acme/alpha.json", "{");

        let errors = load_errors(dir.path());

        assert_eq!(errors.len(), 2, "{errors:?}");
        assert_eq!(errors[0].file, "acme/alpha.json");
        assert_eq!(errors[1].file, "acme/zeta.json");
    }

    #[test]
    fn rejects_document_over_rendered_cap() {
        let dir = TempDir::new().unwrap();
        let catalog = fixture_above(MAX_DOCUMENT_BYTES);
        assert!(rendered_len(&catalog) > MAX_DOCUMENT_BYTES);
        write_catalog(dir.path(), "acme/base.json", &catalog);

        let errors = load_errors(dir.path());

        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].file, "acme/base.json");
        assert_eq!(fields(&errors), ["rendered_bytes"]);
    }

    #[test]
    fn rejects_total_over_cap_with_one_error() {
        let dir = TempDir::new().unwrap();
        let catalog = fixture_below(MAX_DOCUMENT_BYTES);
        let per_file = rendered_len(&catalog);
        assert!(per_file < MAX_DOCUMENT_BYTES, "{per_file}");
        let files = MAX_TOTAL_BYTES / per_file + 2;

        for index in 0..files {
            let name = format!("acme/box{index}");
            let mut catalog = catalog.clone();
            catalog.name = name.clone();
            write_catalog(dir.path(), &format!("{name}.json"), &catalog);
        }

        let errors = load_errors(dir.path());

        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(fields(&errors), ["total_bytes"]);
        assert_eq!(errors[0].file, dir.path().display().to_string());
    }

    #[test]
    fn accepts_document_exactly_at_rendered_cap() {
        let dir = TempDir::new().unwrap();
        let catalog = pad_to_cap(fixture_below(MAX_DOCUMENT_BYTES));
        assert_eq!(rendered_len(&catalog), MAX_DOCUMENT_BYTES);
        write_catalog(dir.path(), "acme/base.json", &catalog);

        let entries = load_dir(dir.path()).unwrap();

        assert_eq!(entries.len(), 1, "{entries:?}");
        assert_eq!(entries[0].0, "acme/base");
    }

    #[test]
    fn accepts_tree_exactly_at_total_cap() {
        let dir = TempDir::new().unwrap();
        let base = fixture_below(MAX_DOCUMENT_BYTES);
        let mut total = 0;
        let files = MAX_TOTAL_BYTES / MAX_DOCUMENT_BYTES;

        for index in 0..files {
            let name = format!("acme/box{index}");
            let mut catalog = base.clone();
            catalog.name = name.clone();
            let catalog = pad_to_cap(catalog);
            total += rendered_len(&catalog);
            write_catalog(dir.path(), &format!("{name}.json"), &catalog);
        }

        assert_eq!(total, MAX_TOTAL_BYTES);

        let entries = load_dir(dir.path()).unwrap();

        assert_eq!(entries.len(), files, "{entries:?}");
    }

    #[test]
    fn loads_valid_tree_sorted_by_name() {
        let dir = TempDir::new().unwrap();
        write(dir.path(), "acme/zeta.json", &valid_document("acme/zeta"));
        write(dir.path(), "beta/alpha.json", &valid_document("beta/alpha"));
        write(dir.path(), "acme/alpha.json", &valid_document("acme/alpha"));

        let entries = load_dir(dir.path()).unwrap();

        let names: Vec<&str> = entries.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["acme/alpha", "acme/zeta", "beta/alpha"]);
        assert_eq!(entries[0].1.description, "Acme base image");
    }

    fn table_catalog(name: &str, description: &str, versions: usize) -> Catalog {
        let mut catalog = fixture(name, versions);
        catalog.description = description.to_string();
        catalog
    }

    #[test]
    fn to_table_emits_sorted_and_escaped_literals() {
        let catalogs = [
            (
                "acme/zeta".to_string(),
                table_catalog("acme/zeta", r#"Acme "zeta" \ path"#, 1),
            ),
            (
                "acme/alpha".to_string(),
                table_catalog("acme/alpha", r#"Acme "alpha" \ path"#, 2),
            ),
        ];

        let table = to_table(&catalogs).unwrap();

        assert!(table.starts_with("pub static CATALOGS: &[(&str, &[u8])] = &["));
        assert!(table.contains("\npub static INDEX: &[(&str, &str, usize)] = &["));
        assert!(table.contains(r#"b"{"#));
        assert!(
            table.find("acme/alpha").unwrap() < table.find("acme/zeta").unwrap(),
            "{table}"
        );
        assert!(
            table.contains(r#"\"description\": \"Acme \\\"alpha\\\" \\\\ path\""#),
            "{table}"
        );
        assert!(
            table.contains(r#"("acme/alpha", "Acme \"alpha\" \\ path", 2)"#),
            "{table}"
        );
    }

    #[test]
    fn to_table_reports_invalid_catalogs() {
        let mut catalog = fixture("acme", 1);
        catalog.name = "acme".to_string();

        let errors = to_table(&[("acme".to_string(), catalog)]).unwrap_err();

        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].file, "acme");
        assert!(errors[0].errors.iter().any(|error| error.field == "name"));
    }

    #[test]
    fn to_table_output_compiles_as_rust() {
        let dir = TempDir::new().unwrap();
        write(dir.path(), "acme/base.json", &valid_document("acme/base"));
        write(
            dir.path(),
            "acme/quoted.json",
            &document(
                "acme/quoted",
                "a \"quoted\" \\ path\nsecond line\nGrüße – ok\u{1}",
            ),
        );

        let catalogs = load_dir(dir.path()).unwrap();
        let table = to_table(&catalogs).unwrap();
        assert!(table.contains("Gr\\xc3\\xbc"), "{table}");
        assert!(table.contains("\\u{1}"), "{table}");
        let source = dir.path().join("catalogs.rs");
        fs::write(&source, table).unwrap();

        let output = Command::new(std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string()))
            .args(["--crate-type", "lib", "--edition", "2024", "-o"])
            .arg(dir.path().join("libcatalogs.rlib"))
            .arg(&source)
            .output()
            .expect("rustc runs");

        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
