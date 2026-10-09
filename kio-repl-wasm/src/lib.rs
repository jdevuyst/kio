use std::cell::OnceCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use kio_lang::cmd::check::AnalysisFailure;
use kio_lang::repl_core::commands::{Command, parse_command};
use kio_lang::repl_core::completion::{self, AstScopeProvider, NameSet};
use kio_lang::repl_core::highlight::Palette;
use kio_lang::repl_core::session::Session;
use serde::Serialize;
use wasm_bindgen::prelude::*;

include!(concat!(env!("OUT_DIR"), "/poc_sources.rs"));

#[derive(Serialize)]
struct Turn {
    output: String,
    keep_running: bool,
    is_error: bool,
}

#[derive(Serialize)]
struct JsError {
    message: String,
}

#[derive(Serialize)]
struct PocInfo {
    id: String,
}

/// The candidates and replacement span for a completion request, shaped
/// for the browser dropdown. `replace_start` / `replace_end` are byte
/// offsets into the input line an accepted candidate replaces.
#[derive(Serialize)]
struct CompletionResult {
    #[serde(rename = "replaceStart")]
    replace_start: usize,
    #[serde(rename = "replaceEnd")]
    replace_end: usize,
    candidates: Vec<CandidateInfo>,
}

/// One completion candidate for the browser dropdown: the text an accept
/// inserts, a short kind badge, and the optional menu detail column.
#[derive(Serialize)]
struct CandidateInfo {
    label: String,
    kind: String,
    detail: Option<String>,
}

#[wasm_bindgen]
pub struct Repl {
    poc_id: String,
    session: Session,
    init_banner: String,
    completion_names: OnceCell<NameSet>,
}

#[wasm_bindgen]
impl Repl {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Result<Repl, JsValue> {
        console_error_panic_hook::set_once();
        Self::for_poc(default_poc_id())
    }

    pub fn init_banner(&self) -> JsValue {
        to_js(Turn {
            output: self.init_banner.clone(),
            keep_running: true,
            is_error: false,
        })
    }

    pub fn current_poc(&self) -> String {
        self.poc_id.clone()
    }

    pub fn pocs(&self) -> JsValue {
        let pocs: Vec<PocInfo> = poc_ids().into_iter().map(|id| PocInfo { id }).collect();
        to_js(pocs)
    }

    pub fn switch_poc(&mut self, id: &str) -> Result<JsValue, JsValue> {
        let next = Self::for_poc(id)?;
        *self = next;
        Ok(self.init_banner())
    }

    pub fn eval(&mut self, input: &str) -> Result<JsValue, JsValue> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Ok(to_js(Turn {
                output: String::new(),
                keep_running: true,
                is_error: false,
            }));
        }

        let command = parse_command(trimmed).map_err(|error| js_error(error.0))?;
        let outcome = self.run_command(command);
        Ok(to_js(Turn {
            output: outcome.output,
            keep_running: outcome.keep_running,
            is_error: false,
        }))
    }

    /// The completion candidates for the cursor at byte `pos` in `input`
    /// — the browser dropdown's candidate source, matching the terminal
    /// `kio repl`'s completion. Requests share the current session's
    /// name set until a command replaces that session snapshot.
    pub fn complete(&self, input: &str, pos: usize) -> JsValue {
        let completion = self.complete_core(input, pos);
        to_js(CompletionResult {
            replace_start: completion.replace.start,
            replace_end: completion.replace.end,
            candidates: completion
                .candidates
                .into_iter()
                .map(|candidate| CandidateInfo {
                    label: candidate.label,
                    kind: candidate.kind.tag().to_owned(),
                    detail: candidate.description,
                })
                .collect(),
        })
    }
}

impl Repl {
    fn names(&self) -> &NameSet {
        self.completion_names
            .get_or_init(|| NameSet::from_session(&self.session))
    }

    fn complete_core(&self, input: &str, pos: usize) -> completion::Completion {
        let names = self.names();
        completion::complete(names, &AstScopeProvider::new(names), input, pos)
    }

    fn run_command(&mut self, command: Command) -> kio_lang::repl_core::commands::Outcome {
        let outcome = command.run(&mut self.session, browser_palette());
        self.completion_names.take();
        outcome
    }

    fn for_poc(id: &str) -> Result<Self, JsValue> {
        Self::try_for_poc(id).map_err(js_error)
    }

    fn try_for_poc(id: &str) -> Result<Self, String> {
        let id = id.trim();
        if !poc_ids().iter().any(|known| known == id) {
            return Err(format!("unknown example `{id}`"));
        }

        let root = poc_root(id);
        let files = source_map_for(id, &root)?;
        let mut session = Session::new_in_memory(root, files);
        session
            .refresh()
            .map_err(|failure| render_failure(&failure))?;
        let output = load_startup_commands(id, &mut session)?;
        Ok(Self {
            poc_id: id.to_owned(),
            session,
            init_banner: format!("Kio REPL\nExample: {id}\n{output}"),
            completion_names: OnceCell::new(),
        })
    }
}

fn default_poc_id() -> &'static str {
    "list"
}

fn poc_ids() -> Vec<String> {
    let mut ids: Vec<String> = POC_SOURCES
        .iter()
        .filter_map(|(rel, _)| rel.split_once("/workdir/").map(|(id, _)| id.to_owned()))
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

fn poc_root(id: &str) -> PathBuf {
    PathBuf::from(format!("/kio-wasm/{id}"))
}

fn source_map_for(id: &str, root: &Path) -> Result<BTreeMap<PathBuf, String>, String> {
    let mut files = BTreeMap::new();
    let prefix = format!("{id}/workdir/");
    for (rel, text) in POC_SOURCES {
        if let Some(workdir_rel) = rel.strip_prefix(&prefix)
            && !workdir_rel.starts_with("demo/")
            && Path::new(workdir_rel)
                .file_name()
                .and_then(|s| s.to_str())
                .is_some_and(kio_lang::file_kind::has_kio_extension)
        {
            files.insert(root.join(workdir_rel), (*text).to_owned());
        }
    }
    if files.is_empty() {
        Err(format!("example `{id}` has no sources"))
    } else {
        Ok(files)
    }
}

fn load_startup_commands(id: &str, session: &mut Session) -> Result<String, String> {
    let mut output = load_startup_module(id, session)?;
    if id == "list" {
        append_startup_command(&mut output, session, Command::Source("filter".to_owned()))?;
    }
    Ok(output)
}

fn append_startup_command(
    output: &mut String,
    session: &mut Session,
    command: Command,
) -> Result<(), String> {
    let outcome = command.run(session, browser_palette());
    if !outcome.keep_running {
        return Err("startup command stopped the REPL".to_owned());
    }
    if !outcome.output.trim().is_empty() {
        output.push_str("\n\n");
        output.push_str(outcome.output.trim_end());
    }
    Ok(())
}

fn load_startup_module(id: &str, session: &mut Session) -> Result<String, String> {
    let modules = session.package_module_paths();
    let Some(module) = startup_module(id, &modules) else {
        return Ok("no modules available".to_owned());
    };
    let outcome = Command::Load(module.clone()).run(session, browser_palette());
    if session.module(&module).is_none() {
        return Err(outcome.output);
    }
    Ok(outcome.output)
}

fn startup_module(id: &str, modules: &[String]) -> Option<String> {
    if modules.iter().any(|module| module == id) {
        return Some(id.to_owned());
    }

    if id == "elab" && modules.iter().any(|module| module == "spine_elaborators") {
        return Some("spine_elaborators".to_owned());
    }

    let demo_testapi_main = "demo/testapi/main";
    if modules.iter().any(|module| module == demo_testapi_main) {
        return Some(demo_testapi_main.to_owned());
    }

    modules
        .iter()
        .filter(|module| module.ends_with("/main"))
        .min_by(|left, right| {
            (left.matches('/').count(), left.len(), left.as_str()).cmp(&(
                right.matches('/').count(),
                right.len(),
                right.as_str(),
            ))
        })
        .or_else(|| modules.first())
        .cloned()
}

fn browser_palette() -> Palette {
    Palette::truecolor()
}

fn render_failure(failure: &AnalysisFailure) -> String {
    let located = failure.primary_error();
    let (span, message) = located.error.diag();
    if let Some(src) = failure.sources.get(&located.file_path) {
        let (line, col) = line_col(src, span.start);
        format!("{}:{line}:{col}: {message}", located.file_path.display())
    } else {
        format!("{}: {message}", located.file_path.display())
    }
}

fn line_col(source: &str, offset: u32) -> (usize, usize) {
    let off = (offset as usize).min(source.len());
    let mut line = 1;
    let mut col = 1;
    for ch in source[..off].chars() {
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

fn to_js<T: Serialize>(value: T) -> JsValue {
    serde_wasm_bindgen::to_value(&value).expect("serializing wasm response succeeds")
}

fn js_error(message: String) -> JsValue {
    to_js(JsError { message })
}

#[cfg(test)]
fn trailing_blocks_repl() -> Repl {
    let root = PathBuf::from("/kio-wasm-tests/trailing-blocks");
    let files = BTreeMap::from([
        (
            root.join("app.pkg.kio"),
            "package app; bridge { app; provider; }".to_owned(),
        ),
        (
            root.join("app.kio"),
            include_str!("../../kio-rs/tests/fixtures/trailing-blocks/app.kio").to_owned(),
        ),
        (
            root.join("provider.kio"),
            include_str!("../../kio-rs/tests/fixtures/trailing-blocks/provider.kio").to_owned(),
        ),
    ]);
    Repl {
        poc_id: "trailing-blocks".to_owned(),
        session: Session::new_in_memory(root, files),
        init_banner: String::new(),
        completion_names: OnceCell::new(),
    }
}

#[cfg(all(test, target_arch = "wasm32"))]
pub mod wasm_tests;

#[cfg(test)]
fn type_only_values_repl() -> Repl {
    let root = PathBuf::from("/kio-wasm-tests/type-only-values");
    let files = BTreeMap::from([
        (
            root.join("app.pkg.kio"),
            "package app; bridge { app; }".to_owned(),
        ),
        (
            root.join("app.kio"),
            "module app; type Existing = .; \
             fn mark[A] { () } \
             fn delayed[A]() { () }"
                .to_owned(),
        ),
    ]);
    Repl {
        poc_id: "type-only-values".to_owned(),
        session: Session::new_in_memory(root, files),
        init_banner: String::new(),
        completion_names: OnceCell::new(),
    }
}

#[cfg(test)]
fn reference_ownership_repl() -> Repl {
    let root = PathBuf::from("/kio-wasm-tests/reference-ownership");
    let files = [
        (
            "demo/a.kio",
            include_str!(
                "../../test-data/goldens/00_success/repl_reference_ownership/workdir/demo/a.kio"
            ),
        ),
        (
            "demo/b.kio",
            include_str!(
                "../../test-data/goldens/00_success/repl_reference_ownership/workdir/demo/b.kio"
            ),
        ),
        (
            "demo/main.kio",
            include_str!(
                "../../test-data/goldens/00_success/repl_reference_ownership/workdir/demo/main.kio"
            ),
        ),
    ]
    .into_iter()
    .map(|(path, source)| (root.join(path), source.to_owned()))
    .collect();
    Repl {
        poc_id: "reference-ownership".to_owned(),
        session: Session::new_in_memory(root, files),
        init_banner: String::new(),
        completion_names: OnceCell::new(),
    }
}

#[cfg(test)]
fn declaration_selectors_repl() -> Repl {
    let root = PathBuf::from("/kio-wasm-tests/selectors");
    let files = [
        ("demo/main.kio", include_str!("../../test-data/goldens/00_success/repl_declaration_selectors/workdir/demo/main.kio")),
        ("demo/ops.kio", include_str!("../../test-data/goldens/00_success/repl_declaration_selectors/workdir/demo/ops.kio")),
        ("demo/other.kio", include_str!("../../test-data/goldens/00_success/repl_declaration_selectors/workdir/demo/other.kio")),
    ].into_iter().map(|(path, source)| (root.join(path), source.to_owned())).collect();
    Repl {
        poc_id: "selectors".to_owned(),
        session: Session::new_in_memory(root, files),
        init_banner: String::new(),
        completion_names: OnceCell::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbered_binding_browser_completion_and_normalization() {
        let mut repl = trailing_blocks_repl();
        repl.run_command(Command::Load("app".to_owned()));
        let expression = ".() { let .x. = ((), ()); x2 }()";
        let output = strip_ansi(
            &repl
                .run_command(Command::Normalize(expression.to_owned()))
                .output,
        );
        assert_eq!(output.trim(), "()", "{output}");
        let input = ".() { let .x. = (); x3; let .x. = (); x";
        let completion = repl.complete_core(input, input.len());
        let numbered: Vec<_> = completion
            .candidates
            .iter()
            .filter(|item| item.label.starts_with('x'))
            .collect();
        assert_eq!(numbered.len(), 1, "{completion:?}");
        assert_eq!(numbered[0].label, "x1");
        assert_eq!(completion.replace, input.len() - 1..input.len());
    }

    #[test]
    fn reference_ownership_uses_the_in_memory_snapshot() {
        let mut repl = reference_ownership_repl();
        let loaded = repl.run_command(Command::Load("demo/main".to_owned()));
        assert!(strip_ansi(&loaded.output).contains("loaded demo/main"));
        let refs = strip_ansi(&repl.run_command(Command::Refs("truth".to_owned())).output);
        assert!(
            refs.contains("demo/a:") && refs.contains("in fn literal_use"),
            "{refs}"
        );
        assert!(
            !refs.contains("demo/b:") && !refs.contains("shadow"),
            "{refs}"
        );
    }

    #[test]
    fn default_poc_is_list() {
        assert_eq!(default_poc_id(), "list");
    }

    #[test]
    fn declaration_selectors_preserve_browser_rendering_and_insertion() {
        let mut repl = declaration_selectors_repl();
        let loaded = repl.run_command(Command::Load("demo/main".to_owned()));
        assert!(
            strip_ansi(&loaded.output).contains("loaded demo/main"),
            "{}",
            loaded.output
        );
        let listing = repl
            .run_command(Command::Ls {
                module: Some("demo/ops".to_owned()),
                verbose: false,
            })
            .output;
        for keyword in ["op", "varop"] {
            assert!(
                listing.contains(&format!(
                    "\x1b[38;2;229;192;123mpub\x1b[0m \x1b[38;2;229;192;123m{keyword}\x1b[0m"
                )),
                "{listing:?}"
            );
        }
        let line = ":sig arithmetic.(op _ +";
        let completion = repl.complete_core(line, line.len());
        let selected = completion
            .candidates
            .iter()
            .find(|item| item.label == "arithmetic.(op _ + _)")
            .unwrap();
        let inserted = format!(
            "{}{}{}",
            &line[..completion.replace.start],
            selected.label,
            &line[completion.replace.end..]
        );
        let command = parse_command(&inserted).unwrap();
        assert_eq!(
            strip_ansi(&repl.run_command(command).output).trim(),
            "pub op _ + _ { impl choose }"
        );
        repl.run_command(Command::Reset);
        assert!(
            !repl
                .complete_core(line, line.len())
                .candidates
                .iter()
                .any(|item| item.label == selected.label)
        );
    }

    #[test]
    fn trailing_blocks_preserve_browser_command_and_completion_scope() {
        let mut repl = trailing_blocks_repl();
        assert!(
            strip_ansi(&repl.run_command(Command::Load("app".to_owned())).output)
                .contains("loaded app")
        );
        for source in ["packet! { () }", "enter! { let value = (); value }"] {
            let output = strip_ansi(
                &repl
                    .run_command(Command::Normalize(source.to_owned()))
                    .output,
            );
            assert_eq!(output.trim(), "()", "{source}: {output}");
        }
        for (source, present, absent) in [
            ("enter! { let local = (); loc", "local", "other"),
            ("enter! { let other = (); ", "other", "local"),
        ] {
            let completion = repl.complete_core(source, source.len());
            assert!(
                completion
                    .candidates
                    .iter()
                    .any(|item| item.label == present),
                "{source}: {completion:?}"
            );
            assert!(
                !completion
                    .candidates
                    .iter()
                    .any(|item| item.label == absent),
                "{source}: {completion:?}"
            );
        }
        repl.run_command(Command::Reset);
        assert!(
            !repl
                .complete_core("packet", 6)
                .candidates
                .iter()
                .any(|item| item.label == "packet!")
        );
    }

    #[test]
    fn recursive_annotations_have_no_owner_in_browser_prompts() {
        let mut repl = trailing_blocks_repl();
        repl.run_command(Command::Load("app".to_owned()));
        for source in ["recur(", "recur(cont, ", ".() { recur(", "enter! { recur("] {
            let result = repl.complete_core(source, source.len());
            assert!(result.candidates.is_empty(), "{source}: {result:?}");
        }
    }

    #[test]
    fn type_only_values_preserve_browser_stages_and_binder_edits() {
        let mut repl = type_only_values_repl();
        let loaded = repl.run_command(Command::Load("app".to_owned()));
        assert!(strip_ansi(&loaded.output).contains("loaded app"));
        for (input, expected) in [
            (":type mark", "mark : [A] ."),
            (":type .[B] { () }", ".[B] { () } : [B] ."),
            (":normalize mark(.)", "()"),
            (":normalize .[B] { () }(.)", "()"),
            (":type delayed(.)", "delayed(.) : . -> ."),
            (":normalize delayed(., ())", "()"),
        ] {
            let outcome = repl.run_command(parse_command(input).unwrap());
            assert!(outcome.keep_running, "{input}: {outcome:?}");
            assert_eq!(strip_ansi(&outcome.output).trim(), expected, "{input}");
        }
        for input in [":normalize mark", ":normalize delayed(.)"] {
            let output = strip_ansi(&repl.run_command(parse_command(input).unwrap()).output);
            let residual = output.trim();
            assert!(
                residual.starts_with("<closure ") && residual.ends_with('>'),
                "{input}: {residual}"
            );
        }
        for input in [":normalize .[", ":normalize .[Sco"] {
            let result = repl.complete_core(input, input.len());
            assert!(result.candidates.is_empty(), "{input}: {result:?}");
        }
        for (binder, prefix) in [("Scoped", "Sco"), ("Other", "Ot")] {
            let input = format!(":normalize .[{binder}] {{ .(value: {prefix}");
            let result = repl.complete_core(&input, input.len());
            assert_eq!(result.replace, input.len() - prefix.len()..input.len());
            assert_eq!(result.candidates.len(), 1, "{input}: {result:?}");
            let selected = &result.candidates[0];
            assert_eq!(selected.label, binder);
            assert_eq!(selected.kind.tag(), "type-param");
            let outside = format!(":normalize .(value: {prefix}");
            assert!(
                repl.complete_core(&outside, outside.len())
                    .candidates
                    .is_empty(),
                "{binder}: binding leaked outside its scope"
            );
            let inserted = format!(
                "{}{}{}) {{ value }} }}(., ())",
                &input[..result.replace.start],
                selected.label,
                &input[result.replace.end..]
            );
            let outcome = repl.run_command(parse_command(&inserted).unwrap());
            assert_eq!(strip_ansi(&outcome.output).trim(), "()", "{inserted}");
        }
    }

    #[test]
    fn completion_snapshot_is_reused_and_invalidated_by_commands() {
        let root = PathBuf::from("/kio-wasm-tests/completion");
        let files = BTreeMap::from([
            (
                root.join("app.pkg.kio"),
                "package app; bridge { app/**; }".to_owned(),
            ),
            (
                root.join("app/first.kio"),
                "module app/first; fn first() { () }".to_owned(),
            ),
            (
                root.join("app/second.kio"),
                "module app/second; fn second() { () }".to_owned(),
            ),
        ]);
        let mut repl = Repl {
            poc_id: "completion".to_owned(),
            session: Session::new_in_memory(root, files),
            init_banner: String::new(),
            completion_names: OnceCell::new(),
        };
        for (selected, absent) in [("first", "second"), ("second", "first")] {
            let loaded = repl.run_command(Command::Load(format!("app/{selected}")));
            assert!(
                strip_ansi(&loaded.output).contains(&format!("loaded app/{selected}")),
                "{}",
                loaded.output
            );
            assert!(repl.completion_names.get().is_none());
            let snapshot = repl.names() as *const NameSet;
            for _ in 0..3 {
                let result = repl.complete_core(selected, selected.len());
                assert!(result.candidates.iter().any(|item| item.label == selected));
                assert!(!result.candidates.iter().any(|item| item.label == absent));
                assert!(std::ptr::eq(repl.names(), snapshot));
            }
        }
        repl.run_command(Command::Load("app/missing".to_owned()));
        assert!(repl.completion_names.get().is_none());
        assert!(
            repl.complete_core("second", 6)
                .candidates
                .iter()
                .any(|item| item.label == "second")
        );
        repl.run_command(Command::Reset);
        assert!(repl.completion_names.get().is_none());
        assert!(
            !repl
                .complete_core("second", 6)
                .candidates
                .iter()
                .any(|item| item.label == "second")
        );
    }

    #[test]
    fn list_startup_prints_filter_source() {
        std::thread::Builder::new()
            .stack_size(64 * 1024 * 1024)
            .spawn(|| {
                let repl = Repl::try_for_poc("list").expect("list POC loads");
                // `browser_palette()` colours the banner, so anchor on the
                // declaration the `:source filter` startup command renders,
                // not on comment prose that ordinary edits churn.
                let banner = strip_ansi(&repl.init_banner);
                assert!(banner.contains("Example: list"), "banner: {banner}");
                assert!(banner.contains("fn filter"), "banner: {banner}");
                assert!(!banner.contains("unresolved"), "banner: {banner}");
            })
            .expect("spawn startup test")
            .join()
            .expect("startup test passes");
    }

    /// Strip every ANSI SGR escape (`\x1b[…m`) from `s`, leaving the
    /// visible text so an assertion can match a multi-token span the
    /// palette split into separately-coloured runs.
    fn strip_ansi(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for e in chars.by_ref() {
                    if e == 'm' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn startup_module_prefers_the_shallowest_main_module() {
        let modules = vec![
            "core".to_owned(),
            "demo/elab/main".to_owned(),
            "demo/testapi/main".to_owned(),
            "testapi/text".to_owned(),
        ];
        assert_eq!(
            startup_module("hkt", &modules),
            Some("demo/testapi/main".to_owned())
        );
    }

    #[test]
    fn startup_module_prefers_root_library_matching_the_poc_id() {
        let modules = vec![
            "demo/testapi/main".to_owned(),
            "list".to_owned(),
            "core".to_owned(),
        ];
        assert_eq!(startup_module("list", &modules), Some("list".to_owned()));
    }

    #[test]
    fn startup_module_prefers_elab_library_module() {
        let modules = vec![
            "demo/elab/main".to_owned(),
            "spine_elaborators".to_owned(),
            "testapi".to_owned(),
        ];
        assert_eq!(
            startup_module("elab", &modules),
            Some("spine_elaborators".to_owned())
        );
    }

    #[test]
    fn browser_palette_enables_ansi_highlighting() {
        let highlighted =
            kio_lang::repl_core::highlight::highlight("fn run() -> . { () }", browser_palette());
        assert!(
            highlighted.contains("\x1b["),
            "browser palette should emit ANSI escapes for xterm"
        );
    }
}
