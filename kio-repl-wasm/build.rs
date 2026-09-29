use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

fn main() {
    let manifest_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let poc_dir = manifest_dir.join("../test-data/poc");
    println!("cargo:rerun-if-changed={}", poc_dir.display());

    let mut files = Vec::new();
    collect(&poc_dir, &poc_dir, &mut files).expect("collect embedded POC sources");
    files.sort();

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    let mut generated = String::from("pub const POC_SOURCES: &[(&str, &str)] = &[\n");
    for rel in files {
        println!("cargo:rerun-if-changed={}", poc_dir.join(&rel).display());
        let rel = rel.to_string_lossy().replace('\\', "/");
        generated.push_str("    (");
        generated.push_str(&format!("{rel:?}"));
        generated
            .push_str(", include_str!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/../test-data/poc/");
        generated.push_str(&rel);
        generated.push_str("\"))),\n");
    }
    generated.push_str("];\n");
    fs::write(out_dir.join("poc_sources.rs"), generated).expect("write generated POC source table");
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect(root, &path, out)?;
        } else if is_embedded_kio_source(root, &path) {
            out.push(
                path.strip_prefix(root)
                    .expect("path under root")
                    .to_path_buf(),
            );
        }
    }
    Ok(())
}

fn is_embedded_kio_source(root: &Path, path: &Path) -> bool {
    if path.extension().and_then(|s| s.to_str()) != Some("kio") {
        return false;
    }

    let Ok(rel) = path.strip_prefix(root) else {
        return false;
    };
    let components: Vec<_> = rel
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect();

    if components.len() < 3 || components[1] != "workdir" {
        return false;
    }

    !components.iter().any(|component| {
        *component == "out" || component.starts_with('.') || *component == "node_modules"
    })
}
