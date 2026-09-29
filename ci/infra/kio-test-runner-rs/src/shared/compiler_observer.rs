//! Opaque debug observer for native compiler processes launched by test runners.
//!
//! The observer is deliberately separate from runner artifact-cache policy. When
//! configured, it becomes the outermost executable for an actual compile and
//! receives the complete former command as its argv. Tool identity probes,
//! runtime launches, cache hits, and same-key cache waiters never pass through
//! this seam.

use std::env;
use std::ffi::{OsStr, OsString};
use std::process::Command;

pub const ENV: &str = "KIO_DEBUG_TEST_RUNNER_COMPILER_OBSERVER";

/// One opaque executable placed outside an actual native compiler command.
///
/// The value is an [`OsString`], not shell text: whitespace and metacharacters
/// are part of the executable name, and inline observer arguments are not
/// supported.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CompilerObserver {
    executable: Option<OsString>,
}

impl CompilerObserver {
    /// Read the observer independently of runner build-cache configuration.
    pub fn from_env() -> Result<Self, String> {
        Self::parse(env::var_os(ENV))
    }

    fn parse(value: Option<OsString>) -> Result<Self, String> {
        match value {
            Some(executable) if executable.is_empty() => {
                Err(format!("{ENV} must name a nonempty executable"))
            }
            executable => Ok(Self { executable }),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(executable: impl Into<OsString>) -> Self {
        Self::parse(Some(executable.into())).expect("test observer executable is nonempty")
    }

    /// Construct a fully nested compile command.
    ///
    /// With both layers configured this is `observer wrapper compiler ...`;
    /// without an observer it is byte-for-byte the former
    /// `wrapper compiler ...` (or bare `compiler ...`) command prefix.
    pub fn command(&self, compiler: &OsStr, wrapper: Option<&OsStr>) -> Command {
        match (&self.executable, wrapper) {
            (Some(observer), Some(wrapper)) => {
                let mut command = Command::new(observer);
                command.arg(wrapper).arg(compiler);
                command
            }
            (Some(observer), None) => {
                let mut command = Command::new(observer);
                command.arg(compiler);
                command
            }
            (None, Some(wrapper)) => {
                let mut command = Command::new(wrapper);
                command.arg(compiler);
                command
            }
            (None, None) => Command::new(compiler),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CompilerObserver, ENV};
    use std::ffi::{OsStr, OsString};

    fn argv(command: &std::process::Command) -> Vec<OsString> {
        std::iter::once(command.get_program().to_os_string())
            .chain(command.get_args().map(OsStr::to_os_string))
            .collect()
    }

    #[test]
    fn unset_preserves_bare_and_wrapped_command_prefixes() {
        let observer = CompilerObserver::parse(None).unwrap();
        assert_eq!(
            argv(&observer.command(OsStr::new("rustc"), None)),
            [OsString::from("rustc")]
        );
        assert_eq!(
            argv(&observer.command(OsStr::new("rustc"), Some(OsStr::new("sccache")))),
            [OsString::from("sccache"), OsString::from("rustc")]
        );
    }

    #[test]
    fn observer_is_outermost_with_and_without_the_cache_wrapper() {
        let observer = CompilerObserver::parse(Some(OsString::from("observe"))).unwrap();
        assert_eq!(
            argv(&observer.command(OsStr::new("rustc"), Some(OsStr::new("sccache")))),
            [
                OsString::from("observe"),
                OsString::from("sccache"),
                OsString::from("rustc")
            ]
        );
        assert_eq!(
            argv(&observer.command(OsStr::new("go"), None)),
            [OsString::from("observe"), OsString::from("go")]
        );
    }

    #[test]
    fn observer_is_one_opaque_executable_not_shell_syntax() {
        let executable = OsString::from("observer --flag; still-one-program");
        let observer = CompilerObserver::parse(Some(executable.clone())).unwrap();
        let command = observer.command(OsStr::new("swiftc"), None);
        assert_eq!(command.get_program(), executable);
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [OsStr::new("swiftc")]
        );
    }

    #[test]
    fn empty_observer_is_rejected() {
        let error = CompilerObserver::parse(Some(OsString::new())).unwrap_err();
        assert_eq!(error, format!("{ENV} must name a nonempty executable"));
    }

    #[cfg(unix)]
    #[test]
    fn non_unicode_observer_path_is_preserved() {
        use std::os::unix::ffi::OsStringExt;

        let executable = OsString::from_vec(vec![b'o', b'b', 0x80]);
        let observer = CompilerObserver::parse(Some(executable.clone())).unwrap();
        assert_eq!(
            observer.command(OsStr::new("ghc"), None).get_program(),
            executable
        );
    }
}
