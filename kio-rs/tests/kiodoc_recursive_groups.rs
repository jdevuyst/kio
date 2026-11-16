//! Recursive declaration context through the real documentation CLI.

#![cfg(all(feature = "surface", feature = "cli"))]

mod support;

use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use support::test_binary;

use tempfile::TempDir;

const GROUP: &str =
    "rec {\n  pub type Chain = Node;\n  pub type { Node: . | Chain { pub mk; pub get } }\n}";

fn package(source: &str) -> TempDir {
    let dir = TempDir::new().expect("create documentation package");
    fs::write(
        dir.path().join("pkg.pkg.kio"),
        "pkg pkg;\nbuild { cache (); docs { md \".\"; } }\nbridge { pkg; }\n",
    )
    .unwrap();
    fs::write(dir.path().join("pkg.kio"), source).unwrap();
    dir
}

fn doc(dir: &Path, args: &[&str]) -> Output {
    Command::new(test_binary!("kio"))
        .arg("doc")
        .args(args)
        .current_dir(dir)
        .env("NO_COLOR", "1")
        .output()
        .expect("run documentation CLI")
}

fn success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn plain_html(source: &str) -> String {
    let mut inside = false;
    source
        .chars()
        .filter(|ch| match ch {
            '<' => {
                inside = true;
                false
            }
            '>' => {
                inside = false;
                false
            }
            _ => !inside,
        })
        .collect::<String>()
        .replace("&gt;", ">")
        .replace("&lt;", "<")
        .replace("&amp;", "&")
}

#[test]
fn kiodoc_recursive_group_site_and_directives_keep_complete_context() {
    let source = "mod pkg;\n\n\
        /// Group prose.\n\
        rec {\n\
          /// Alias prose. [`@signature Node`]\n\
          pub type Chain = Node;\n\
          /// Nominal prose. [`@source Chain`]\n\
          pub type { Node: . | Chain { pub mk; pub get; }; };\n\
        }\n";
    let dir = package(source);
    success(&doc(dir.path(), &["build", "--html", "--md"]));
    let mut copied = None;
    for path in ["out/docs/pkg.html", "out/docs-md/pkg.md"] {
        let rendered = fs::read_to_string(dir.path().join(path)).unwrap();
        let plain = plain_html(&rendered);
        let normalized = plain.split_whitespace().collect::<Vec<_>>().join(" ");
        let normalized_group = GROUP.split_whitespace().collect::<Vec<_>>().join(" ");
        assert_eq!(
            normalized.matches(&normalized_group).count(),
            4,
            "{path}: {plain}"
        );
        assert_eq!(plain.matches("Group prose.").count(), 1, "{plain}");
        assert!(plain.contains("Recursive group"), "{plain}");
        assert!(rendered.contains("id=\"item-pkg-Chain\""), "{rendered}");
        assert!(rendered.contains("id=\"item-pkg-Node\""), "{rendered}");
        assert!(plain.find("Group prose.") < plain.find("Alias prose."));
        copied = plain
            .find(GROUP)
            .map(|start| plain[start..start + GROUP.len()].to_owned());
    }
    fs::write(
        dir.path().join("copied.kio"),
        format!(
            "mod copied;\n{}\n",
            copied.expect("rendered recursive declaration")
        ),
    )
    .unwrap();
    let checked = Command::new(test_binary!("kio"))
        .args(["check", "copied.kio"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    success(&checked);
}

#[test]
fn kiodoc_recursive_group_outer_and_nominal_docs_are_validated() {
    let mut escaped = Vec::new();
    for (label, outer, member) in [
        ("outer reference", "/// [`Missing_outer`]\n", ""),
        ("nominal reference", "", "/// [`Missing_member`]\n"),
        (
            "outer fence",
            "/// ```kio {@}\n/// missing()\n/// ```\n",
            "",
        ),
        (
            "nominal fence",
            "",
            "/// ```kio {@}\n/// missing()\n/// ```\n",
        ),
    ] {
        let dir = package(&format!(
            "mod pkg;\n{outer}rec {{\ntype Chain = Node;\n{member}type {{ Node: . | Chain {{ mk; get; }}; }};\n}}\n"
        ));
        let output = doc(dir.path(), &["check"]);
        let diagnostics = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if output.status.success()
            || !(diagnostics.contains("Missing_") || diagnostics.contains("missing"))
        {
            escaped.push(format!("{label}: {diagnostics}"));
        }
    }
    assert!(
        escaped.is_empty(),
        "documentation escaped validation: {escaped:?}"
    );
}

#[test]
fn kiodoc_recursive_nominals_are_type_oriented_doc_targets() {
    let dir = package(
        "mod pkg;\n\n\
        /// Type [`Twig`]. Signature [`@signature Twig`]. Source [`@source Twig`].\n\
        rec { type Tree = Twig; type { Twig: . | Tree; }; }\n\
        /// Type [`List`]. Signature [`@signature List`]. Source [`@source List`].\n\
        rec type { List: . | List; };\n",
    );
    success(&doc(dir.path(), &["build", "--md"]));
    let rendered = fs::read_to_string(dir.path().join("out/docs-md/pkg.md")).unwrap();
    assert!(!rendered.contains("@signature"), "{rendered}");
    assert!(!rendered.contains("@source"), "{rendered}");
    assert!(rendered.contains("id=\"item-pkg-Twig\""), "{rendered}");
    assert!(rendered.contains("id=\"item-pkg-List\""), "{rendered}");
    for name in ["Twig", "List"] {
        // Package pages only expose public declarations; exercise the module
        // scope for these deliberately private recursive nominal declarations.
        let source = fs::read_to_string(dir.path().join("pkg.kio")).unwrap();
        let bad_source = format!("/// [`@type {name}`]\n{source}");
        fs::write(dir.path().join("pkg.kio"), &bad_source).unwrap();
        let output = doc(dir.path(), &["check"]);
        assert!(!output.status.success(), "@type admitted nominal {name}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("`@type` expects a value binding"),
            "{stderr}"
        );
        fs::write(dir.path().join("pkg.kio"), &source).unwrap();
    }
}

#[test]
fn kiodoc_recursive_group_private_names_do_not_enter_package_scope() {
    let mut escaped = Vec::new();
    for directive in ["Hidden", "@signature Hidden", "@source Node"] {
        let dir = package(
            "mod pkg;\nrec { type Hidden = Node; type { Node: . | Hidden { mk; get; }; }; }\n",
        );
        fs::write(dir.path().join("input.md"), format!("[`{directive}`]\n")).unwrap();
        let output = doc(dir.path(), &["check"]);
        if output.status.success() {
            escaped.push(directive);
        }
    }
    assert!(
        escaped.is_empty(),
        "private targets escaped boundary filtering: {escaped:?}"
    );
}

#[test]
fn kiodoc_recursive_named_compact_type_preserves_group_and_member_ownership() {
    let dir = package(
        "mod pkg;\n\n\
        /// Grove group prose.\n\
        rec {\n\
          /// Branch prose. Signature [`@signature Branches`]. Source [`@source Branch`].\n\
          pub type Branches = { Branch: . | Trunk; };\n\
          /// Trunk prose.\n\
          pub type { Trunk: Branches { pub mk; pub get; }; };\n\
        }\n",
    );
    let checked = Command::new(test_binary!("kio"))
        .args(["check", "pkg.kio"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    success(&checked);
    success(&doc(dir.path(), &["build", "--md"]));
    let rendered = fs::read_to_string(dir.path().join("out/docs-md/pkg.md")).unwrap();
    let plain = plain_html(&rendered);
    assert_eq!(plain.matches("Grove group prose.").count(), 1, "{plain}");
    assert!(rendered.contains("id=\"item-pkg-Branches\""), "{rendered}");
    assert!(rendered.contains("id=\"item-pkg-Branch\""), "{rendered}");
    assert!(rendered.contains("id=\"item-pkg-Trunk\""), "{rendered}");
    assert!(!rendered.contains("@signature"), "{rendered}");
    assert!(!rendered.contains("@source"), "{rendered}");
    assert_eq!(plain.matches("rec {").count(), 7, "{plain}");
}
