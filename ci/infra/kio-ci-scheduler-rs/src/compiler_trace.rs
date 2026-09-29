use std::env;
use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::compiler_admission::{
    AdmissionDecision, AdmissionEvaluation, DecisionBlocker, DecisionBlockers,
};

pub(crate) const TRACE_ENV: &str = "KIO_DEBUG_CI_SCHEDULER_TRACE";
const SCHEMA: &str = "kio-ci-compiler-admission-v3";
const MAX_TOKEN_BYTES: usize = 64;
static PROCESS_ACQUISITION_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug)]
pub(crate) struct CompilerTraceConfig {
    schedule_root: PathBuf,
    directory: PathBuf,
    #[cfg(test)]
    io_assertions: Option<TraceIoAssertions>,
}

#[cfg(test)]
#[derive(Clone, Debug)]
struct TraceIoAssertions {
    lock_path: PathBuf,
    claims_dir: PathBuf,
}

impl CompilerTraceConfig {
    pub(crate) fn from_env(schedule_root: &Path) -> Result<Option<Self>, Error> {
        let token = match env::var(TRACE_ENV) {
            Ok(token) => token,
            Err(env::VarError::NotPresent) => return Ok(None),
            Err(env::VarError::NotUnicode(_)) => {
                return Err(Error::invalid(
                    PathBuf::from(TRACE_ENV),
                    format!("{TRACE_ENV} must be valid Unicode"),
                ));
            }
        };
        Self::new(schedule_root, &token).map(Some)
    }

    fn new(schedule_root: &Path, token: &str) -> Result<Self, Error> {
        validate_token(token)?;
        let directory = schedule_root
            .join("debug")
            .join("compiler-admission")
            .join(token);
        Ok(Self {
            schedule_root: schedule_root.to_path_buf(),
            directory,
            #[cfg(test)]
            io_assertions: None,
        })
    }

    pub(crate) fn start(
        &self,
        program: Option<&OsStr>,
        mode: &'static str,
        requested_capacity: usize,
        _ticket: u64,
    ) -> Result<CompilerTrace, Error> {
        #[cfg(test)]
        if let Some(assertions) = &self.io_assertions {
            assertions.assert_start(_ticket);
        }
        std::fs::create_dir_all(&self.directory)
            .map_err(|error| Error::io(&self.directory, error))?;
        let directory = std::fs::canonicalize(&self.directory)
            .map_err(|error| Error::io(&self.directory, error))?;
        let schedule_root = std::fs::canonicalize(&self.schedule_root)
            .map_err(|error| Error::io(&self.schedule_root, error))?;
        if !directory.starts_with(&schedule_root) {
            return Err(Error::invalid(
                directory,
                "compiler trace directory resolves outside the scheduler root",
            ));
        }
        let acquisition_sequence = loop {
            let acquisition_sequence = next_acquisition_sequence()?;
            let path = directory.join(format!(
                "{:010}-{acquisition_sequence:020}.jsonl",
                std::process::id()
            ));
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => break (acquisition_sequence, path, file),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(Error::io(&path, error)),
            }
        };
        let (acquisition_sequence, path, file) = acquisition_sequence;
        let wall_anchor_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| Error::invalid(path.clone(), error.to_string()))?
            .as_millis();
        Ok(CompilerTrace {
            path,
            file,
            started_at: Instant::now(),
            wall_anchor_unix_ms,
            acquisition_sequence,
            mode,
            requested_capacity,
            program_basename_hex: program_basename_hex(program),
            last_wait: None,
            #[cfg(test)]
            assert_unlocked_on_record: self
                .io_assertions
                .as_ref()
                .map(|assertions| assertions.lock_path.clone()),
        })
    }

    #[cfg(test)]
    pub(crate) fn for_test(schedule_root: &Path, token: &str) -> Self {
        Self::new(schedule_root, token).unwrap()
    }

    #[cfg(test)]
    pub(crate) fn assert_io_after_publication(mut self, lock_path: PathBuf) -> Self {
        self.io_assertions = Some(TraceIoAssertions {
            claims_dir: lock_path.with_file_name("claims"),
            lock_path,
        });
        self
    }

    #[cfg(test)]
    pub(crate) fn directory(&self) -> &Path {
        &self.directory
    }
}

pub(crate) struct CompilerTrace {
    path: PathBuf,
    file: File,
    started_at: Instant,
    wall_anchor_unix_ms: u128,
    acquisition_sequence: u64,
    mode: &'static str,
    requested_capacity: usize,
    program_basename_hex: String,
    last_wait: Option<AdmissionEvaluation>,
    #[cfg(test)]
    assert_unlocked_on_record: Option<PathBuf>,
}

impl CompilerTrace {
    pub(crate) fn record(&mut self, evaluation: AdmissionEvaluation) -> Result<(), Error> {
        #[cfg(test)]
        if let Some(lock_path) = &self.assert_unlocked_on_record {
            assert_lock_is_free(lock_path);
        }
        let wait = evaluation.decision == AdmissionDecision::Wait;
        if wait && self.last_wait == Some(evaluation) {
            return Ok(());
        }
        self.last_wait = wait.then_some(evaluation);

        let mut line = String::with_capacity(800);
        line.push_str("{\"schema\":\"");
        line.push_str(SCHEMA);
        line.push_str("\",\"wall_anchor_unix_ms\":");
        line.push_str(&self.wall_anchor_unix_ms.to_string());
        line.push_str(",\"elapsed_us\":");
        line.push_str(&self.started_at.elapsed().as_micros().to_string());
        line.push_str(",\"pid\":");
        line.push_str(&std::process::id().to_string());
        line.push_str(",\"acquisition_sequence\":");
        line.push_str(&self.acquisition_sequence.to_string());
        line.push_str(",\"ticket\":");
        push_option_u64(&mut line, evaluation.ticket);
        line.push_str(",\"requested_capacity\":");
        line.push_str(&self.requested_capacity.to_string());
        line.push_str(",\"mode\":\"");
        line.push_str(self.mode);
        line.push_str("\",\"program_basename_hex\":\"");
        line.push_str(&self.program_basename_hex);
        line.push_str("\",\"active_total\":");
        push_option_usize(&mut line, evaluation.active_total);
        line.push_str(",\"active_adaptive\":");
        push_option_usize(&mut line, evaluation.active_adaptive);
        line.push_str(",\"pending_total\":");
        push_option_usize(&mut line, evaluation.pending_total);
        line.push_str(",\"oldest_pending_ticket\":");
        push_option_u64(&mut line, evaluation.oldest_pending_ticket);
        line.push_str(",\"fixed_capacity_min\":");
        push_option_usize(&mut line, evaluation.fixed_capacity_min);
        line.push_str(",\"effective_limit\":");
        push_option_usize(&mut line, evaluation.effective_limit);
        line.push_str(",\"candidate_slot\":");
        push_option_usize(&mut line, evaluation.candidate_slot);
        line.push_str(",\"adaptive_capacity\":");
        push_option_usize(&mut line, evaluation.adaptive_capacity);
        line.push_str(",\"decision\":\"");
        line.push_str(evaluation.decision.label());
        line.push_str("\",\"blockers\":");
        push_blockers(&mut line, evaluation.blockers);
        line.push_str("}\n");
        self.file
            .write_all(line.as_bytes())
            .and_then(|()| self.file.flush())
            .map_err(|error| Error::io(&self.path, error))
    }

    #[cfg(test)]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
impl TraceIoAssertions {
    fn assert_start(&self, ticket: u64) {
        assert_lock_is_free(&self.lock_path);
        let prefix = format!("v1-{ticket:020}-");
        let published = std::fs::read_dir(&self.claims_dir).unwrap().any(|entry| {
            let name = entry.unwrap().file_name();
            let name = name.to_str().unwrap();
            name.starts_with(&prefix) && (name.ends_with(".pending") || name.ends_with(".active"))
        });
        assert!(
            published,
            "compiler trace setup preceded publication of ticket {ticket}"
        );
    }
}

#[cfg(test)]
fn assert_lock_is_free(path: &Path) {
    use std::fs::{OpenOptions, TryLockError};
    use std::time::{Duration, Instant};

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .unwrap();
    let deadline = Instant::now() + Duration::from_millis(500);
    loop {
        match file.try_lock() {
            Ok(()) => {
                file.unlock().unwrap();
                return;
            }
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(TryLockError::WouldBlock) => {
                panic!("compiler trace I/O ran while {} was locked", path.display())
            }
            Err(TryLockError::Error(error)) => panic!("cannot probe state lock: {error}"),
        }
    }
}

fn push_blockers(line: &mut String, blockers: DecisionBlockers) {
    line.push('[');
    let mut first = true;
    for blocker in DecisionBlocker::ALL {
        if !blockers.contains(blocker) {
            continue;
        }
        if !first {
            line.push(',');
        }
        first = false;
        line.push('"');
        line.push_str(blocker.label());
        line.push('"');
    }
    line.push(']');
}

fn push_option_usize(line: &mut String, value: Option<usize>) {
    match value {
        Some(value) => line.push_str(&value.to_string()),
        None => line.push_str("null"),
    }
}

fn push_option_u64(line: &mut String, value: Option<u64>) {
    match value {
        Some(value) => line.push_str(&value.to_string()),
        None => line.push_str("null"),
    }
}

fn program_basename_hex(program: Option<&OsStr>) -> String {
    let Some(program) = program else {
        return String::new();
    };
    let bytes = program.as_encoded_bytes();
    let basename = bytes
        .iter()
        .rposition(|byte| matches!(byte, b'/' | b'\\'))
        .map_or(bytes, |separator| &bytes[separator + 1..]);
    let mut encoded = String::with_capacity(basename.len() * 2);
    for byte in basename {
        use std::fmt::Write as _;
        write!(&mut encoded, "{byte:02x}")
            .unwrap_or_else(|_| unreachable!("formatting into a String cannot fail"));
    }
    encoded
}

fn next_acquisition_sequence() -> Result<u64, Error> {
    PROCESS_ACQUISITION_SEQUENCE
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |sequence| {
            sequence.checked_add(1)
        })
        .map(|previous| previous + 1)
        .map_err(|_| {
            Error::invalid(
                PathBuf::from(TRACE_ENV),
                "compiler trace acquisition sequence exhausted",
            )
        })
}

fn validate_token(token: &str) -> Result<(), Error> {
    let valid = !token.is_empty()
        && token.len() <= MAX_TOKEN_BYTES
        && token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    let reserved = is_windows_reserved_name(token);
    if valid && !reserved {
        return Ok(());
    }
    Err(Error::invalid(
        PathBuf::from(TRACE_ENV),
        format!(
            "{TRACE_ENV} must be 1 to {MAX_TOKEN_BYTES} ASCII letters, digits, '-' or '_', and not a reserved portable filename"
        ),
    ))
}

fn is_windows_reserved_name(token: &str) -> bool {
    let upper = token.to_ascii_uppercase();
    matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$")
        || upper
            .strip_prefix("COM")
            .or_else(|| upper.strip_prefix("LPT"))
            .is_some_and(|suffix| {
                matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
            })
}

#[derive(Debug)]
pub(crate) struct Error {
    path: PathBuf,
    message: String,
}

impl Error {
    fn io(path: &Path, source: std::io::Error) -> Self {
        Self {
            path: path.to_path_buf(),
            message: source.to_string(),
        }
    }

    fn invalid(path: PathBuf, message: impl Into<String>) -> Self {
        Self {
            path,
            message: message.into(),
        }
    }

    pub(crate) fn into_parts(self) -> (PathBuf, String) {
        (self.path, self.message)
    }
}

#[cfg(test)]
mod tests {
    use super::{CompilerTraceConfig, SCHEMA, TRACE_ENV, program_basename_hex, validate_token};
    use crate::compiler_admission::{
        AdmissionDecision, AdmissionEvaluation, DecisionBlocker, DecisionBlockers,
    };
    use std::collections::BTreeMap;
    use std::ffi::OsStr;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum JsonType {
        String,
        Number,
        Null,
        StringArray,
    }

    #[derive(Debug, PartialEq)]
    enum JsonValue<'source> {
        String(&'source str),
        Number,
        Null,
        StringArray(Vec<&'source str>),
    }

    impl JsonValue<'_> {
        const fn kind(&self) -> JsonType {
            match self {
                Self::String(_) => JsonType::String,
                Self::Number => JsonType::Number,
                Self::Null => JsonType::Null,
                Self::StringArray(_) => JsonType::StringArray,
            }
        }
    }

    #[derive(Clone, Copy)]
    enum SchemaType {
        Exact(JsonType),
        Nullable(JsonType),
    }

    impl SchemaType {
        fn accepts(self, actual: JsonType) -> bool {
            match self {
                Self::Exact(expected) => actual == expected,
                Self::Nullable(expected) => actual == expected || actual == JsonType::Null,
            }
        }
    }

    struct FlatJsonParser<'source> {
        source: &'source str,
        offset: usize,
    }

    impl<'source> FlatJsonParser<'source> {
        fn parse(source: &'source str) -> BTreeMap<&'source str, JsonValue<'source>> {
            let mut parser = Self { source, offset: 0 };
            parser.expect(b'{');
            let mut object = BTreeMap::new();
            if parser.take(b'}') {
                assert_eq!(parser.offset, source.len());
                return object;
            }
            loop {
                let key = parser.string();
                parser.expect(b':');
                let previous = object.insert(key, parser.value());
                assert!(previous.is_none(), "duplicate JSON key {key}");
                if parser.take(b'}') {
                    break;
                }
                parser.expect(b',');
            }
            assert_eq!(parser.offset, source.len(), "trailing JSON input");
            object
        }

        fn value(&mut self) -> JsonValue<'source> {
            match self.peek() {
                Some(b'"') => JsonValue::String(self.string()),
                Some(b'[') => JsonValue::StringArray(self.string_array()),
                Some(b'n') => {
                    self.literal(b"null");
                    JsonValue::Null
                }
                Some(b'-' | b'0'..=b'9') => {
                    self.number();
                    JsonValue::Number
                }
                byte => panic!("invalid JSON value at {}: {byte:?}", self.offset),
            }
        }

        fn string(&mut self) -> &'source str {
            self.expect(b'"');
            let start = self.offset;
            loop {
                match self.peek() {
                    Some(b'"') => {
                        let value = &self.source[start..self.offset];
                        self.offset += 1;
                        return value;
                    }
                    Some(b'\\' | 0..=31) => {
                        panic!("trace JSON strings must not require escaping")
                    }
                    Some(_) => self.offset += 1,
                    None => panic!("unterminated JSON string"),
                }
            }
        }

        fn string_array(&mut self) -> Vec<&'source str> {
            self.expect(b'[');
            let mut values = Vec::new();
            if self.take(b']') {
                return values;
            }
            loop {
                values.push(self.string());
                if self.take(b']') {
                    return values;
                }
                self.expect(b',');
            }
        }

        fn number(&mut self) {
            self.take(b'-');
            match self.peek() {
                Some(b'0') => {
                    self.offset += 1;
                    assert!(!matches!(self.peek(), Some(b'0'..=b'9')));
                }
                Some(b'1'..=b'9') => self.digits(),
                byte => panic!("invalid JSON number at {}: {byte:?}", self.offset),
            }
            if self.take(b'.') {
                assert!(matches!(self.peek(), Some(b'0'..=b'9')));
                self.digits();
            }
            if matches!(self.peek(), Some(b'e' | b'E')) {
                self.offset += 1;
                if matches!(self.peek(), Some(b'+' | b'-')) {
                    self.offset += 1;
                }
                assert!(matches!(self.peek(), Some(b'0'..=b'9')));
                self.digits();
            }
        }

        fn digits(&mut self) {
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.offset += 1;
            }
        }

        fn literal(&mut self, expected: &[u8]) {
            let end = self.offset + expected.len();
            assert_eq!(self.source.as_bytes().get(self.offset..end), Some(expected));
            self.offset = end;
        }

        fn peek(&self) -> Option<u8> {
            self.source.as_bytes().get(self.offset).copied()
        }

        fn take(&mut self, expected: u8) -> bool {
            if self.peek() != Some(expected) {
                return false;
            }
            self.offset += 1;
            true
        }

        fn expect(&mut self, expected: u8) {
            assert!(
                self.take(expected),
                "expected byte {expected:?} at {}",
                self.offset
            );
        }
    }

    #[test]
    fn portable_trace_token_validation_rejects_paths_and_device_names() {
        for token in ["run", "run-12_A", "0"] {
            validate_token(token).unwrap();
        }
        for token in [
            "",
            ".",
            "..",
            "a/b",
            "a\\b",
            "with space",
            "CON",
            "com1",
            "lPt9",
        ] {
            assert!(validate_token(token).is_err(), "token {token:?}");
        }
        assert!(validate_token(&"a".repeat(65)).is_err());
        assert_eq!(TRACE_ENV, "KIO_DEBUG_CI_SCHEDULER_TRACE");
    }

    #[test]
    fn generic_program_attribution_uses_only_the_opaque_basename() {
        assert_eq!(
            program_basename_hex(Some(OsStr::new("opaque/path/custom-driver"))),
            "637573746f6d2d647269766572"
        );
        assert_eq!(
            program_basename_hex(Some(OsStr::new("opaque\\path\\custom.exe"))),
            "637573746f6d2e657865"
        );
        assert_eq!(program_basename_hex(None), "");
    }

    #[test]
    fn trace_configuration_stays_below_the_schedule_root() {
        let root =
            std::env::temp_dir().join(format!("kio-compiler-trace-config-{}", std::process::id()));
        let config = CompilerTraceConfig::for_test(&root, "run");
        assert_eq!(
            config.directory(),
            root.join("debug/compiler-admission/run")
        );
    }

    fn admitted(ticket: u64) -> AdmissionEvaluation {
        AdmissionEvaluation {
            decision: AdmissionDecision::Admit,
            blockers: DecisionBlockers::default(),
            ticket: Some(ticket),
            active_total: Some(0),
            active_adaptive: Some(0),
            pending_total: Some(1),
            oldest_pending_ticket: Some(ticket),
            fixed_capacity_min: Some(1),
            effective_limit: Some(1),
            candidate_slot: Some(1),
            adaptive_capacity: Some(2),
        }
    }

    fn assert_v3_schema(values: &BTreeMap<&str, JsonValue<'_>>) {
        use JsonType::{Number, String, StringArray};
        use SchemaType::{Exact, Nullable};

        let expected = BTreeMap::from([
            ("schema", Exact(String)),
            ("wall_anchor_unix_ms", Exact(Number)),
            ("elapsed_us", Exact(Number)),
            ("pid", Exact(Number)),
            ("acquisition_sequence", Exact(Number)),
            ("ticket", Nullable(Number)),
            ("requested_capacity", Exact(Number)),
            ("mode", Exact(String)),
            ("program_basename_hex", Exact(String)),
            ("active_total", Nullable(Number)),
            ("active_adaptive", Nullable(Number)),
            ("pending_total", Nullable(Number)),
            ("oldest_pending_ticket", Nullable(Number)),
            ("fixed_capacity_min", Nullable(Number)),
            ("effective_limit", Nullable(Number)),
            ("candidate_slot", Nullable(Number)),
            ("adaptive_capacity", Nullable(Number)),
            ("decision", Exact(String)),
            ("blockers", Exact(StringArray)),
        ]);

        assert_eq!(values.len(), expected.len(), "schema field set changed");
        for (name, expected_type) in expected {
            let actual = values
                .get(name)
                .unwrap_or_else(|| panic!("missing schema field {name}"));
            assert!(
                expected_type.accepts(actual.kind()),
                "schema field {name} has type {:?}",
                actual.kind()
            );
        }
        assert_eq!(values.get("schema"), Some(&JsonValue::String(SCHEMA)));
    }

    #[test]
    fn produced_lines_are_valid_v3_json_with_the_required_field_types() {
        let root =
            std::env::temp_dir().join(format!("kio-compiler-trace-schema-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let config = CompilerTraceConfig::for_test(&root, "schema");
        let mut trace = config
            .start(Some(OsStr::new("generic-program")), "adaptive", 2, 1)
            .unwrap();
        trace.record(admitted(1)).unwrap();

        let mut waiting = admitted(1);
        waiting.decision = AdmissionDecision::Wait;
        waiting.blockers = DecisionBlockers::one(DecisionBlocker::AdaptiveCapacity);
        trace.record(waiting).unwrap();

        let contents = std::fs::read_to_string(trace.path()).unwrap();
        let lines = contents.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 2);
        let admitted = FlatJsonParser::parse(lines[0]);
        let waiting = FlatJsonParser::parse(lines[1]);
        assert_v3_schema(&admitted);
        assert_v3_schema(&waiting);
        assert_eq!(
            waiting.get("blockers"),
            Some(&JsonValue::StringArray(vec!["adaptive-cap"]))
        );
        drop(trace);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn repeat_tickets_use_unique_process_acquisition_files() {
        let root =
            std::env::temp_dir().join(format!("kio-compiler-trace-unique-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let config = CompilerTraceConfig::for_test(&root, "repeat");
        let mut first = config
            .start(Some(OsStr::new("one")), "fixed", 1, 1)
            .unwrap();
        let mut second = config
            .start(Some(OsStr::new("two")), "fixed", 1, 1)
            .unwrap();
        first.record(admitted(1)).unwrap();
        second.record(admitted(1)).unwrap();

        assert_ne!(first.path(), second.path());
        let first_line = std::fs::read_to_string(first.path()).unwrap();
        let second_line = std::fs::read_to_string(second.path()).unwrap();
        assert!(first_line.contains("\"ticket\":1"));
        assert!(second_line.contains("\"ticket\":1"));
        assert_ne!(
            first_line
                .split("\"acquisition_sequence\":")
                .nth(1)
                .and_then(|value| value.split(',').next()),
            second_line
                .split("\"acquisition_sequence\":")
                .nth(1)
                .and_then(|value| value.split(',').next())
        );
        drop((first, second));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn trace_records_generic_attribution_without_the_program_path() {
        let root =
            std::env::temp_dir().join(format!("kio-compiler-trace-program-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let config = CompilerTraceConfig::for_test(&root, "program");
        let mut trace = config
            .start(
                Some(OsStr::new("private/path/custom-driver")),
                "adaptive",
                2,
                1,
            )
            .unwrap();
        trace.record(admitted(1)).unwrap();
        let line = std::fs::read_to_string(trace.path()).unwrap();
        assert!(line.contains("\"program_basename_hex\":\"637573746f6d2d647269766572\""));
        assert!(!line.contains("private/path"));
        assert!(!line.contains("custom-driver"));
        drop(trace);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn unchanged_waits_deduplicate_but_semantic_transitions_are_recorded() {
        let root = std::env::temp_dir().join(format!(
            "kio-compiler-trace-transitions-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let config = CompilerTraceConfig::for_test(&root, "transitions");
        let mut trace = config
            .start(Some(OsStr::new("compiler")), "adaptive", 2, 2)
            .unwrap();
        for blocker in [
            DecisionBlocker::UnknownLiveClaim,
            DecisionBlocker::NotFifoHead,
            DecisionBlocker::AdaptiveCapacity,
            DecisionBlocker::FixedCapacity,
        ] {
            let mut wait = admitted(2);
            wait.decision = AdmissionDecision::Wait;
            wait.blockers = DecisionBlockers::one(blocker);
            trace.record(wait).unwrap();
            trace.record(wait).unwrap();
        }

        trace.record(admitted(2)).unwrap();

        let contents = std::fs::read_to_string(trace.path()).unwrap();
        assert_eq!(contents.lines().count(), 5);
        assert_eq!(contents.matches("\"decision\":\"wait\"").count(), 4);
        assert_eq!(contents.matches("\"decision\":\"admit\"").count(), 1);
        drop(trace);
        let _ = std::fs::remove_dir_all(root);
    }
}
