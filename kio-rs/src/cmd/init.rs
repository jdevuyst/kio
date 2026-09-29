//! Implementation of `kio init`.

use std::fs;
use std::path::{Path, PathBuf};

use crate::exit_code::ExitCode;
use crate::path_display::DisplayPath;

const HELP_TEMPLATE: &str = "\
Usage: kio init [<package-name>]

Scaffold a new Kio package in the current directory.

With no package name, the current directory name is used. The package
name must be a Kio value-name identifier: lowercase or `_` initial,
then lowercase letters, digits, or `_`, and it may not begin with `__`.
It must contain at least one ASCII letter.

The command writes:
  <package-name>.pkg.kio
  main.kio

It refuses to overwrite existing package files.

Options:
  -h, --help    Show this help and exit.

Exit codes (per {base}/specs/exit-codes.md): 0 on success; 2 on CLI
usage error; 1 on filesystem errors.

See {base}/specs/cli.md#kio-init-package-name for full command behavior.";

pub fn run(args: &[String]) -> ExitCode {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!(
            "{}",
            HELP_TEMPLATE.replace("{base}", crate::KIO_DOCS_BASE_URL)
        );
        return ExitCode::Success;
    }
    let package_name = match args {
        [] => match default_package_name() {
            Ok(name) => name,
            Err(code) => return code,
        },
        [name] if !name.starts_with('-') => name.clone(),
        [flag] if flag.starts_with('-') => {
            eprintln!("error: unknown flag for `kio init`: {flag}");
            return ExitCode::Usage;
        }
        _ => {
            eprintln!("error: `kio init` accepts at most one package name");
            return ExitCode::Usage;
        }
    };

    if let Err(message) = validate_package_name(&package_name) {
        eprintln!("error: invalid package name `{package_name}`: {message}");
        return ExitCode::Usage;
    }

    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(e) => {
            eprintln!("error: cannot read current directory: {e}");
            return ExitCode::Internal;
        }
    };
    init_at(&cwd, &package_name)
}

fn default_package_name() -> Result<String, ExitCode> {
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(e) => {
            eprintln!("error: cannot read current directory: {e}");
            return Err(ExitCode::Internal);
        }
    };
    let Some(name) = cwd.file_name().and_then(|s| s.to_str()) else {
        eprintln!("error: cannot infer package name from the current directory");
        return Err(ExitCode::Usage);
    };
    Ok(name.to_owned())
}

fn init_at(root: &Path, package_name: &str) -> ExitCode {
    let package_file = root.join(format!("{package_name}.pkg.kio"));
    let module = root.join("main.kio");

    if let Some(existing) = existing_package_file(root, &package_file, &module) {
        eprintln!(
            "error: refusing to overwrite existing package file {}",
            DisplayPath(&existing)
        );
        return ExitCode::Usage;
    }

    if let Err(e) = fs::write(&package_file, package_source(package_name)) {
        eprintln!("error: cannot write {}: {e}", DisplayPath(&package_file));
        return ExitCode::Internal;
    }
    if let Err(e) = fs::write(&module, main_source()) {
        eprintln!("error: cannot write {}: {e}", DisplayPath(&module));
        return ExitCode::Internal;
    }

    println!("created Kio package `{package_name}`");
    println!("  {}", DisplayPath(&package_file));
    println!("  {}", DisplayPath(&module));
    ExitCode::Success
}

fn existing_package_file(root: &Path, package_file: &Path, module: &Path) -> Option<PathBuf> {
    if package_file.exists() {
        return Some(package_file.to_path_buf());
    }
    if module.exists() {
        return Some(module.to_path_buf());
    }
    match fs::read_dir(root) {
        Ok(entries) => {
            let mut hits = entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| {
                    path.file_name()
                        .and_then(|s| s.to_str())
                        .is_some_and(crate::file_kind::is_package_file)
                })
                .collect::<Vec<_>>();
            hits.sort();
            hits.into_iter().next()
        }
        Err(_) => None,
    }
}

fn validate_package_name(name: &str) -> Result<(), &'static str> {
    let role = crate::naming::NameRole::Package;
    crate::naming::validate_user_name(name, role).map_err(|violation| violation.explanation(role))
}

fn package_source(package_name: &str) -> String {
    format!(
        concat!(
            "package {};\n\n",
            "build {{\n",
            "  cache \"out/.kio-cache/\";\n\n",
            "  target js {{\n",
            "    out \"out/js/\"\n",
            "  }}\n",
            "}}\n\n",
            "bridge {{\n",
            "  main\n",
            "}}\n",
        ),
        package_name,
    )
}

fn main_source() -> String {
    concat!(
        "module main;\n\n",
        "host type String role(str);\n\n",
        "host fn print(p0: String) -> .;\n\n",
        "pub fn main() -> . { print(\"hello from Kio\\n\"(String)) }\n",
    )
    .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_package_names() {
        assert!(validate_package_name("hello").is_ok());
        assert!(validate_package_name("hello_world1").is_ok());
        assert!(validate_package_name("_a").is_ok());
        assert!(validate_package_name("_1a").is_err());
        assert!(validate_package_name("_").is_err());
        assert!(validate_package_name("_1").is_err());
        assert!(validate_package_name("_1_2").is_err());
        assert!(validate_package_name("").is_err());
        assert!(validate_package_name("Hello").is_err());
        assert!(validate_package_name("hello-world").is_err());
        assert!(validate_package_name("__internal").is_err());
    }

    #[test]
    fn init_writes_package_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(init_at(dir.path(), "hello"), ExitCode::Success);
        assert!(dir.path().join("hello.pkg.kio").is_file());
        assert!(dir.path().join("main.kio").is_file());
    }

    #[test]
    fn init_refuses_existing_package_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("hello.pkg.kio"), "").expect("write");
        assert_eq!(init_at(dir.path(), "hello"), ExitCode::Usage);
    }
}
