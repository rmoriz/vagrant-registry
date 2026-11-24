use std::path::PathBuf;

use vagrant_registry_core::catalogs::CATALOGS;
use vagrant_registry_core::tree::load_dir;

#[test]
fn embedded_table_matches_the_data_directory() {
    let data_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data");
    let entries = load_dir(&data_dir).expect("data/ must load");
    assert!(
        !entries.is_empty(),
        "data/ must contain at least one catalog"
    );

    for (name, _) in &entries {
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
    }

    assert_eq!(CATALOGS.len(), entries.len());
}
