#[path = "../manual_build.rs"]
mod manual_build;

fn main() {
    let manifest = std::path::PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"),
    );
    manual_build::prepare(manifest.parent().expect("compiler crate root"));
}
