use std::fmt::Write;
use std::process::Output;

pub fn describe(output: &Output) -> String {
    let mut message = output.status.to_string();
    for (name, bytes) in [("stdout", &output.stdout), ("stderr", &output.stderr)] {
        if !bytes.is_empty() {
            write!(message, "\n{name}:\n{}", String::from_utf8_lossy(bytes))
                .expect("writing a String cannot fail");
        }
    }
    message
}

#[cfg(test)]
mod tests {
    use super::describe;
    use std::process::Command;

    #[test]
    fn ordinary_failure_keeps_status_and_both_streams() {
        #[cfg(unix)]
        let output = Command::new("sh")
            .args([
                "-c",
                "printf 'compiler output'; printf 'compiler error' >&2; exit 23",
            ])
            .output()
            .unwrap();
        #[cfg(windows)]
        let output = Command::new("cmd")
            .args([
                "/C",
                "echo compiler output& echo compiler error 1>&2& exit /b 23",
            ])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(23));
        let message = describe(&output);
        assert!(message.starts_with(&output.status.to_string()));
        assert!(message.contains("stdout:\ncompiler output"));
        assert!(message.contains("stderr:\ncompiler error"));
    }

    #[cfg(unix)]
    #[test]
    fn silent_signal_failure_is_distinct_from_numeric_exit() {
        use std::os::unix::process::ExitStatusExt;

        let killed = Command::new("sh")
            .args(["-c", "kill -KILL $$"])
            .output()
            .unwrap();
        let exited = Command::new("sh")
            .args(["-c", "exit 137"])
            .output()
            .unwrap();
        assert_eq!(killed.status.signal(), Some(9));
        assert_eq!(exited.status.code(), Some(137));
        assert!(killed.stderr.is_empty());
        assert_eq!(describe(&killed), killed.status.to_string());
        assert_ne!(describe(&killed), describe(&exited));
    }

    #[cfg(unix)]
    #[test]
    fn termination_retains_signal_without_inventing_a_cause() {
        use std::os::unix::process::ExitStatusExt;

        let output = Command::new("sh")
            .args(["-c", "kill -TERM $$"])
            .output()
            .unwrap();
        assert_eq!(output.status.signal(), Some(15));
        let message = describe(&output);
        assert_eq!(message, output.status.to_string());
        assert!(!message.contains("OOM"));
        assert!(!message.contains("cancel"));
    }
}
