use super::*;
use crate::package_collection::{PreparedBuildInputs, prepare_build_inputs};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Component;

pub(super) enum PackagePlan {
    Ready {
        inputs: Box<PreparedBuildInputs>,
        selected: Vec<usize>,
    },
    Failed {
        paths: Vec<PathBuf>,
        code: ExitCode,
        stderr: String,
    },
}

impl PackagePlan {
    fn paths(&self) -> &[PathBuf] {
        match self {
            Self::Ready { inputs, .. } => &inputs.files,
            Self::Failed { paths, .. } => paths,
        }
    }
}

pub(super) fn prepare(pkg_dir: &Path, target_ids: &[String]) -> PackagePlan {
    let mut cap = CapturedOutput::new();
    let mut paths = Vec::new();
    let result = (|| {
        let marker = package_marker(pkg_dir, &mut cap)?;
        paths.push(marker.clone());
        let inputs = prepare_build_inputs(pkg_dir, &marker, &mut paths).map_err(|error| {
            let failure = check::AnalysisFailure::from_walk_error(error, BTreeMap::new());
            check::render_analysis_failure(&failure, &mut cap.stderr)
        })?;
        let selected = select(&inputs, target_ids).map_err(|message| {
            cap_errln!(cap, "error: {message}");
            // A selection error must not discard the package's known inputs.
            paths = inputs.files.clone();
            ExitCode::Build
        })?;
        Ok((inputs, selected))
    })();
    match result {
        Ok((inputs, selected)) => PackagePlan::Ready {
            inputs: Box::new(inputs),
            selected,
        },
        Err(code) => PackagePlan::Failed {
            paths,
            code,
            stderr: cap.stderr,
        },
    }
}

fn select(inputs: &PreparedBuildInputs, target_ids: &[String]) -> Result<Vec<usize>, String> {
    let block = inputs.manifest.package_file.build.as_ref().ok_or_else(||
        "the package's package file has no `build { … }` block — kio build requires one to name the compilation targets (see specs/package.md § Build target files)".to_owned()
    )?;
    check_unique_ids(block)?;
    if target_ids.is_empty() {
        return Ok((0..block.targets.len()).collect());
    }
    let mut selected = Vec::new();
    for id in target_ids {
        let index = block
            .targets
            .iter()
            .position(|target| target.id == *id)
            .ok_or_else(|| format!("unknown target id `{id}` in the build block"))?;
        if !selected.contains(&index) {
            selected.push(index);
        }
    }
    Ok(selected)
}

pub(super) fn validate(plans: &BTreeMap<PathBuf, PackagePlan>) -> Result<(), String> {
    let mut outputs = Vec::new();
    for plan in plans.values() {
        let PackagePlan::Ready { inputs, selected } = plan else {
            continue;
        };
        let block = inputs
            .manifest
            .package_file
            .build
            .as_ref()
            .expect("selected build block");
        for &index in selected {
            let target = &block.targets[index];
            if BuildTarget::from_id(&target.id).is_none() {
                continue;
            }
            let mut entries = target.entries.iter().filter(|entry| entry.key == "out");
            let Some(entry) = entries.next() else {
                continue;
            };
            // These invalid targets fail in backend validation before any writes.
            if entries.next().is_some() || Path::new(&entry.value).is_absolute() {
                continue;
            }
            let path = normalize(&inputs.root.join(&entry.value)).map_err(|error| {
                format!(
                    "cannot resolve output `{}` for target `{}`: {error}",
                    entry.value, target.id
                )
            })?;
            let label = format!(
                "target `{}` in `{}` (out `{}`)",
                target.id,
                DisplayPath(&inputs.root),
                entry.value
            );
            outputs.push((path, label));
        }
    }
    outputs.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    for pair in outputs.windows(2) {
        if pair[1].0.starts_with(&pair[0].0) {
            return Err(format!(
                "conflicting output directories: {} and {}",
                pair[0].1, pair[1].1
            ));
        }
    }
    let outputs: BTreeMap<_, _> = outputs.into_iter().collect();
    let mut seen = BTreeSet::new();
    for plan in plans.values() {
        for path in plan.paths() {
            let access = if path.is_absolute() {
                path.clone()
            } else {
                std::env::current_dir()
                    .map_err(|e| e.to_string())?
                    .join(path)
            };
            // The collector does not follow directory symlinks. Keep its access
            // path as well as a file symlink's referent so neither can be erased.
            for input in std::iter::once(access.clone()).chain(fs::canonicalize(&access).ok()) {
                if !seen.insert(input.clone()) {
                    continue;
                }
                for ancestor in input.ancestors() {
                    if let Some(target) = outputs.get(ancestor) {
                        return Err(format!(
                            "output directory for {target} contains input `{}`",
                            DisplayPath(path)
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

/// Resolve existing aliases before interpreting `..`; a missing tail needs no
/// filesystem creation. Leaf symlinks intentionally receive the same conservative
/// alias check as existing parent directories.
fn normalize(path: &Path) -> std::io::Result<PathBuf> {
    // In particular, a Windows drive-relative prefix can replace the absolute
    // package root when joined; it must not escape absolute-path comparison.
    if !path.is_absolute() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "output does not resolve to an absolute path from the package root",
        ));
    }
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => result.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            Component::Normal(name) => {
                result.push(name);
                match fs::canonicalize(&result) {
                    Ok(canonical) => result = canonical,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
            }
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_requires_an_absolute_access_path() {
        assert!(normalize(Path::new("missing/relative")).is_err());
        #[cfg(windows)]
        assert!(normalize(Path::new("C:missing-relative-output")).is_err());
    }

    fn package(root: &Path, targets: &str) {
        fs::create_dir_all(root).unwrap();
        fs::write(
            root.join("app.pkg.kio"),
            format!("package app; build {{ {targets} }}"),
        )
        .unwrap();
    }

    fn plans(roots: &[&Path], ids: &[&str]) -> BTreeMap<PathBuf, PackagePlan> {
        roots
            .iter()
            .map(|root| {
                (
                    root.to_path_buf(),
                    prepare(
                        root,
                        &ids.iter().map(|id| (*id).to_owned()).collect::<Vec<_>>(),
                    ),
                )
            })
            .collect()
    }

    #[test]
    fn selected_outputs_compare_components_and_deduplicate_identity() {
        let dir = tempfile::tempdir().unwrap();
        for (targets, conflict) in [
            (
                "target js { out \"out\"; }; target rust { out \"out\"; }",
                true,
            ),
            (
                "target js { out \"out\"; }; target rust { out \"out/rs\"; }",
                true,
            ),
            (
                "target js { out \"out/a/../rs\"; }; target rust { out \"out/rs\"; }",
                true,
            ),
            (
                "target js { out \"out/js\"; }; target rust { out \"out/js2\"; }",
                false,
            ),
            (
                "target js { out \"../external\"; }; target rust { out \"out/rs\"; }",
                false,
            ),
        ] {
            package(dir.path(), targets);
            assert_eq!(
                validate(&plans(&[dir.path()], &[])).is_err(),
                conflict,
                "{targets}"
            );
            assert!(validate(&plans(&[dir.path()], &["js", "js"])).is_ok());
            let plan = prepare(dir.path(), &["js".to_owned(), "js".to_owned()]);
            assert!(matches!(plan, PackagePlan::Ready { selected, .. } if selected == [0]));
        }
    }

    #[test]
    fn every_registered_backend_protects_known_inputs() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("input")).unwrap();
        fs::write(dir.path().join("input/main.kio"), "module input/main;").unwrap();
        for backend in BuildTarget::ALL {
            package(
                dir.path(),
                &format!("target {} {{ out \"input\"; }}", backend.id()),
            );
            let error = validate(&plans(&[dir.path()], &[])).unwrap_err();
            assert!(error.contains("contains input"), "{error}");
        }
    }

    #[test]
    fn dependency_lock_and_signature_inputs_are_protected() {
        for name in ["dependency.dep.kio", "dependency.lock.kio", "app.sig.kio"] {
            let dir = tempfile::tempdir().unwrap();
            package(dir.path(), "target js { out \"input\"; }");
            fs::create_dir(dir.path().join("input")).unwrap();
            fs::write(
                dir.path().join("input").join(name),
                "read only after preflight",
            )
            .unwrap();
            let error = validate(&plans(&[dir.path()], &[])).unwrap_err();
            assert!(error.contains(name), "{error}");
        }
    }

    #[test]
    fn manifest_read_failure_retains_collected_inputs() {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let good = root.join("good");
        let bad = root.join("bad");
        package(&good, "target js { out \"../bad/retained\"; }");
        package(&bad, "target js { out \"out\"; }");
        fs::create_dir(bad.join("retained")).unwrap();
        let input = bad.join("retained/main.kio");
        fs::write(&input, "module retained/main;").unwrap();
        let marker = bad.join("app.pkg.kio");
        let (failed, attempts) =
            crate::package_collection::import_grammar_with_denied_reads([marker.clone()], || {
                prepare(&bad, &[])
            });
        assert_eq!(attempts, [marker]);
        assert!(matches!(&failed, PackagePlan::Failed { paths, .. } if paths.contains(&input)));
        let plans = BTreeMap::from([(good.clone(), prepare(&good, &[])), (bad, failed)]);
        assert!(validate(&plans).unwrap_err().contains("retained/main.kio"));
    }

    #[test]
    fn cross_package_outputs_and_failed_package_inputs_share_preflight() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        package(&a, "target js { out \"../shared\"; }");
        package(&b, "target rust { out \"../shared/rs\"; }");
        assert!(
            validate(&plans(&[&a, &b], &[]))
                .unwrap_err()
                .contains("conflicting output")
        );
        package(&a, "target js { out \"../b\"; }");
        fs::write(b.join("app.pkg.kio"), "package app; build { invalid }").unwrap();
        let plans = plans(&[&a, &b], &[]);
        assert!(matches!(
            plans.get(&b),
            Some(PackagePlan::Failed {
                code: ExitCode::Parse,
                ..
            })
        ));
        assert!(validate(&plans).unwrap_err().contains("contains input"));
    }

    #[cfg(unix)]
    #[test]
    fn aliases_resolve_before_parent_components_and_protect_both_input_paths() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let canonical = fs::canonicalize(dir.path()).unwrap();
        let root = canonical.join("pkg");
        let outside = canonical.join("outside");
        fs::create_dir_all(outside.join("nested")).unwrap();
        package(&root, "target js { out \"src\"; }");
        fs::create_dir(root.join("src")).unwrap();
        fs::write(outside.join("main.kio"), "module src/main;").unwrap();
        symlink(outside.join("main.kio"), root.join("src/main.kio")).unwrap();
        symlink(outside.join("nested"), root.join("link")).unwrap();
        assert_eq!(
            normalize(&root.join("link/../shared")).unwrap(),
            outside.join("shared")
        );
        assert!(
            validate(&plans(&[&root], &[]))
                .unwrap_err()
                .contains("contains input")
        );
        package(&root, "target js { out \"../outside\"; }");
        assert!(
            validate(&plans(&[&root], &[]))
                .unwrap_err()
                .contains("contains input")
        );
        package(
            &root,
            "target js { out \"link\"; }; target rust { out \"../outside/nested\"; }",
        );
        assert!(
            validate(&plans(&[&root], &[]))
                .unwrap_err()
                .contains("conflicting output")
        );
    }
}
