#![allow(dead_code)]

#[path = "../catalog_v1.rs"]
mod catalog_v1;
#[path = "../digest.rs"]
mod digest;
#[path = "../fs_retry.rs"]
mod fs_retry;
#[path = "../manifest.rs"]
mod manifest;
#[path = "../parallel.rs"]
mod parallel;
#[path = "../process.rs"]
mod process;
#[path = "../qa_paths.rs"]
mod qa_paths;
#[path = "../source_v1.rs"]
mod source_v1;
#[path = "../sources.rs"]
mod sources;

use std::fs;
use std::path::Path;

fn main() {
    let input = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: validate-source REPOSITORY-OR-URL");
        std::process::exit(2);
    });
    if input.starts_with("https://") || input.starts_with("ssh://") {
        validate_remote(&input);
    } else {
        match catalog_v1::read_manifest_catalog(Path::new(&input), "validation") {
            Ok(catalog) => report(&catalog),
            Err(error) => fail(&error),
        }
    }
}

fn validate_remote(url: &str) {
    let cache = sources::temporary_path(&std::env::temp_dir(), "skill-manager-validation");
    if let Err(error) = fs::create_dir(&cache) {
        fail(&format!("Could not create {}: {error}", cache.display()));
    }
    let result = source_v1::prepare_new_source(url, &cache);
    match result {
        Ok(candidate) => {
            report(&candidate.catalog);
            source_v1::discard_candidate(&candidate);
        }
        Err(error) => {
            let _ = fs_retry::remove_dir_all(&cache);
            fail(&error);
        }
    }
    let _ = fs_retry::remove_dir_all(&cache);
}

fn report(catalog: &catalog_v1::ManifestCatalog) {
    println!(
        "{}: {} valid install(s), {} catalog error(s)",
        catalog.manifest.source.id,
        catalog.items.len(),
        catalog.errors.len()
    );
    for error in &catalog.errors {
        println!("{}: {}", error.path, error.message);
    }
}

fn fail(error: &str) -> ! {
    eprintln!("{error}");
    std::process::exit(1)
}
