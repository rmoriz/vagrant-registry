#[path = "src/model.rs"]
mod model;
#[path = "src/render.rs"]
mod render;
#[path = "src/tree.rs"]
mod tree;
#[path = "src/validate.rs"]
mod validate;
#[path = "src/version.rs"]
mod version;

use std::env;
use std::path::Path;

use tree::{LoadError, load_dir, to_table};

fn main() {
    println!("cargo:rerun-if-changed=../../data");

    // `rerun-if-changed=../../data` turns off Cargo's default tracking of the
    // package's own files, so the sources pulled in through `#[path]` above
    // (the accepted self-dependency deviation) need hooks of their own.
    for source in [
        "model.rs",
        "render.rs",
        "tree.rs",
        "validate.rs",
        "version.rs",
    ] {
        println!("cargo:rerun-if-changed=src/{source}");
    }

    let data_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data");

    let catalogs = match load_dir(&data_dir) {
        Ok(catalogs) => catalogs,
        Err(errors) => panic!("{}", report(&errors)),
    };

    let table = match to_table(&catalogs) {
        Ok(table) => table,
        Err(errors) => panic!("{}", report(&errors)),
    };

    let out_dir = env::var("OUT_DIR").expect("OUT_DIR");
    std::fs::write(Path::new(&out_dir).join("catalogs.rs"), table).expect("writing catalogs.rs");
}

fn report(errors: &[LoadError]) -> String {
    errors
        .iter()
        .flat_map(|load_error| {
            load_error.errors.iter().map(|error| {
                format!(
                    "{}: {}: {}",
                    label(&load_error.file),
                    error.field,
                    error.message
                )
            })
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Renders a file path relative to the repository root, so a report names
/// `data/acme/base.json` rather than `acme/base.json`. Bare catalog names from
/// `to_table` errors and absolute paths are left as they are.
fn label(file: &str) -> String {
    if Path::new(file).is_absolute() || !file.ends_with(".json") {
        file.to_string()
    } else {
        format!("data/{file}")
    }
}
