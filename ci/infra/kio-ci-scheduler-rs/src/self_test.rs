//! Fast native smoke test for scheduler storage and process supervision.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Output};

use crate::compiler_admission::{CompilerAdmission, CompilerPermit};
use crate::process_supervisor;
use crate::resource_admission::{
    FixedResource, FixedResourceAdmission, ResourceAdmissionError, WorkMode,
};

/// Exercise native work/cargo/compiler admission and one supervised child.
///
/// `state_root` must not already exist. The caller supplies a unique path so
/// this smoke test cannot contend with or mutate the live Git-common scheduler
/// state. The directory is removed before a successful return.
pub fn run(state_root: &Path) -> Result<(), SelfTestError> {
    let mut child = Command::new(
        std::env::current_exe()
            .map_err(|source| SelfTestError(SelfTestErrorKind::CurrentExe(source)))?,
    );
    child.arg("--build-id");
    let output = exercise(state_root, &mut child)?;
    validate_child_output(output, format!("{}\n", crate::BUILD_ID).as_bytes())
}

fn exercise(state_root: &Path, child: &mut Command) -> Result<Output, SelfTestError> {
    let state = IsolatedStateRoot::create(state_root)?;
    let output = {
        // Keep the same global acquisition order that nested scheduler calls
        // enforce, so the smoke test cannot conceal an ordering deadlock.
        let work = FixedResourceAdmission::work(state_root.to_path_buf(), 1, WorkMode::Normal)
            .map_err(|source| {
                SelfTestError(SelfTestErrorKind::Admission {
                    resource: FixedResource::Work,
                    source,
                })
            })?
            .acquire()
            .map_err(|source| {
                SelfTestError(SelfTestErrorKind::Admission {
                    resource: FixedResource::Work,
                    source,
                })
            })?;
        let cargo = FixedResourceAdmission::cargo(state_root.to_path_buf())
            .map_err(|source| {
                SelfTestError(SelfTestErrorKind::Admission {
                    resource: FixedResource::Cargo,
                    source,
                })
            })?
            .acquire()
            .map_err(|source| {
                SelfTestError(SelfTestErrorKind::Admission {
                    resource: FixedResource::Cargo,
                    source,
                })
            })?;
        let compiler = acquire_compiler(state_root)?;
        debug_assert_eq!(work.resource(), FixedResource::Work);
        debug_assert_eq!(cargo.resource(), FixedResource::Cargo);

        child.env_remove("KIO_CI_SCHEDULE_DIR");
        child.env("KIO_CI_SCHEDULE_HELD", "work,cargo");
        #[cfg(unix)]
        {
            work.prepare_inherited_lease(child).map_err(|source| {
                SelfTestError(SelfTestErrorKind::Admission {
                    resource: FixedResource::Work,
                    source,
                })
            })?;
            cargo.prepare_inherited_lease(child).map_err(|source| {
                SelfTestError(SelfTestErrorKind::Admission {
                    resource: FixedResource::Cargo,
                    source,
                })
            })?;
        }
        compiler
            .prepare_for(child)
            .map_err(|source| SelfTestError(SelfTestErrorKind::CompilerAdmission(source)))?;
        process_supervisor::output(child)
            .map_err(|source| SelfTestError(SelfTestErrorKind::SupervisedChild(source)))?
    };
    state.remove()?;
    Ok(output)
}

fn acquire_compiler(state_root: &Path) -> Result<CompilerPermit, SelfTestError> {
    CompilerAdmission::shared(state_root.to_path_buf(), 1)
        .and_then(|admission| admission.acquire())
        .map_err(|source| SelfTestError(SelfTestErrorKind::CompilerAdmission(source)))
}

fn validate_child_output(output: Output, expected_stdout: &[u8]) -> Result<(), SelfTestError> {
    if !output.status.success() {
        return Err(SelfTestError(SelfTestErrorKind::ChildStatus(output.status)));
    }
    if !output.stderr.is_empty() {
        return Err(SelfTestError(SelfTestErrorKind::UnexpectedStderr(
            output.stderr,
        )));
    }
    if output.stdout != expected_stdout {
        return Err(SelfTestError(SelfTestErrorKind::UnexpectedStdout {
            expected: expected_stdout.to_vec(),
            actual: output.stdout,
        }));
    }
    Ok(())
}

struct IsolatedStateRoot {
    path: PathBuf,
    remove_on_drop: bool,
}

impl IsolatedStateRoot {
    fn create(path: &Path) -> Result<Self, SelfTestError> {
        fs::create_dir(path).map_err(|source| {
            SelfTestError(SelfTestErrorKind::StateRoot {
                action: "create",
                path: path.to_path_buf(),
                source,
            })
        })?;
        Ok(Self {
            path: path.to_path_buf(),
            remove_on_drop: true,
        })
    }

    fn remove(mut self) -> Result<(), SelfTestError> {
        fs::remove_dir_all(&self.path).map_err(|source| {
            SelfTestError(SelfTestErrorKind::StateRoot {
                action: "remove",
                path: self.path.clone(),
                source,
            })
        })?;
        self.remove_on_drop = false;
        Ok(())
    }
}

impl Drop for IsolatedStateRoot {
    fn drop(&mut self) {
        if self.remove_on_drop {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

#[derive(Debug)]
pub struct SelfTestError(SelfTestErrorKind);

#[derive(Debug)]
enum SelfTestErrorKind {
    CurrentExe(io::Error),
    StateRoot {
        action: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    Admission {
        resource: FixedResource,
        source: ResourceAdmissionError,
    },
    CompilerAdmission(crate::compiler_admission::Error),
    SupervisedChild(io::Error),
    ChildStatus(ExitStatus),
    UnexpectedStdout {
        expected: Vec<u8>,
        actual: Vec<u8>,
    },
    UnexpectedStderr(Vec<u8>),
}

impl fmt::Display for SelfTestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            SelfTestErrorKind::CurrentExe(source) => {
                write!(formatter, "cannot resolve scheduler executable: {source}")
            }
            SelfTestErrorKind::StateRoot {
                action,
                path,
                source,
            } => write!(
                formatter,
                "cannot {action} isolated scheduler state {}: {source}",
                path.display()
            ),
            SelfTestErrorKind::Admission { resource, source } => {
                write!(
                    formatter,
                    "{} self-test admission: {source}",
                    resource.as_str()
                )
            }
            SelfTestErrorKind::CompilerAdmission(source) => {
                write!(formatter, "compiler self-test admission: {source}")
            }
            SelfTestErrorKind::SupervisedChild(source) => {
                write!(formatter, "cannot run supervised self-test child: {source}")
            }
            SelfTestErrorKind::ChildStatus(status) => {
                write!(formatter, "supervised self-test child exited {status}")
            }
            SelfTestErrorKind::UnexpectedStdout { expected, actual } => write!(
                formatter,
                "supervised self-test child wrote unexpected stdout (expected {:?}, got {:?})",
                String::from_utf8_lossy(expected),
                String::from_utf8_lossy(actual)
            ),
            SelfTestErrorKind::UnexpectedStderr(actual) => write!(
                formatter,
                "supervised self-test child wrote stderr: {:?}",
                String::from_utf8_lossy(actual)
            ),
        }
    }
}

impl std::error::Error for SelfTestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.0 {
            SelfTestErrorKind::CurrentExe(source)
            | SelfTestErrorKind::StateRoot { source, .. }
            | SelfTestErrorKind::SupervisedChild(source) => Some(source),
            SelfTestErrorKind::Admission { source, .. } => Some(source),
            SelfTestErrorKind::CompilerAdmission(source) => Some(source),
            SelfTestErrorKind::ChildStatus(_)
            | SelfTestErrorKind::UnexpectedStdout { .. }
            | SelfTestErrorKind::UnexpectedStderr(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{SelfTestErrorKind, exercise};
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};

    const CHILD_MARKER: &str = "kio-scheduler-supervised-self-test-child";
    static NONCE: AtomicU64 = AtomicU64::new(0);

    #[test]
    #[ignore]
    fn supervised_child_fixture() {
        assert_eq!(
            std::env::var("KIO_CI_SCHEDULE_HELD").unwrap(),
            "work,cargo,compiler"
        );
        let schedule_root = PathBuf::from(std::env::var_os("KIO_CI_SCHEDULE_DIR").unwrap());
        let expected_root =
            PathBuf::from(std::env::var_os("KIO_TEST_EXPECT_SCHEDULE_ROOT").unwrap());
        assert_eq!(schedule_root, std::fs::canonicalize(expected_root).unwrap());
        for resource in ["work", "cargo", "compiler"] {
            let claims = schedule_root.join(resource).join("claims");
            assert!(std::fs::read_dir(claims).unwrap().any(|entry| {
                entry
                    .unwrap()
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.ends_with(".active"))
            }));
        }

        #[cfg(unix)]
        {
            let lease_fds = crate::resource_admission::inherited_lease_fds_from_env().unwrap();
            let expected = std::env::var("KIO_TEST_EXPECT_LEASE_FDS")
                .unwrap()
                .parse::<usize>()
                .unwrap();
            assert_eq!(lease_fds.len(), expected);
            for fd in lease_fds {
                // SAFETY: this only probes a descriptor inherited by the test
                // child and does not alter descriptor or process state.
                assert_ne!(unsafe { libc::fcntl(fd, libc::F_GETFD) }, -1);
            }
        }
        println!("{CHILD_MARKER}");
    }

    #[test]
    fn isolated_self_test_exercises_all_resources_and_supervised_stdio() {
        let state_root = unique_state_root("native");
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .args([
                "--ignored",
                "--exact",
                "self_test::tests::supervised_child_fixture",
                "--nocapture",
            ])
            .env("KIO_TEST_EXPECT_SCHEDULE_ROOT", &state_root);
        #[cfg(unix)]
        child.env(
            "KIO_TEST_EXPECT_LEASE_FDS",
            (crate::resource_admission::inherited_lease_fds_from_env()
                .unwrap()
                .len()
                + 3)
            .to_string(),
        );

        let output = exercise(&state_root, &mut child).unwrap();
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        assert!(
            String::from_utf8(output.stdout)
                .unwrap()
                .contains(CHILD_MARKER)
        );
        assert!(!state_root.exists());
    }

    #[test]
    fn compiler_admission_failures_keep_their_error_class() {
        let state_root = unique_state_root("compiler-error");
        std::fs::create_dir(&state_root).unwrap();
        std::fs::write(state_root.join("compiler"), b"not a directory").unwrap();

        let error = match super::acquire_compiler(&state_root) {
            Ok(_) => panic!("invalid compiler state unexpectedly admitted"),
            Err(error) => error,
        };
        assert!(matches!(&error.0, SelfTestErrorKind::CompilerAdmission(_)));
        assert!(
            error
                .to_string()
                .starts_with("compiler self-test admission:")
        );
        assert!(std::error::Error::source(&error).is_some());

        std::fs::remove_dir_all(state_root).unwrap();
    }

    #[test]
    fn state_root_must_be_isolated() {
        let state_root = unique_state_root("occupied");
        std::fs::create_dir(&state_root).unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap());
        let error = exercise(&state_root, &mut child).unwrap_err();
        assert!(matches!(error.0, SelfTestErrorKind::StateRoot { .. }));
        std::fs::remove_dir(state_root).unwrap();
    }

    fn unique_state_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "kio-scheduler-self-test-{label}-{}-{}",
            std::process::id(),
            NONCE.fetch_add(1, Ordering::Relaxed)
        ))
    }
}
