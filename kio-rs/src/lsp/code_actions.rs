//! LSP code-action support.
//!
//! Auto-import is a client-driven edit oracle, not a resolution rule.
//! It reads the typed package to suggest a fully-qualified import edit
//! for the current file; accepting the action changes only that file and
//! pins an explicit module path. Adding declarations elsewhere in the
//! package can add or remove suggestions, but it never changes how
//! existing code resolves unless the user applies an edit.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionParams, CodeActionResponse,
    Diagnostic, Position, Range, TextEdit, Uri,
};

use crate::ast::{ImportKind, Module, Surface};
use crate::cmd::check::LspAnalysis;
use crate::lsp::positions::{LineIndex, LspPosition};
use crate::lsp::util::workspace_edit_from_changes;
use crate::package_collection::SourceOverlay;

#[derive(Clone, Copy)]
pub(crate) struct FixValidationContext<'a> {
    pub package_root: &'a Path,
    pub focus_file: &'a Path,
    pub overlay: &'a SourceOverlay,
}

pub(crate) fn handle_code_action(
    params: CodeActionParams,
    source: Option<&str>,
    module: Option<&Module<Surface>>,
    analysis: Option<&LspAnalysis>,
    document_version: Option<i32>,
    fix_validation: Option<FixValidationContext<'_>>,
) -> Option<CodeActionResponse> {
    let mut actions = Vec::new();
    if code_action_kind_requested(&params.context, &CodeActionKind::QUICKFIX) {
        for diagnostic in &params.context.diagnostics {
            if !diagnostic_matches_document_version(diagnostic, document_version) {
                continue;
            }
            add_eager_fix_actions(
                &mut actions,
                &params.text_document.uri,
                diagnostic,
                document_version,
                source,
                fix_validation,
            );
            if let Some(name) = unresolved_name(diagnostic) {
                let auto_imports = auto_import_actions(
                    &params.text_document.uri,
                    diagnostic,
                    name,
                    source,
                    module,
                    analysis,
                    document_version,
                );
                let has_auto_import = !auto_imports.is_empty();
                actions.extend(auto_imports);
                if !has_auto_import {
                    add_stub_action(
                        &mut actions,
                        &params.text_document.uri,
                        diagnostic,
                        name,
                        source,
                        document_version,
                    );
                }
            }
        }
    }
    if code_action_kind_requested(&params.context, &CodeActionKind::SOURCE_FIX_ALL) {
        add_fix_all_action(
            &mut actions,
            &params.text_document.uri,
            &params.context.diagnostics,
            document_version,
            source,
            fix_validation,
        );
    }
    Some(actions)
}

fn code_action_kind_requested(
    context: &lsp_types::CodeActionContext,
    kind: &CodeActionKind,
) -> bool {
    context.only.as_ref().is_none_or(|requested| {
        requested.iter().any(|requested| {
            kind.as_str() == requested.as_str()
                || kind
                    .as_str()
                    .strip_prefix(requested.as_str())
                    .is_some_and(|suffix| suffix.starts_with('.'))
        })
    })
}

fn diagnostic_matches_document_version(
    diagnostic: &Diagnostic,
    current_version: Option<i32>,
) -> bool {
    let Some(current_version) = current_version else {
        return true;
    };
    diagnostic
        .data
        .as_ref()
        .and_then(|data| data.get("kioDocumentVersion"))
        .and_then(serde_json::Value::as_i64)
        .and_then(|version| i32::try_from(version).ok())
        == Some(current_version)
}

pub fn handle_code_action_resolve(action: CodeAction) -> CodeAction {
    let Some(data) = action.data.clone() else {
        return action;
    };
    let Some(kind) = data.get("kind").and_then(|v| v.as_str()) else {
        return action;
    };
    match kind {
        "autoImport" => resolve_auto_import(action, &data),
        "addStub" => resolve_add_stub(action, &data),
        _ => action,
    }
}

fn add_eager_fix_actions(
    actions: &mut Vec<CodeActionOrCommand>,
    uri: &Uri,
    diagnostic: &Diagnostic,
    document_version: Option<i32>,
    source: Option<&str>,
    fix_validation: Option<FixValidationContext<'_>>,
) {
    let Some(fixes) = diagnostic_fixes(diagnostic, source) else {
        return;
    };
    for fix in fixes {
        if let Some(validation) = fix_validation {
            let Some(source) = source else {
                continue;
            };
            let valid = if fix.scaffold {
                scaffold_is_current(source, &fix, validation, diagnostic)
            } else {
                fix_rechecks(
                    source,
                    &fix.edits,
                    validation,
                    std::slice::from_ref(diagnostic),
                    fix.follow_on_reanalysis_outside.as_ref(),
                )
            };
            if !valid {
                continue;
            }
        } else if document_version.is_some() {
            // An edit for an open document must never be advertised without
            // authenticating its current source and checking the complete
            // repaired module. The context-free path remains for unit-level
            // conversion and closed documents only.
            continue;
        }
        let versions = document_versions(uri, document_version);
        let edit = workspace_edit_from_changes(
            HashMap::from([(uri.clone(), fix.edits)]),
            versions.as_ref(),
        );
        actions.push(CodeActionOrCommand::CodeAction(CodeAction {
            title: fix.title,
            kind: Some(CodeActionKind::QUICKFIX),
            diagnostics: Some(vec![diagnostic.clone()]),
            edit: Some(edit),
            is_preferred: Some(fix.machine_applicable),
            ..Default::default()
        }));
    }
}

fn add_fix_all_action(
    actions: &mut Vec<CodeActionOrCommand>,
    uri: &Uri,
    diagnostics: &[Diagnostic],
    document_version: Option<i32>,
    source: Option<&str>,
    fix_validation: Option<FixValidationContext<'_>>,
) {
    let (Some(source), Some(validation)) = (source, fix_validation) else {
        return;
    };
    let mut targets = Vec::new();
    let mut edits = Vec::new();
    for diagnostic in diagnostics {
        if !diagnostic_matches_document_version(diagnostic, document_version) {
            continue;
        }
        let Some(fix) = diagnostic_fixes(diagnostic, Some(source))
            .and_then(|fixes| fixes.into_iter().find(|fix| fix.machine_applicable))
        else {
            continue;
        };
        targets.push(diagnostic.clone());
        edits.extend(fix.edits);
    }
    if targets.len() < 2 || !fix_rechecks(source, &edits, validation, &targets, None) {
        return;
    }
    let versions = document_versions(uri, document_version);
    let edit =
        workspace_edit_from_changes(HashMap::from([(uri.clone(), edits)]), versions.as_ref());
    actions.push(CodeActionOrCommand::CodeAction(CodeAction {
        title: "Fix all Kio diagnostics".to_owned(),
        kind: Some(CodeActionKind::SOURCE_FIX_ALL),
        diagnostics: Some(targets),
        edit: Some(edit),
        is_preferred: Some(false),
        ..Default::default()
    }));
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RecheckedDiagnostic {
    file: PathBuf,
    code: Option<i32>,
    message: String,
    span: crate::span::Span,
}

#[derive(Clone, Copy, Debug)]
struct ByteEdit {
    start: usize,
    end: usize,
    replacement_len: usize,
}

fn fix_rechecks(
    source: &str,
    edits: &[TextEdit],
    validation: FixValidationContext<'_>,
    targets: &[Diagnostic],
    follow_on_reanalysis_outside: Option<&Range>,
) -> bool {
    let Some(repaired_source) = apply_text_edits(source, edits) else {
        return false;
    };

    let line_index = LineIndex::new(source);
    let Some(byte_edits) = byte_edits(&line_index, edits) else {
        return false;
    };
    let follow_on_scope = follow_on_reanalysis_outside.and_then(|range| {
        Some(crate::span::Span::new(
            u32::try_from(exact_byte_offset(&line_index, range.start)?).ok()?,
            u32::try_from(exact_byte_offset(&line_index, range.end)?).ok()?,
        ))
    });
    if follow_on_reanalysis_outside.is_some() && follow_on_scope.is_none() {
        return false;
    }

    // Authenticate the diagnostic set against the same exact overlay used to
    // validate the repair. This permits one quick fix to coexist with other
    // independent current errors, without accepting a repair that introduces
    // a new error or merely moves the target diagnostic.
    let targets_are_warnings = targets.iter().all(|target| target.code.is_none());
    if targets
        .iter()
        .any(|target| target.code.is_none() != targets_are_warnings)
    {
        // A successful analysis publishes warnings; a failed analysis
        // publishes errors. One authentic response therefore never mixes the
        // two channels.
        return false;
    }
    let original = if targets_are_warnings {
        crate::cmd::check::analyze_workspace_at_with_overlay_lsp(
            validation.package_root,
            validation.overlay,
        )
    } else {
        crate::cmd::check::analyze_module_at_with_overlay_lsp(
            validation.package_root,
            validation.overlay,
            validation.focus_file,
        )
    };
    let mut matched_targets = vec![false; targets.len()];
    let mut expected = Vec::new();
    let original_was_clean = match &original {
        Err(failure) => {
            for located in &failure.errors {
                if let Some(target) =
                    targets
                        .iter()
                        .enumerate()
                        .find_map(|(target_index, target)| {
                            (!matched_targets[target_index]
                                && located_matches_lsp_diagnostic(
                                    located,
                                    target,
                                    validation.package_root,
                                    validation.focus_file,
                                    &line_index,
                                ))
                            .then_some(target_index)
                        })
                {
                    matched_targets[target] = true;
                    continue;
                }

                let mut diagnostic = rechecked_located_diagnostic(validation.package_root, located);
                if same_path(&diagnostic.file, validation.focus_file) {
                    let Some(span) = map_unaffected_span(diagnostic.span, &byte_edits) else {
                        // An individual action must not rewrite an independent
                        // diagnostic. A combined fix lists every affected target.
                        return false;
                    };
                    diagnostic.span = span;
                }
                expected.push(diagnostic);
            }
            false
        }
        Ok(analysis) => {
            for warning in &analysis.warnings {
                if let Some(target) =
                    targets
                        .iter()
                        .enumerate()
                        .find_map(|(target_index, target)| {
                            (!matched_targets[target_index]
                                && warning_matches_lsp_diagnostic(
                                    warning,
                                    target,
                                    validation.package_root,
                                    validation.focus_file,
                                    &line_index,
                                ))
                            .then_some(target_index)
                        })
                {
                    matched_targets[target] = true;
                    continue;
                }

                let mut diagnostic = rechecked_warning(validation.package_root, warning);
                if same_path(&diagnostic.file, validation.focus_file) {
                    let Some(span) = map_unaffected_span(diagnostic.span, &byte_edits) else {
                        return false;
                    };
                    diagnostic.span = span;
                }
                expected.push(diagnostic);
            }
            true
        }
    };
    if matched_targets.iter().any(|matched| !matched) {
        return false;
    }

    let mut overlay = validation.overlay.clone();
    overlay.insert(validation.focus_file.to_owned(), repaired_source.clone());
    let repaired = if targets_are_warnings {
        crate::cmd::check::analyze_workspace_at_with_overlay_lsp(validation.package_root, &overlay)
    } else {
        crate::cmd::check::analyze_module_at_with_overlay_lsp(
            validation.package_root,
            &overlay,
            validation.focus_file,
        )
    };
    let actual = match (original_was_clean, repaired) {
        (false, Ok(_)) => return true,
        (false, Err(failure)) => failure
            .errors
            .iter()
            .map(|located| rechecked_located_diagnostic(validation.package_root, located))
            .collect::<Vec<_>>(),
        (true, Ok(analysis)) => analysis
            .warnings
            .iter()
            .map(|warning| rechecked_warning(validation.package_root, warning))
            .collect::<Vec<_>>(),
        (true, Err(_)) => return false,
    };
    for actual in actual {
        if let Some(index) = expected.iter().position(|candidate| candidate == &actual) {
            expected.swap_remove(index);
            continue;
        }
        let Some(scope) = follow_on_scope else {
            return false;
        };
        if !same_path(&actual.file, validation.focus_file) {
            return false;
        }
        let Some(original_span) = map_repaired_unaffected_span(actual.span, &byte_edits) else {
            return false;
        };
        if spans_overlap(original_span, scope)
            || source.get(original_span.start as usize..original_span.end as usize)
                != repaired_source.get(actual.span.start as usize..actual.span.end as usize)
        {
            return false;
        }
    }
    true
}

fn rechecked_located_diagnostic(
    package_root: &Path,
    located: &crate::pass::resolve::LocatedError,
) -> RecheckedDiagnostic {
    let (span, message) = located.error.diag();
    RecheckedDiagnostic {
        file: absolute_diagnostic_path(package_root, &located.file_path),
        code: Some(located.error.exit_code().as_i32()),
        message: message.to_owned(),
        span,
    }
}

fn rechecked_warning(
    package_root: &Path,
    warning: &crate::cmd::check::LspWarning,
) -> RecheckedDiagnostic {
    RecheckedDiagnostic {
        file: absolute_diagnostic_path(package_root, &warning.file_path),
        code: None,
        message: warning.message.clone(),
        span: warning.span,
    }
}

fn located_matches_lsp_diagnostic(
    located: &crate::pass::resolve::LocatedError,
    diagnostic: &Diagnostic,
    package_root: &Path,
    focus_file: &Path,
    line_index: &LineIndex,
) -> bool {
    if !same_path(
        &absolute_diagnostic_path(package_root, &located.file_path),
        focus_file,
    ) {
        return false;
    }
    let (span, message) = located.error.diag();
    if message != diagnostic.message {
        return false;
    }
    let range = line_index.to_range(span);
    diagnostic.range.start.line == range.start.line
        && diagnostic.range.start.character == range.start.character
        && diagnostic.range.end.line == range.end.line
        && diagnostic.range.end.character == range.end.character
        && diagnostic.code.as_ref().is_none_or(|code| match code {
            lsp_types::NumberOrString::Number(code) => *code == located.error.exit_code().as_i32(),
            lsp_types::NumberOrString::String(_) => false,
        })
}

fn warning_matches_lsp_diagnostic(
    warning: &crate::cmd::check::LspWarning,
    diagnostic: &Diagnostic,
    package_root: &Path,
    focus_file: &Path,
    line_index: &LineIndex,
) -> bool {
    if !same_path(
        &absolute_diagnostic_path(package_root, &warning.file_path),
        focus_file,
    ) || diagnostic.code.is_some()
        || diagnostic.message != warning.message
    {
        return false;
    }
    let range = line_index.to_range(warning.span);
    diagnostic.range.start.line == range.start.line
        && diagnostic.range.start.character == range.start.character
        && diagnostic.range.end.line == range.end.line
        && diagnostic.range.end.character == range.end.character
}

fn absolute_diagnostic_path(package_root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        package_root.join(path)
    }
}

fn same_path(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    matches!(
        (left.canonicalize(), right.canonicalize()),
        (Ok(left), Ok(right)) if left == right
    )
}

fn byte_edits(index: &LineIndex, edits: &[TextEdit]) -> Option<Vec<ByteEdit>> {
    let mut result = Vec::with_capacity(edits.len());
    for edit in edits {
        let start = exact_byte_offset(index, edit.range.start)?;
        let end = exact_byte_offset(index, edit.range.end)?;
        if start > end {
            return None;
        }
        result.push(ByteEdit {
            start,
            end,
            replacement_len: edit.new_text.len(),
        });
    }
    result.sort_unstable_by_key(|edit| (edit.start, edit.end));
    if result.windows(2).any(|pair| pair[0].end > pair[1].start) {
        return None;
    }
    Some(result)
}

fn map_unaffected_span(span: crate::span::Span, edits: &[ByteEdit]) -> Option<crate::span::Span> {
    let start = span.start as usize;
    let end = span.end as usize;
    let mut delta = 0_i64;
    for edit in edits {
        let overlaps = if edit.start == edit.end {
            start < edit.start && edit.start < end
        } else {
            start < edit.end && edit.start < end
        };
        if overlaps {
            return None;
        }
        if edit.end <= start {
            delta += edit.replacement_len as i64 - (edit.end - edit.start) as i64;
        }
    }
    let mapped_start = i64::from(span.start).checked_add(delta)?;
    let mapped_end = i64::from(span.end).checked_add(delta)?;
    Some(crate::span::Span::new(
        u32::try_from(mapped_start).ok()?,
        u32::try_from(mapped_end).ok()?,
    ))
}

fn map_repaired_unaffected_span(
    span: crate::span::Span,
    edits: &[ByteEdit],
) -> Option<crate::span::Span> {
    let start = i64::from(span.start);
    let end = i64::from(span.end);
    let mut delta = 0_i64;
    for edit in edits {
        let repaired_start = i64::try_from(edit.start).ok()?.checked_add(delta)?;
        let repaired_end = repaired_start.checked_add(i64::try_from(edit.replacement_len).ok()?)?;
        let overlaps = if repaired_start == repaired_end {
            start < repaired_start && repaired_start < end
        } else {
            start < repaired_end && repaired_start < end
        };
        if overlaps {
            return None;
        }
        if repaired_end <= start {
            delta = delta.checked_add(
                i64::try_from(edit.replacement_len).ok()?
                    - i64::try_from(edit.end.checked_sub(edit.start)?).ok()?,
            )?;
        }
    }
    Some(crate::span::Span::new(
        u32::try_from(start.checked_sub(delta)?).ok()?,
        u32::try_from(end.checked_sub(delta)?).ok()?,
    ))
}

fn spans_overlap(left: crate::span::Span, right: crate::span::Span) -> bool {
    left.start < right.end && right.start < left.end
}

fn apply_text_edits(source: &str, edits: &[TextEdit]) -> Option<String> {
    let index = LineIndex::new(source);
    let mut byte_edits = Vec::with_capacity(edits.len());
    for edit in edits {
        let start = exact_byte_offset(&index, edit.range.start)?;
        let end = exact_byte_offset(&index, edit.range.end)?;
        if start > end {
            return None;
        }
        byte_edits.push((start, end, edit.new_text.as_str()));
    }
    byte_edits.sort_unstable_by_key(|(start, end, _)| (*start, *end));
    if byte_edits.windows(2).any(|pair| pair[0].1 > pair[1].0) {
        return None;
    }
    let mut repaired = source.to_owned();
    for (start, end, replacement) in byte_edits.into_iter().rev() {
        repaired.replace_range(start..end, replacement);
    }
    Some(repaired)
}

fn exact_byte_offset(index: &LineIndex, position: Position) -> Option<usize> {
    let lsp_position = LspPosition {
        line: position.line,
        character: position.character,
    };
    let offset = index.position_to_offset(lsp_position);
    (index.to_position(offset) == lsp_position).then_some(offset as usize)
}

fn auto_import_actions(
    uri: &Uri,
    diagnostic: &Diagnostic,
    name: &str,
    source: Option<&str>,
    module: Option<&Module<Surface>>,
    analysis: Option<&LspAnalysis>,
    document_version: Option<i32>,
) -> Vec<CodeActionOrCommand> {
    let (Some(module), Some(analysis)) = (module, analysis) else {
        return Vec::new();
    };
    let Some(insert_range) = source.and_then(|source| import_insert_range(module, source)) else {
        return Vec::new();
    };
    let current_path = module_path_string(module);
    let mut candidates = exported_modules(analysis, name);
    candidates.retain(|path| path != &current_path);
    candidates.sort_by(|a, b| {
        a.matches('/')
            .count()
            .cmp(&b.matches('/').count())
            .then_with(|| a.cmp(b))
    });
    let is_unique = candidates.len() == 1;
    let mut actions = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let alias = candidate.rsplit('/').next().unwrap_or(candidate.as_str());
        let alias_collision = module.imports.iter().any(|import_| {
            matches!(&import_.kind, ImportKind::Qualified { alias: existing, .. } if existing == alias)
        });
        let title = if alias_collision {
            format!("Import `{name}` from `{candidate}`")
        } else {
            format!("Import `{candidate}` as `{alias}`")
        };
        actions.push(CodeActionOrCommand::CodeAction(CodeAction {
            title,
            kind: Some(CodeActionKind::QUICKFIX),
            diagnostics: Some(vec![diagnostic.clone()]),
            is_preferred: Some(is_unique),
            data: Some(serde_json::json!({
                "kind": "autoImport",
                "uri": uri.as_str(),
                "name": name,
                "modulePath": candidate,
                "alias": alias,
                "aliasCollision": alias_collision,
                "diagnosticRange": diagnostic.range,
                "documentVersion": document_version,
                "insertRange": insert_range,
            })),
            ..Default::default()
        }));
    }
    actions
}

fn add_stub_action(
    actions: &mut Vec<CodeActionOrCommand>,
    uri: &Uri,
    diagnostic: &Diagnostic,
    name: &str,
    source: Option<&str>,
    document_version: Option<i32>,
) {
    let Some(source) = source else {
        return;
    };
    let arity = call_arity_after_name(source, diagnostic.range);
    actions.push(CodeActionOrCommand::CodeAction(CodeAction {
        title: format!("Add `{name}` declaration"),
        kind: Some(CodeActionKind::QUICKFIX),
        diagnostics: Some(vec![diagnostic.clone()]),
        is_preferred: Some(false),
        data: Some(serde_json::json!({
            "kind": "addStub",
            "uri": uri.as_str(),
            "name": name,
            "arity": arity,
            "documentVersion": document_version,
            "insertRange": eof_range(source),
        })),
        ..Default::default()
    }));
}

fn resolve_auto_import(mut action: CodeAction, data: &serde_json::Value) -> CodeAction {
    let Some(uri) = data
        .get("uri")
        .and_then(|v| v.as_str())
        .and_then(Uri::from_str_lossy)
    else {
        return action;
    };
    let Some(name) = data.get("name").and_then(|v| v.as_str()) else {
        return action;
    };
    let Some(module_path) = data.get("modulePath").and_then(|v| v.as_str()) else {
        return action;
    };
    let Some(alias) = data.get("alias").and_then(|v| v.as_str()) else {
        return action;
    };
    let alias_collision = data
        .get("aliasCollision")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let insert_range = data
        .get("insertRange")
        .and_then(|v| serde_json::from_value::<Range>(v.clone()).ok())
        .unwrap_or_default();
    let diagnostic_range = data
        .get("diagnosticRange")
        .and_then(|v| serde_json::from_value::<Range>(v.clone()).ok());
    let mut edits = Vec::new();
    if alias_collision {
        edits.push(TextEdit {
            range: insert_range,
            new_text: format!("\nimport {module_path}({name});"),
        });
    } else {
        edits.push(TextEdit {
            range: insert_range,
            new_text: format!("\nimport {module_path} as {alias};"),
        });
        if let Some(range) = diagnostic_range {
            edits.push(TextEdit {
                range,
                new_text: format!("{alias}.{name}"),
            });
        }
    }
    let versions = document_versions(&uri, document_version(data));
    action.edit = Some(workspace_edit_from_changes(
        HashMap::from([(uri, edits)]),
        versions.as_ref(),
    ));
    action
}

fn resolve_add_stub(mut action: CodeAction, data: &serde_json::Value) -> CodeAction {
    let Some(uri) = data
        .get("uri")
        .and_then(|v| v.as_str())
        .and_then(Uri::from_str_lossy)
    else {
        return action;
    };
    let Some(name) = data.get("name").and_then(|v| v.as_str()) else {
        return action;
    };
    let arity = data.get("arity").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    let insert_range = data
        .get("insertRange")
        .and_then(|v| serde_json::from_value::<Range>(v.clone()).ok())
        .unwrap_or_default();
    let params = (0..arity)
        .map(|i| format!("arg{i}: ."))
        .collect::<Vec<_>>()
        .join(", ");
    let text = format!("\n\nfn {name}({params}) -> . {{ () }}\n");
    let versions = document_versions(&uri, document_version(data));
    action.edit = Some(workspace_edit_from_changes(
        HashMap::from([(
            uri,
            vec![TextEdit {
                range: insert_range,
                new_text: text,
            }],
        )]),
        versions.as_ref(),
    ));
    action
}

fn document_version(data: &serde_json::Value) -> Option<i32> {
    data.get("documentVersion")
        .and_then(serde_json::Value::as_i64)
        .and_then(|version| i32::try_from(version).ok())
}

fn document_versions(uri: &Uri, version: Option<i32>) -> Option<BTreeMap<Uri, i32>> {
    version.map(|version| BTreeMap::from([(uri.clone(), version)]))
}

struct EagerFix {
    title: String,
    edits: Vec<TextEdit>,
    machine_applicable: bool,
    scaffold: bool,
    follow_on_reanalysis_outside: Option<Range>,
}

fn diagnostic_fixes(diagnostic: &Diagnostic, source: Option<&str>) -> Option<Vec<EagerFix>> {
    let fixes = diagnostic.data.as_ref()?.get("fixes")?.as_array()?;
    let result = fixes
        .iter()
        .filter_map(|fix| {
            let title = fix.get("title")?.as_str()?.to_owned();
            let applicability = fix
                .get("applicability")
                .and_then(|v| v.as_str())
                .unwrap_or("maybeIncorrect");
            let follow_on_reanalysis_outside = fix
                .get("followOnReanalysisOutside")
                .and_then(|range| serde_json::from_value(range.clone()).ok());
            let edits = fix
                .get("edits")?
                .as_array()?
                .iter()
                .map(|edit| {
                    let range = serde_json::from_value(edit.get("range")?.clone()).ok()?;
                    let new_text = if let Some(replacement) =
                        edit.get("replacement").and_then(serde_json::Value::as_str)
                    {
                        replacement.to_owned()
                    } else {
                        materialize_replacement_parts(edit, source?)?
                    };
                    Some(TextEdit { range, new_text })
                })
                .collect::<Option<Vec<_>>>()?;
            (!edits.is_empty()).then_some(EagerFix {
                title,
                edits,
                machine_applicable: applicability == "machineApplicable",
                scaffold: fix
                    .get("scaffold")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
                follow_on_reanalysis_outside,
            })
        })
        .collect::<Vec<_>>();
    (!result.is_empty()).then_some(result)
}

fn scaffold_is_current(
    source: &str,
    fix: &EagerFix,
    validation: FixValidationContext<'_>,
    target: &Diagnostic,
) -> bool {
    if fix.machine_applicable {
        return false;
    }
    let Some(repaired) = apply_text_edits(source, &fix.edits) else {
        return false;
    };
    if crate::pass::parser::parse_module_file(&repaired).is_err() {
        return false;
    }
    let Err(failure) = crate::cmd::check::analyze_module_at_with_overlay_lsp(
        validation.package_root,
        validation.overlay,
        validation.focus_file,
    ) else {
        return false;
    };
    let index = LineIndex::new(source);
    failure.errors.iter().any(|located| {
        located_matches_lsp_diagnostic(
            located,
            target,
            validation.package_root,
            validation.focus_file,
            &index,
        ) && located.error.diagnostic().fixes().iter().any(|actual| {
            actual.scaffold
                && actual.title == fix.title
                && actual.edits.len() == fix.edits.len()
                && actual
                    .edits
                    .iter()
                    .zip(&fix.edits)
                    .all(|(actual, requested)| {
                        actual.file.is_none()
                            && actual.replacement_parts.is_empty()
                            && actual.replacement == requested.new_text
                            && exact_byte_offset(&index, requested.range.start)
                                == Some(actual.span.start as usize)
                            && exact_byte_offset(&index, requested.range.end)
                                == Some(actual.span.end as usize)
                    })
        })
    })
}

fn materialize_replacement_parts(edit: &serde_json::Value, source: &str) -> Option<String> {
    let index = LineIndex::new(source);
    if let Some(guards) = edit.get("requiredWhitespace") {
        for guard in guards.as_array()? {
            let range: Range = serde_json::from_value(guard.clone()).ok()?;
            let start = exact_byte_offset(&index, range.start)?;
            let end = exact_byte_offset(&index, range.end)?;
            if start > end || !source.get(start..end)?.chars().all(char::is_whitespace) {
                return None;
            }
        }
    }
    let parts = edit.get("replacementParts")?.as_array()?;
    let mut replacement = String::new();
    for part in parts {
        if let Some(text) = part.get("text").and_then(serde_json::Value::as_str) {
            replacement.push_str(text);
            continue;
        }
        let range: Range = serde_json::from_value(part.get("sourceRange")?.clone()).ok()?;
        let start = exact_byte_offset(&index, range.start)?;
        let end = exact_byte_offset(&index, range.end)?;
        if start > end {
            return None;
        }
        replacement.push_str(source.get(start..end)?);
    }
    Some(replacement)
}

fn unresolved_name(diagnostic: &Diagnostic) -> Option<&str> {
    diagnostic.data.as_ref()?.get("unresolvedName")?.as_str()
}

fn exported_modules(analysis: &LspAnalysis, name: &str) -> Vec<String> {
    let mut modules = Vec::new();
    for (module_path, entry) in analysis.root_package_lowered.modules() {
        for item in &entry.module.items {
            crate::pass::resolve::for_each_item_declaration(item, |declaration| {
                if declaration
                    .fn_def()
                    .is_some_and(|d| d.name == name && d.vis.is_pub())
                    || declaration
                        .newtype()
                        .is_some_and(|n| n.name == name && n.vis.is_pub())
                    || declaration.host_fn().is_some_and(|h| h.name == name)
                {
                    modules.push(module_path.to_owned());
                }
            });
        }
    }
    modules.sort();
    modules.dedup();
    modules
}

fn module_path_string(module: &Module<Surface>) -> String {
    module
        .path
        .segments
        .iter()
        .map(|segment| segment.name.as_str())
        .collect::<Vec<_>>()
        .join("/")
}

fn import_insert_range(module: &Module<Surface>, source: &str) -> Option<Range> {
    let offset = if let Some(import_) = module.imports.last() {
        import_.span.end
    } else {
        // The path span excludes `;`; inspect only header trivia and its delimiter.
        let header_end = module
            .items
            .first()
            .map_or(module.meta.span.end, |item| item.span().start);
        let header = source.get(module.path.span.end as usize..header_end as usize)?;
        let tokens = crate::pass::lexer::lex(header).ok()?;
        let token = tokens.first()?;
        if !matches!(token.kind, crate::pass::lexer::TokenKind::Semicolon) {
            return None;
        }
        module.path.span.end + token.span.end
    };
    let index = LineIndex::new(source);
    let pos = index.to_position(offset);
    Some(Range {
        start: Position {
            line: pos.line,
            character: pos.character,
        },
        end: Position {
            line: pos.line,
            character: pos.character,
        },
    })
}

fn eof_range(source: &str) -> Range {
    let index = LineIndex::new(source);
    let pos = index.to_position(source.len() as u32);
    Range {
        start: lsp_types::Position {
            line: pos.line,
            character: pos.character,
        },
        end: lsp_types::Position {
            line: pos.line,
            character: pos.character,
        },
    }
}

fn call_arity_after_name(source: &str, range: Range) -> usize {
    let index = LineIndex::new(source);
    let mut offset = index.position_to_offset(LspPosition {
        line: range.end.line,
        character: range.end.character,
    }) as usize;
    let bytes = source.as_bytes();
    while bytes.get(offset).is_some_and(u8::is_ascii_whitespace) {
        offset += 1;
    }
    if bytes.get(offset) != Some(&b'(') {
        return 0;
    }
    offset += 1;
    let mut depth = 0usize;
    let mut args = 0usize;
    let mut saw_token = false;
    while let Some(&b) = bytes.get(offset) {
        match b {
            b'(' | b'[' | b'{' => {
                depth += 1;
                saw_token = true;
            }
            b')' if depth == 0 => break,
            b')' | b']' | b'}' => {
                depth = depth.saturating_sub(1);
                saw_token = true;
            }
            b',' if depth == 0 => {
                args += 1;
                saw_token = false;
            }
            b if b.is_ascii_whitespace() => {}
            _ => saw_token = true,
        }
        offset += 1;
    }
    if saw_token { args + 1 } else { 0 }
}

trait UriFromLossy {
    fn from_str_lossy(s: &str) -> Option<Self>
    where
        Self: Sized;
}

impl UriFromLossy for Uri {
    fn from_str_lossy(s: &str) -> Option<Self> {
        s.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::{CodeActionContext, TextDocumentIdentifier};
    use std::str::FromStr;

    #[test]
    fn auto_import_insertion_preserves_header_and_import_boundaries() {
        for marked in [
            "module main;|",
            "module main \t;|\nfn f() -> . { () }\n",
            "module main // header ; stays a comment\r\n;|\r\nfn f() -> . { () }\r\n",
            "module main;|fn f() -> . { () }\n",
            "module main;\nimport existing as e;|\nfn f() -> . { () }\n",
            "module main;\nimport existing as e // import ; comment\r\n;|fn f() -> . { () }\n",
        ] {
            let (prefix, suffix) = marked.split_once('|').unwrap();
            let source = format!("{prefix}{suffix}");
            let module = crate::pass::parser::parse_module_file(&source)
                .unwrap()
                .module;
            let range = import_insert_range(&module, &source).expect("parsed insertion boundary");
            assert_eq!(range.start, range.end);
            assert_eq!(
                exact_byte_offset(&LineIndex::new(&source), range.start),
                Some(prefix.len()),
                "{marked}"
            );
            let edited = apply_text_edits(
                &source,
                &[TextEdit {
                    range,
                    new_text: "\nimport added as a;".to_owned(),
                }],
            )
            .unwrap();
            assert!(
                crate::pass::parser::parse_module_file(&edited).is_ok(),
                "{edited}"
            );
        }
    }

    #[test]
    fn auto_import_insertion_withholds_invalid_header_locations() {
        let module = crate::pass::parser::parse_module_file("module main;\nfn f() -> . { () }\n")
            .unwrap()
            .module;
        for source in [
            "module main \nfn f() -> . { () }\n",
            "module main\"\nfn f() -> . { () }\n",
            "module main",
        ] {
            assert_eq!(import_insert_range(&module, source), None, "{source}");
        }
    }

    #[test]
    fn eager_fixes_do_not_require_a_parsed_syntax_context() {
        let uri = Uri::from_str("file:///workspace/a/b/c.kio").expect("file URI");
        let range = Range::default();
        let diagnostic = Diagnostic {
            range,
            message: "replace this".to_owned(),
            data: Some(serde_json::json!({
                "fixes": [{
                    "title": "Replace it",
                    "applicability": "machineApplicable",
                    "edits": [{ "range": range, "replacement": "fixed" }]
                }]
            })),
            ..Diagnostic::default()
        };
        let params = CodeActionParams {
            text_document: TextDocumentIdentifier { uri },
            range,
            context: CodeActionContext {
                diagnostics: vec![diagnostic],
                ..CodeActionContext::default()
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };

        let actions =
            handle_code_action(params, None, None, None, None, None).expect("code actions");
        assert_eq!(actions.len(), 1);
        let CodeActionOrCommand::CodeAction(action) = &actions[0] else {
            panic!("expected eager code action");
        };
        assert_eq!(action.title, "Replace it");
        assert!(action.edit.is_some());
    }

    #[test]
    fn eager_fixes_are_withheld_for_a_stale_document_version() {
        let uri = Uri::from_str("file:///workspace/a/b/c.kio").expect("file URI");
        let range = Range::default();
        let diagnostic = Diagnostic {
            range,
            message: "replace this".to_owned(),
            data: Some(serde_json::json!({
                "kioDocumentVersion": 4,
                "fixes": [{
                    "title": "Replace it",
                    "applicability": "machineApplicable",
                    "edits": [{ "range": range, "replacement": "fixed" }]
                }]
            })),
            ..Diagnostic::default()
        };
        let params = CodeActionParams {
            text_document: TextDocumentIdentifier { uri },
            range,
            context: CodeActionContext {
                diagnostics: vec![diagnostic],
                ..CodeActionContext::default()
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };

        let actions = handle_code_action(params, None, None, None, Some(5), None)
            .expect("code actions are a concrete empty response");
        assert!(actions.is_empty());
    }

    #[test]
    fn source_fix_all_requests_do_not_include_quickfix_actions() {
        let uri = Uri::from_str("file:///workspace/a/b/c.kio").expect("file URI");
        let range = Range::default();
        let diagnostic = Diagnostic {
            range,
            message: "replace this".to_owned(),
            data: Some(serde_json::json!({
                "fixes": [{
                    "title": "Replace it",
                    "applicability": "machineApplicable",
                    "edits": [{ "range": range, "replacement": "fixed" }]
                }]
            })),
            ..Diagnostic::default()
        };
        let params = CodeActionParams {
            text_document: TextDocumentIdentifier { uri },
            range,
            context: CodeActionContext {
                diagnostics: vec![diagnostic],
                only: Some(vec![CodeActionKind::SOURCE_FIX_ALL]),
                ..CodeActionContext::default()
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };

        let actions = handle_code_action(params, None, None, None, None, None)
            .expect("code actions are a concrete empty response");
        assert!(actions.is_empty());
    }

    #[test]
    fn source_slice_fix_materializes_exact_authenticated_text() {
        let source = "type User = Dependency;\ntype Dependency = .;";
        let edit = serde_json::json!({
            "replacementParts": [
                { "sourceRange": {
                    "start": { "line": 1, "character": 0 },
                    "end": { "line": 1, "character": 20 }
                }},
                { "text": "\n" },
                { "sourceRange": {
                    "start": { "line": 0, "character": 0 },
                    "end": { "line": 0, "character": 23 }
                }}
            ],
            "requiredWhitespace": []
        });

        assert_eq!(
            materialize_replacement_parts(&edit, source).as_deref(),
            Some("type Dependency = .;\ntype User = Dependency;")
        );
    }

    #[test]
    fn source_slice_fix_is_withheld_when_a_guard_owns_a_comment() {
        let source = "type A = .;\n// keep with B\ntype B = A;";
        let edit = serde_json::json!({
            "replacementParts": [{ "text": "replacement" }],
            "requiredWhitespace": [{
                "start": { "line": 0, "character": 11 },
                "end": { "line": 2, "character": 0 }
            }]
        });

        assert!(materialize_replacement_parts(&edit, source).is_none());
    }
}
