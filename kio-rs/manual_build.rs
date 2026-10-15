#[path = "manual_pack.rs"]
mod manual_pack;

pub fn prepare(core_root: &std::path::Path) {
    use std::{env, fs, path::PathBuf, process::Command};
    if env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("wasm32") {
        return;
    }
    println!(
        "cargo:rerun-if-changed={}",
        core_root.join("manual_build.rs").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        core_root.join("manual_pack.rs").display()
    );
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("build output directory"));
    let root = core_root.parent().expect("crate parent");
    if manual_pack::is_checkout(core_root) {
        for path in ["docs", "specs", "KIO-FOR-AGENTS.md", "TERMINOLOGY.md"] {
            println!("cargo:rerun-if-changed={}", root.join(path).display());
        }
        let git_dir = Command::new("git")
            .current_dir(root)
            .args(["rev-parse", "--absolute-git-dir"])
            .output()
            .expect("locate manual inventory index");
        assert!(git_dir.status.success(), "locate manual inventory index");
        println!(
            "cargo:rerun-if-changed={}/index",
            String::from_utf8_lossy(&git_dir.stdout).trim()
        );
        let bundle = manual_pack::assemble(root).expect("assemble bundled manual");
        manual_pack::snapshot(&bundle, &output, false).expect("write bundled manual");
    } else {
        let directory = core_root.join("manual");
        let bundle = manual_pack::Bundle {
            index: fs::read_to_string(directory.join("index.tsv"))
                .expect("read packaged manual index"),
            payload: fs::read(directory.join("payload.tar.gz-concat"))
                .expect("read packaged manual payload"),
        };
        manual_pack::validate(&bundle).expect("validate packaged manual snapshot");
        for name in ["index.tsv", "payload.tar.gz-concat"] {
            let source = core_root.join("manual").join(name);
            println!("cargo:rerun-if-changed={}", source.display());
            fs::copy(source, output.join(name)).expect("copy packaged manual");
        }
    }
}
