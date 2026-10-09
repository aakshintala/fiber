//! Maps each provider a first-party package declares to that package's
//! short name (`docs/extensions.md`, "Names"), so `extension_missing` can
//! name the extension to install.

#![allow(
    clippy::print_stdout,
    reason = "cargo reads build-script instructions from stdout"
)]

use std::env;
use std::fs;
use std::io;
use std::path::PathBuf;

fn read_error(path: &std::path::Path, error: io::Error) -> io::Error {
    io::Error::other(format!("{}: {error}", path.display()))
}

fn main() -> io::Result<()> {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").map_err(io::Error::other)?;
    let out_dir = env::var("OUT_DIR").map_err(io::Error::other)?;
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../providers");
    let root = PathBuf::from(manifest_dir).join("../../providers");
    let mut package_dirs = Vec::new();
    for entry in fs::read_dir(&root).map_err(|error| read_error(&root, error))? {
        let entry = entry.map_err(|error| read_error(&root, error))?;
        package_dirs.push(entry.path());
    }
    package_dirs.sort();
    let mut pairs: Vec<(String, String)> = Vec::new();
    for package_dir in package_dirs {
        if !package_dir.is_dir() {
            continue;
        }
        let Some(package) = package_dir.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let providers_dir = package_dir.join("providers");
        let entries = match fs::read_dir(&providers_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(read_error(&providers_dir, error)),
        };
        let mut files = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| read_error(&providers_dir, error))?;
            files.push(entry.path());
        }
        files.sort();
        for file in files {
            if file.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let Some(stem) = file.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            pairs.push((stem.to_owned(), package.to_owned()));
        }
    }
    pairs.sort();
    let mut table = String::from("&[");
    for (provider, package) in &pairs {
        table.push_str(&format!("({provider:?}, {package:?}), "));
    }
    table.push(']');
    let out = PathBuf::from(out_dir).join("provider_packages.rs");
    fs::write(&out, table).map_err(|error| read_error(&out, error))
}
