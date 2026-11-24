use std::path::PathBuf;

use vagrant_registry_core::catalogs::{CATALOGS, INDEX};
use vagrant_registry_core::render::to_json;
use vagrant_registry_core::tree::load_dir;

#[test]
fn embedded_table_matches_the_data_directory() {
    let data_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data");
    let entries = load_dir(&data_dir).expect("data/ must load");
    assert!(
        !entries.is_empty(),
        "data/ must contain at least one catalog"
    );

    for (name, catalog) in &entries {
        assert!(
            data_dir.join(format!("{name}.json")).is_file(),
            "{name} must match a file below data/"
        );
        assert_eq!(
            CATALOGS
                .iter()
                .filter(|(candidate, _)| *candidate == name)
                .count(),
            1,
            "{name} must appear in CATALOGS exactly once"
        );

        let (_, bytes) = CATALOGS
            .iter()
            .find(|(candidate, _)| *candidate == name)
            .expect("entry must exist");
        assert_eq!(*bytes, to_json(catalog).unwrap().as_bytes());
    }

    assert_eq!(CATALOGS.len(), entries.len());

    assert_eq!(INDEX.len(), entries.len());
    for (index_name, _, versions) in INDEX {
        let (_, catalog) = entries
            .iter()
            .find(|(name, _)| name.as_str() == *index_name)
            .expect("INDEX entry must match a loaded catalog");
        assert_eq!(*versions, catalog.versions.len());
    }
}
