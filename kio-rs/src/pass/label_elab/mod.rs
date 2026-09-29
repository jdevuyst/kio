//! Label elaboration.
//!
//! Runs after `pass/desugar` and before `Package::build`. Strips the
//! three label-related surface forms, all of which need a label table
//! (a per-module name -> label-info index) that name resolution doesn't
//! itself produce:
//!
//! - [`Item::Labels`] -> N [`Item::Newtype`] entries (one per explicit label
//!   declaration; reuse markers emit none),
//!   plus one [`Item::TypeAlias`] for the named form.
//! - [`Type::LabelSugar`] is rejected at this boundary with a directed
//!   diagnostic; labels are value syntax and generated newtype
//!   names are the only type-surface spelling.
//! - [`Expr::LabelValue`] (the `{f = e}` / `{f}` value sugar) → calls to
//!   the generated constructor members.
//!
//! This pass is also the [`Desugared`] → [`Lowered`] phase
//! boundary: the four extension types `ExprTuple`, `ExprLabelValue`,
//! `TypeLabelSugar`, and `ItemLabels` collapse to `Never` in
//! `Lowered`. `Tuple` was stripped by `desugar` already (its
//! Desugared-phase ext is also `Never`, so the match-arm here
//! discharges via `match ext {}`); the other three are stripped by
//! the lowering rules below.
//!
//! Type-position uses of a label are rejected with a directed
//! diagnostic. A self-reference inside a generated label payload must use
//! the generated newtype name.
//!
//! The generated newtype name is the surface label spelling with the
//! first letter capitalized (`foo` → `Foo`). The casing convention
//! already pins lowercase for labels and values and uppercase
//! for types, so the transformation is the natural one. Collisions
//! with another module-level `F` (a `newtype F`, `type F`, or
//! named-form `labels F = { … };` type alias) are caught by the
//! ordinary duplicate-name rule in [`build_label_table`]: a clear
//! diagnostic asks the user to rename the label or the colliding
//! declaration. The chosen spelling is stored in
//! [`LabelInfo::newtype_name`] and used by every downstream rewrite,
//! so the rest of this file is name-agnostic.
//!
//! The Desugared → Lowered walk is shared with any other future pass
//! at the same boundary via the [`DesugaredToLowered`] trait in
//! [`crate::pass::label_elab::walk`]: [`LabelElabVisitor`] implements three
//! rewrite hooks ([`Expr::LabelValue`], [`Type::LabelSugar`],
//! [`Item::Labels`]) plus field access/update label resolution. Every
//! other AST variant clones-and-recurses through the trait's defaults.

mod routes;
pub mod walk;

use std::path::PathBuf;

use crate::ast::{
    DeferredRecLabelsDiagnostic, Desugared, ElaboratorCall, ElaboratorKind, Equiv, Expr,
    FieldAccessLabel, FieldUpdateLabel, FnDef, Import, ImportItem, ImportKind, Item, LabelEntry,
    LabelSugarLabel, LabelValueLabel, Labels, LabelsArm, Lowered, Meta, Module, ModulePath,
    Newtype, PackageFile, PathSegment, Signature, SignatureParam, Type, TypeAlias, TypeMember,
    TypeRecGroup, TypeRecMember, Visibility, convert_type_member, mint_label_newtype_name,
};
use crate::error::{Error, Fix};
use crate::pass::label_elab::walk::DesugaredToLowered;
use crate::pass::resolve::{LocatedError, is_visible};
use crate::span::Span;
#[cfg(feature = "parallel")]
use rayon::prelude::*;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// The written binding owns visibility and duplicate-introduction checks;
/// its terminal nominal can belong to a different module after forwarding.
#[derive(Debug, Clone)]
struct LabelInfo {
    /// The generated newtype's name — minted as the surface label
    /// spelling with the first letter capitalized (`foo` → `Foo`).
    /// The concrete spelling is whatever [`mint_label_newtype_name`]
    /// picked at table-build time, and every downstream rewrite
    /// reads this field rather than recomputing the mint.
    newtype_name: String,
    newtype_module: ModulePath,
    declaration_span: Span,
    vis: Visibility,
    accessible: bool,
    origin: LabelOrigin,
}

impl LabelInfo {
    /// Allocate terminal-provider access only when a written label is used.
    /// Enumerating an upstream module's unused forwards must not introduce
    /// additional consumer dependencies. The path uses the label's own span
    /// and leaves rigid same-named binders to ordinary identity checking.
    fn newtype_path(
        &self,
        current_module: &[PathSegment],
        label_span: Span,
        terminal_aliases: &mut TerminalAliases,
    ) -> Vec<PathSegment> {
        let mut segments = Vec::new();
        let alias = match &self.origin {
            LabelOrigin::Imported { source, alias }
                if *source == module_path_to_string(&self.newtype_module) =>
            {
                Some(alias.clone())
            }
            _ if self.newtype_module.segments == current_module => None,
            _ => Some(terminal_aliases.alias_for(&self.newtype_module, label_span)),
        };
        if let Some(alias) = alias {
            segments.push(PathSegment::new(alias, label_span));
        }
        segments.push(PathSegment::new(self.newtype_name.clone(), label_span));
        segments
    }

    fn ensure_accessible(
        &self,
        label: &str,
        label_span: Span,
        importer: &[PathSegment],
    ) -> Result<(), Error> {
        if self.accessible {
            return Ok(());
        }
        let LabelOrigin::Imported { source, .. } = &self.origin else {
            unreachable!("module-local labels are always accessible in their declaring module");
        };
        let source_name = source;
        let importer_name = importer
            .iter()
            .map(PathSegment::as_str)
            .collect::<Vec<_>>()
            .join("/");
        let error = match &self.vis {
            Visibility::PublicIn(path) => Error::import(
                label_span,
                format!(
                    "label `{label}` from module `{source_name}` is restricted to `pub({})` and is not visible from `{importer_name}`",
                    module_path_to_string(path)
                ),
            ),
            Visibility::Private => Error::import(
                label_span,
                format!("label `{label}` in module `{source_name}` is private"),
            )
            .with_help(format!(
                "add `pub` to label `{label}` in module `{source_name}` to make it importable"
            )),
            Visibility::Public => unreachable!("public imported labels are accessible"),
        };
        Err(error)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LabelOrigin {
    /// Declared by a `labels` in this module.
    Local,
    /// Brought in from another module. `source` is the declaring module.
    ///
    /// `alias` is always present: a user's qualified import supplies it, and
    /// an explicit braced selective import receives a deterministic,
    /// collision-allocated internal alias. Consequently label selection
    /// never widens the consumer's ordinary type namespace.
    Imported { source: String, alias: String },
}

type LabelTable = HashMap<String, LabelInfo>;

/// The output of [`elaborate_package`]: every parsed module rebuilt
/// at the [`Lowered`] phase, plus the optional package file in the
/// same phase.
pub type ElaboratedPackage = (
    Vec<(PathBuf, Module<Lowered>)>,
    Option<PackageFile<Lowered>>,
);

/// Package-level label summary used by the scheduled frontend.
///
/// Built from lowered-body stubs first, then shared by per-module
/// elaboration so ready modules can lower without waiting for unrelated
/// bodies to be forced.
pub struct PackageLabelTables {
    local: HashMap<String, LabelTable>,
}

/// Run label elaboration over every parsed module (and the package
/// file's exported fn bodies, if any). Consumes a list of
/// [`Desugared`]-phase modules and returns the corresponding
/// [`Lowered`]-phase list, with `Item::Labels` replaced by the
/// generated `Item::Newtype` chains and label sugar in types and
/// expressions rewritten.
pub fn elaborate_package(
    parsed: Vec<(PathBuf, Module<Desugared>)>,
    package_file: Option<PackageFile<Desugared>>,
) -> Result<ElaboratedPackage, LocatedError> {
    let package_tables = PackageLabelTables::build(&parsed)?;

    // Pass 2: build each module's effective label table, lower against it,
    // erase braced selective items, and add the qualified module aliases used
    // by imported label constructor/projector paths.
    // Consumes each `(PathBuf, Module<Desugared>)` and produces the
    // `(PathBuf, Module<Lowered>)` pair.
    let indexed: Vec<_> = parsed.into_iter().enumerate().collect();
    let mut lowered_results: Vec<LabelModuleResult<Module<Lowered>>> =
        crate::maybe_into_par_iter!(indexed)
            .map(|(index, (file_path, module))| {
                let module_path = module_path_to_string(&module.path);
                let module_span = module.path.span;
                let result = package_tables.elaborate_module(module);
                LabelModuleResult {
                    index,
                    module_path,
                    module_span,
                    file_path,
                    result,
                }
            })
            .collect();
    if let Some(error) = first_label_module_error(&lowered_results) {
        return Err(error);
    }
    lowered_results.sort_by_key(|result| result.index);
    let lowered_parsed: Vec<(PathBuf, Module<Lowered>)> = lowered_results
        .into_iter()
        .map(|result| {
            (
                result.file_path,
                result
                    .result
                    .expect("label-elab errors returned before module collection"),
            )
        })
        .collect();

    // Pass 3: package file.
    let lowered_package_file = match package_file {
        None => None,
        Some(package_file) => {
            Some(
                elaborate_package_file(package_file).map_err(|error| LocatedError {
                    file_path: PathBuf::from("<package-file>"),
                    error,
                })?,
            )
        }
    };

    Ok((lowered_parsed, lowered_package_file))
}

impl PackageLabelTables {
    /// Build per-module local label tables, indexed by slash module path
    /// (`pkg/helper`). The resulting summary lets later module walks
    /// resolve cross-module label imports without re-scanning the whole
    /// package.
    pub fn build(parsed: &[(PathBuf, Module<Desugared>)]) -> Result<Self, LocatedError> {
        let table_results: Vec<LabelModuleResult<LabelTable>> = crate::maybe_par_iter!(parsed)
            .map(|(file_path, module)| {
                let module_path = module_path_to_string(&module.path);
                let result = build_label_table(module);
                LabelModuleResult {
                    index: 0,
                    module_path,
                    module_span: module.path.span,
                    file_path: file_path.clone(),
                    result,
                }
            })
            .collect();
        if let Some(error) = first_label_module_error(&table_results) {
            return Err(error);
        }
        let mut local: HashMap<String, LabelTable> = HashMap::new();
        for result in table_results {
            let table = result
                .result
                .expect("label-table errors returned before package table collection");
            if let Some(_existing) = local.insert(result.module_path.clone(), table) {
                // Duplicate-module-path is caught later by `Package::build`,
                // but if we got here there are two entries for the same
                // path — bail with a path-flavored error so the diagnostic
                // names the second occurrence's file.
                return Err(LocatedError {
                    file_path: result.file_path,
                    error: Error::import(
                        result.module_span,
                        format!("duplicate module path `{}`", result.module_path),
                    ),
                });
            }
        }
        routes::complete_forwarded_labels(parsed, &mut local)?;
        Ok(Self { local })
    }

    /// Elaborate one module against the package label summary.
    pub fn elaborate_module(&self, module: Module<Desugared>) -> Result<Module<Lowered>, Error> {
        build_effective_table(&module, &self.local)
            .and_then(|effective| elaborate_module(module, effective))
    }
}

/// Elaborate a package file. Package files have no `labels` of their
/// own; exported fn bodies currently lower against an empty
/// label table, matching [`elaborate_package`].
pub fn elaborate_package_file(
    export: PackageFile<Desugared>,
) -> Result<PackageFile<Lowered>, Error> {
    let empty = LabelTable::new();
    LabelElabVisitor {
        table: &empty,
        module_path: &[],
        terminal_aliases: TerminalAliases::default(),
        bound: Vec::new(),
        minted_newtypes: HashSet::new(),
    }
    .walk_package_file(export)
}

struct LabelModuleResult<T> {
    index: usize,
    module_path: String,
    module_span: Span,
    file_path: PathBuf,
    result: Result<T, Error>,
}

fn first_label_module_error<T>(results: &[LabelModuleResult<T>]) -> Option<LocatedError> {
    results
        .iter()
        .filter_map(|result| {
            result.result.as_ref().err().map(|error| {
                let (span, _) = error.diag();
                (
                    result.module_path.as_str(),
                    span.start,
                    span.end,
                    LocatedError {
                        file_path: result.file_path.clone(),
                        error: error.clone(),
                    },
                )
            })
        })
        .min_by(|a, b| (a.0, a.1, a.2).cmp(&(b.0, b.1, b.2)))
        .map(|(_, _, _, located)| located)
}

/// Collect the explicit module-local label declarations and validate every
/// surface reuse marker in deterministic source order. Imported labels never
/// participate: another module cannot change whether an existing marker
/// resolves, and declarations later in this module cannot satisfy it.
fn build_label_table(module: &Module<Desugared>) -> Result<LabelTable, Error> {
    let mut table = LabelTable::new();
    let mut declarations: HashMap<String, LabelEntry<Desugared>> = HashMap::new();
    let reserved = collect_module_type_names(module);
    for item in &module.items {
        match item {
            Item::Labels(labels, _) => {
                add_labels_to_table(
                    labels,
                    &module.path,
                    &reserved,
                    &mut declarations,
                    &mut table,
                )?;
            }
            Item::TypeRecGroup(group) => {
                for member in &group.members {
                    if let TypeRecMember::Labels(labels, _) = member {
                        add_labels_to_table(
                            labels,
                            &module.path,
                            &reserved,
                            &mut declarations,
                            &mut table,
                        )?;
                    }
                }
            }
            _ => {}
        }
    }
    Ok(table)
}

fn add_labels_to_table(
    labels: &Labels<Desugared>,
    module_path: &ModulePath,
    reserved: &HashSet<String>,
    declarations: &mut HashMap<String, LabelEntry<Desugared>>,
    table: &mut LabelTable,
) -> Result<(), Error> {
    for entry in label_entries_in_source_order(labels)? {
        if entry.is_reuse_marker() {
            validate_reuse_marker(labels, entry, declarations)?;
            continue;
        }

        if let Some(first) = declarations.get(entry.name.as_str()) {
            let headers_compatible = first.universal_header_matches(entry);
            let alias_binders_compatible = labels.unbound_entry_universal(entry).is_none();
            let payloads_match = type_shape_eq(&first.payload, &entry.payload);
            let fix_scope_is_unambiguous = labels.type_alias_params.is_empty();
            let mut error = Error::name_res(
                entry.name_span,
                format!(
                    "label `{}` is already explicitly declared in this module",
                    entry.name
                ),
            )
            .with_secondary(
                first.name_span,
                format!("`{}` was first declared here", entry.name),
            );
            if labels.type_alias_name.is_some()
                && headers_compatible
                && first.existential_params.is_empty()
                && entry.existential_params.is_empty()
                && alias_binders_compatible
                && payloads_match
                && fix_scope_is_unambiguous
            {
                error = error
                    .with_help(format!(
                        "reuse the earlier label with `{}: _`; `_` reuses its generated nominal instead of declaring it again",
                        entry.name
                    ))
                    .with_suggestion(entry.payload.span(), "_");
            } else if labels.type_alias_name.is_none() {
                error = error.with_help(
                    "anonymous `labels` declarations cannot reuse labels; rename this label or move the use into a named `labels` declaration",
                );
            } else if !headers_compatible {
                error = error.with_help(
                    "a reuse marker must repeat the earlier label's universal binder arity and kinds; align the header and write `_`, or rename this label",
                );
            } else if !first.existential_params.is_empty() {
                error = error.with_help(
                    "the earlier label introduces existential binders, so an identical payload spelling can name a different type here; write `_` explicitly only if this position should use the earlier generated nominal, otherwise rename this label",
                );
            } else if !entry.existential_params.is_empty() {
                error = error.with_help(
                    "reuse markers cannot declare existential binders; remove the existential binders and replace the payload with `_`, or rename this label",
                );
            } else if !alias_binders_compatible {
                error = error.with_help(
                    "a reuse marker's universal binder names must be parameters of the enclosing alias; align the alias and entry headers, then write `_`, or rename this label",
                );
            } else if !fix_scope_is_unambiguous {
                error = error.with_help(
                    "the enclosing alias introduces type binders, so an automatic payload replacement could change which type a spelling names; write `_` explicitly only if this position should use the earlier generated nominal, otherwise rename this label",
                );
            } else {
                error = error.with_help(
                    "this payload differs from the earlier declaration; write `_` only if this position should use the earlier generated nominal, otherwise rename this label",
                );
            }
            return Err(error);
        }

        let newtype_name = mint_label_newtype_name(&entry.name);
        if reserved.contains(&newtype_name) {
            return Err(Error::name_res(
                entry.name_span,
                format!(
                    "label `{label}` would generate newtype `{newtype_name}`, which \
                     collides with an existing top-level declaration of the same name",
                    label = entry.name
                ),
            )
            .with_help(format!(
                "rename the label `{label}` or the colliding `{newtype_name}` declaration",
                label = entry.name
            )));
        }
        declarations.insert(entry.name.clone(), entry.clone());
        table.insert(
            entry.name.clone(),
            LabelInfo {
                newtype_name,
                newtype_module: module_path.clone(),
                declaration_span: entry.name_span,
                vis: labels.vis.clone(),
                accessible: true,
                origin: LabelOrigin::Local,
            },
        );
    }
    Ok(())
}

fn label_entries_in_source_order(
    d: &Labels<Desugared>,
) -> Result<Vec<&LabelEntry<Desugared>>, Error> {
    let mut entries = Vec::new();
    for arm_entries in d.arms_in_source_order() {
        let mut arm_seen: HashMap<&str, Span> = HashMap::new();
        for entry in arm_entries {
            if let Some(&first_span) = arm_seen.get(entry.name.as_str()) {
                return Err(Error::name_res(
                    entry.name_span,
                    format!("duplicate label `{}` in this product arm", entry.name),
                )
                .with_secondary(first_span, format!("`{}` first written here", entry.name))
                .with_help("write each label at most once in a product arm"));
            }
            arm_seen.insert(entry.name.as_str(), entry.name_span);
            entries.push(entry);
        }
    }
    Ok(entries)
}

/// Conservative syntactic equality for the repeated-explicit-label code fix.
/// It does not participate in reuse resolution: unequal payloads are the same
/// duplicate-declaration error, but only an identical shape makes replacing the
/// later payload with `_` safe as an automatic edit.
fn type_shape_eq(a: &Type<Desugared>, b: &Type<Desugared>) -> bool {
    match (a, b) {
        (
            Type::Path {
                segments: a_segments,
                args: a_args,
                ..
            },
            Type::Path {
                segments: b_segments,
                args: b_args,
                ..
            },
        ) => {
            a_segments.len() == b_segments.len()
                && a_segments
                    .iter()
                    .zip(b_segments)
                    .all(|(a, b)| a.name == b.name)
                && type_list_shape_eq(a_args, b_args)
        }
        (Type::Unit { .. }, Type::Unit { .. }) | (Type::Bottom { .. }, Type::Bottom { .. }) => true,
        (
            Type::Function {
                param: a_param,
                ret: a_ret,
                ..
            },
            Type::Function {
                param: b_param,
                ret: b_ret,
                ..
            },
        ) => type_shape_eq(a_param, b_param) && type_shape_eq(a_ret, b_ret),
        (
            Type::Product {
                left: a_left,
                right: a_right,
                ..
            },
            Type::Product {
                left: b_left,
                right: b_right,
                ..
            },
        )
        | (
            Type::Sum {
                left: a_left,
                right: a_right,
                ..
            },
            Type::Sum {
                left: b_left,
                right: b_right,
                ..
            },
        ) => type_shape_eq(a_left, b_left) && type_shape_eq(a_right, b_right),
        (
            Type::Forall {
                param: a_param,
                body: a_body,
                ..
            },
            Type::Forall {
                param: b_param,
                body: b_body,
                ..
            },
        ) => {
            a_param.name == b_param.name
                && a_param.kind == b_param.kind
                && type_shape_eq(a_body, b_body)
        }
        (
            Type::LabelSugar {
                labels: a_labels, ..
            },
            Type::LabelSugar {
                labels: b_labels, ..
            },
        ) => {
            a_labels.len() == b_labels.len()
                && a_labels.iter().zip(b_labels).all(|(a, b)| {
                    a.label == b.label
                        && match (&a.payload, &b.payload) {
                            (Some(a), Some(b)) => type_shape_eq(a, b),
                            (None, None) => true,
                            _ => false,
                        }
                })
        }
        (Type::Infer { .. }, Type::Infer { .. }) => true,
        _ => false,
    }
}

fn type_list_shape_eq(a: &[Type<Desugared>], b: &[Type<Desugared>]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(a, b)| type_shape_eq(a, b))
}

fn validate_reuse_marker(
    labels: &Labels<Desugared>,
    reuse: &LabelEntry<Desugared>,
    declarations: &HashMap<String, LabelEntry<Desugared>>,
) -> Result<(), Error> {
    if labels.type_alias_name.is_none() {
        return Err(Error::name_res(
            reuse.name_span,
            format!(
                "label reuse marker `{}: _` is not allowed in an anonymous `labels` declaration",
                reuse.name
            ),
        )
        .with_help(
            "give the `labels` declaration a named alias, or write an explicit payload to declare a new label",
        ));
    }
    if let Some(existential) = reuse.existential_params.first() {
        return Err(Error::name_res(
            existential.span,
            format!(
                "label reuse marker `{}: _` cannot declare existential binders",
                reuse.name
            ),
        )
        .with_secondary(reuse.name_span, "this entry is a reuse marker")
        .with_help(
            "remove the existential binders; they belong only on the original explicit label declaration",
        ));
    }
    let Some(declaration) = declarations.get(reuse.name.as_str()) else {
        return Err(Error::name_res(
            reuse.name_span,
            format!(
                "label `{}` cannot be reused because it has no earlier explicit declaration in this module",
                reuse.name
            ),
        )
        .with_help(format!(
            "declare `{0}: Payload` earlier in this module; imported, qualified, and later declarations cannot satisfy `{0}: _`",
            reuse.name
        )));
    };
    if !declaration.universal_header_matches(reuse) {
        let expected = declaration
            .type_params
            .iter()
            .map(|param| param.effective_kind().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let found = reuse
            .type_params
            .iter()
            .map(|param| param.effective_kind().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(Error::name_res(
            reuse.name_span,
            format!(
                "label `{}` reuse has an incompatible universal binder header",
                reuse.name
            ),
        )
        .with_secondary(
            declaration.name_span,
            format!(
                "the original declaration has {} binder(s) with kinds [{}]",
                declaration.type_params.len(),
                expected
            ),
        )
        .with_help(format!(
            "repeat {} universal binder(s) with kinds [{}] before `: _`; binder names may differ",
            declaration.type_params.len(),
            if declaration.type_params.is_empty() {
                "none"
            } else {
                expected.as_str()
            }
        ))
        .with_note(format!(
            "this reuse has {} binder(s) with kinds [{}]",
            reuse.type_params.len(),
            found
        )));
    }
    if let Some(param) = labels.unbound_entry_universal(reuse) {
        return Err(Error::name_res(
            param.span,
            format!(
                "label `{}` reuse binder `{}` does not name a compatible parameter of the enclosing alias",
                reuse.name, param.name
            ),
        )
        .with_secondary(
            labels.type_alias_span.unwrap_or(labels.meta.span),
            "the enclosing named `labels` declaration introduces the alias parameters here",
        )
        .with_help(
            "repeat the original label's binder kinds using parameters declared by the enclosing alias",
        ));
    }
    Ok(())
}

/// Gather every top-level type name in the module so [`mint_label_newtype_name`]
/// can pick a spelling that doesn't collide. The set covers all
/// declaration heads at top level — `newtype` heads, `type`
/// heads, and the named-form `labels` type-alias head. (The label
/// entries themselves aren't yet minted; their names are added to
/// the reserved set in [`build_label_table`] as each one is decided.)
fn collect_module_type_names(module: &Module<Desugared>) -> HashSet<String> {
    let mut names = HashSet::new();
    for item in &module.items {
        match item {
            Item::Newtype(d) => {
                names.insert(d.name.clone());
            }
            // At `Desugared` every `Item::TypeAlias` is a type, so it
            // contributes a type name.
            Item::TypeAlias(a) => {
                names.insert(a.name.clone());
            }
            Item::Labels(d, _) => {
                if let Some(name) = &d.type_alias_name {
                    names.insert(name.clone());
                }
            }
            Item::TypeRecGroup(group) => {
                for member in &group.members {
                    match member {
                        crate::ast::TypeRecMember::TypeAlias(alias) => {
                            names.insert(alias.name.clone());
                        }
                        crate::ast::TypeRecMember::Newtype(newtype) => {
                            names.insert(newtype.name.clone());
                        }
                        crate::ast::TypeRecMember::Labels(labels, _) => {
                            names.extend(labels.type_alias_name.iter().cloned());
                        }
                    }
                }
            }
            // A `host type` contributes a type name; a `host fn` is a
            // value name and reserves no type.
            Item::HostType(h) => {
                names.insert(h.name.clone());
            }
            Item::HostFn(_) => {}
            Item::FnDef(_) => {}
            // `equiv` decls don't introduce types; nothing to reserve.
            Item::Equiv(_, _) => {}
            Item::LabelForward(_, _) => {}
            // Elaborator decls are compile-time metadata; no type name.
            Item::Elaborator(_, _) => {}
            // Statically uninhabited at `Desugared`.
            Item::LiteralAlias(_, ext) => match *ext {},
            Item::Op(_, ext) => match *ext {},
            Item::VariadicOperator(_, ext) => match *ext {},
            Item::RecGroup(_, ext) => match *ext {},
        }
    }
    names
}

/// Build a module's effective label table. Braced selective imports add only
/// label bindings; bare selective imports remain exclusively in the ordinary
/// value/type namespace. Each braced source receives a collision-allocated
/// qualified alias used by the lowered constructor/projector references.
///
/// `package_local_tables` is keyed by slash module-path string
/// (e.g. `"pkg/helper"`), matching how [`Use::Selective::from`] is
/// rendered.
fn build_effective_table(
    consumer: &Module<Desugared>,
    package_local_tables: &HashMap<String, LabelTable>,
) -> Result<EffectiveLabels, Error> {
    let mut effective = package_local_tables
        .get(&module_path_to_string(&consumer.path))
        .expect("each elaborated module has a completed package label table")
        .clone();
    let mut occupied = module_binding_names(consumer);
    let mut selective_aliases: BTreeMap<String, (String, ModulePath, Span)> = BTreeMap::new();
    for u in &consumer.imports {
        match &u.kind {
            // `import m({foo});` selects the label namespace. A single
            // generated alias per source keeps the lowered path exact without
            // adding `Foo` to the ordinary selective-import namespace.
            ImportKind::Selective { items, from } => {
                let from_str = module_path_to_string(from);
                let Some(source_table) = package_local_tables.get(&from_str) else {
                    continue;
                };
                let mut alias = None;
                for (name, label_span) in items.iter().filter_map(ImportItem::as_label) {
                    let Some(source_info) = source_table.get(name) else {
                        continue;
                    };
                    let alias = alias.get_or_insert_with(|| {
                        selective_aliases
                            .entry(from_str.clone())
                            .or_insert_with(|| {
                                (
                                    allocate_synthetic_label_alias(&from_str, &mut occupied),
                                    from.clone(),
                                    label_span,
                                )
                            })
                            .0
                            .clone()
                    });
                    insert_imported_label(
                        &mut effective,
                        name,
                        label_span,
                        source_info,
                        &consumer.path,
                        from,
                        alias,
                    )?;
                }
            }
            // Qualified import: `import m as <alias>;`. Brings every label in
            // module `m` into scope under the qualified spelling
            // `<alias>.<label>` for the curly-brace forms `{m.f}` /
            // `{m.f: X}`. These lower to the **alias-qualified** `<alias>.F`
            // reference (not a bare `F`) so two modules whose labels mint
            // the same leaf stay identity-exact; the user's
            // `import m as <alias>;` line stays untouched and round-trips.
            ImportKind::Qualified { path, alias, .. } => {
                let from_str = module_path_to_string(path);
                let Some(source_table) = package_local_tables.get(&from_str) else {
                    continue;
                };
                for (label_name, source_info) in source_table {
                    let qualified_key = format!("{alias}.{label_name}");
                    // A bare `{f}` and a qualified `{m.f}` keying the
                    // same label don't conflict — the keys are distinct.
                    // The only conflict to flag is two qualified
                    // imports binding the same alias to different
                    // sources, but the resolver has already validated
                    // alias uniqueness, so we just insert.
                    effective.insert(
                        qualified_key,
                        LabelInfo {
                            accessible: is_visible(&source_info.vis, &consumer.path),
                            origin: LabelOrigin::Imported {
                                source: module_path_to_string(path),
                                alias: alias.clone(),
                            },
                            ..source_info.clone()
                        },
                    );
                }
            }
            _ => {}
        }
    }
    let synthetic_imports = selective_aliases
        .into_values()
        .map(|(alias, source, span)| synthetic_qualified_import(source, alias, span))
        .collect();
    Ok(EffectiveLabels {
        table: effective,
        synthetic_imports,
    })
}

struct EffectiveLabels {
    table: LabelTable,
    synthetic_imports: Vec<Import>,
}

#[derive(Default)]
struct TerminalAliases {
    available: BTreeMap<String, String>,
    occupied: BTreeSet<String>,
    generated: BTreeMap<String, Import>,
}

impl TerminalAliases {
    fn new(module: &Module<Desugared>, synthetic_imports: &[Import]) -> Self {
        let mut aliases = Self {
            occupied: module_binding_names(module),
            ..Self::default()
        };
        for import_ in module.imports.iter().chain(synthetic_imports) {
            if let ImportKind::Qualified { path, alias } = &import_.kind {
                aliases.occupied.insert(alias.clone());
                aliases
                    .available
                    .entry(module_path_to_string(path))
                    .or_insert_with(|| alias.clone());
            }
        }
        aliases
    }

    fn alias_for(&mut self, source: &ModulePath, span: Span) -> String {
        let key = module_path_to_string(source);
        if let Some(alias) = self.available.get(&key) {
            return alias.clone();
        }
        let alias = allocate_synthetic_label_alias(&key, &mut self.occupied);
        self.available.insert(key.clone(), alias.clone());
        self.generated.insert(
            key,
            synthetic_qualified_import(source.clone(), alias.clone(), span),
        );
        alias
    }
}

fn insert_imported_label(
    effective: &mut LabelTable,
    name: &str,
    label_span: Span,
    source_info: &LabelInfo,
    importer: &ModulePath,
    source: &ModulePath,
    alias: &str,
) -> Result<(), Error> {
    let origin = LabelOrigin::Imported {
        source: module_path_to_string(source),
        alias: alias.to_owned(),
    };
    if let Some(existing) = effective.get(name) {
        if existing.origin == origin {
            return Err(
                Error::name_res(label_span, format!("duplicate label import `{name}`"))
                    .with_help("remove the repeated label import"),
            );
        }
        let existing_site = match &existing.origin {
            LabelOrigin::Local => "declared in this module".to_owned(),
            LabelOrigin::Imported { source, .. } => {
                format!("imported from `{source}`")
            }
        };
        return Err(Error::name_res(
            label_span,
            format!(
                "label `{name}` is {existing_site} and also imported from `{}`",
                module_path_surface(source)
            ),
        ));
    }
    effective.insert(
        name.to_owned(),
        LabelInfo {
            accessible: is_visible(&source_info.vis, importer),
            origin,
            ..source_info.clone()
        },
    );
    Ok(())
}

/// Collect every ordinary module binding before allocating a generated label
/// alias. Generated label nominals are included even though they do not exist
/// until this pass completes.
fn module_binding_names(module: &Module<Desugared>) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for import_ in &module.imports {
        match &import_.kind {
            ImportKind::Qualified { alias, .. } => {
                names.insert(alias.clone());
            }
            ImportKind::Selective { items, .. } => {
                names.extend(
                    items
                        .iter()
                        .filter_map(ImportItem::as_name)
                        .map(str::to_owned),
                );
            }
            ImportKind::Intrinsics => {
                names.extend(
                    crate::pass::resolve::PRIME_INTRINSICS
                        .iter()
                        .map(|s| s.to_string()),
                );
            }
            ImportKind::Comptime => {
                names.extend(
                    crate::comptime::PUBLIC_COMPTIME_NAMES
                        .iter()
                        .map(|s| s.to_string()),
                );
            }
        }
    }
    for item in &module.items {
        match item {
            Item::FnDef(def) => {
                names.insert(def.name.clone());
            }
            Item::RecGroup(group, _) => {
                names.extend(group.members.iter().map(|def| def.name.clone()));
            }
            Item::TypeAlias(alias) => {
                names.insert(alias.name.clone());
            }
            Item::LiteralAlias(alias, _) => {
                names.insert(alias.name.clone());
            }
            Item::Newtype(newtype) => {
                names.insert(newtype.name.clone());
                names.insert(newtype.constructor.name.clone());
                names.insert(newtype.projector.name.clone());
            }
            Item::Labels(labels, _) => {
                names.extend(labels.type_alias_name.iter().cloned());
                names.extend(
                    labels
                        .entries
                        .iter()
                        .filter(|entry| !entry.is_reuse_marker())
                        .map(|entry| mint_label_newtype_name(&entry.name)),
                );
            }
            Item::TypeRecGroup(group) => {
                for member in &group.members {
                    match member {
                        crate::ast::TypeRecMember::TypeAlias(alias) => {
                            names.insert(alias.name.clone());
                        }
                        crate::ast::TypeRecMember::Newtype(newtype) => {
                            names.insert(newtype.name.clone());
                            names.insert(newtype.constructor.name.clone());
                            names.insert(newtype.projector.name.clone());
                        }
                        crate::ast::TypeRecMember::Labels(labels, _) => {
                            names.extend(labels.type_alias_name.iter().cloned());
                            names.extend(
                                labels
                                    .entries
                                    .iter()
                                    .filter(|entry| !entry.is_reuse_marker())
                                    .map(|entry| mint_label_newtype_name(&entry.name)),
                            );
                        }
                    }
                }
            }
            Item::Equiv(equiv, _) => {
                names.insert(equiv.name.clone());
            }
            Item::Elaborator(elaborator, _) => {
                names.insert(elaborator.name.clone());
            }
            Item::HostType(host) => {
                names.insert(host.name.clone());
            }
            Item::HostFn(host) => {
                names.insert(host.name.clone());
            }
            Item::Op(_, _) | Item::VariadicOperator(_, _) | Item::LabelForward(_, _) => {}
        }
    }
    names
}

/// Allocate an injective parser-legal alias for a source module. Hex encoding
/// distinguishes path bytes; suffixing avoids every consumer binding and
/// alias allocated earlier in this module.
fn allocate_synthetic_label_alias(from_path: &str, occupied: &mut BTreeSet<String>) -> String {
    let encoded = crate::naming::encode_name_component(from_path);
    let base = format!("_label_{encoded}");
    for suffix in 1usize.. {
        let candidate = if suffix == 1 {
            format!("{base}__")
        } else {
            format!("{base}_n{suffix}__")
        };
        if occupied.insert(candidate.clone()) {
            return candidate;
        }
    }
    unreachable!("an unbounded label-alias suffix space is never exhausted")
}

fn synthetic_qualified_import(source: ModulePath, alias: String, span: Span) -> Import {
    Import {
        trailing_trivia: Vec::new(),
        kind: ImportKind::Qualified {
            path: source,
            alias,
        },
        span,
        leading_trivia: Vec::new(),
    }
}

/// Canonical module-map key: segments `/`-joined.
fn module_path_to_string(p: &ModulePath) -> String {
    p.segments.join("/")
}

/// Surface module-path spelling. Used in diagnostics so the path a
/// user reads matches the path they write.
fn module_path_surface(p: &ModulePath) -> String {
    module_path_to_string(p)
}

/// Rebuild a module from [`Desugared`] to [`Lowered`]: replaces
/// `Item::Labels` with the generated `Item::Newtype` chain (and
/// optional `Item::TypeAlias` for the named form), rewrites label
/// sugar in every expression. Braced label imports are removed and their
/// collision-allocated qualified module uses are appended before walking.
fn elaborate_module(
    mut module: Module<Desugared>,
    effective: EffectiveLabels,
) -> Result<Module<Lowered>, Error> {
    let terminal_aliases = TerminalAliases::new(&module, &effective.synthetic_imports);
    erase_label_imports(&mut module.imports);
    module.imports.extend(effective.synthetic_imports);
    // `walk_module` consumes `module`; clone the path segments so the
    // visitor can retain the importing-module identity used by label
    // accessibility checks.
    let module_path = module.path.segments.clone();
    let mut visitor = LabelElabVisitor {
        table: &effective.table,
        module_path: &module_path,
        terminal_aliases,
        bound: Vec::new(),
        minted_newtypes: HashSet::new(),
    };
    let mut lowered = visitor.walk_module(module)?;
    lowered
        .imports
        .extend(visitor.terminal_aliases.generated.into_values());
    Ok(lowered)
}

fn erase_label_imports(imports: &mut Vec<Import>) {
    imports.retain_mut(|import_| {
        let ImportKind::Selective { items, .. } = &mut import_.kind else {
            return true;
        };
        items.retain(|item| !matches!(item, ImportItem::Label { .. }));
        !items.is_empty()
    });
}

// ---- Visitor ------------------------------------------------------

/// Per-module elaboration visitor.
///
/// Holds the effective label table for the module under elaboration.
/// Implements [`DesugaredToLowered`] with three required rewrite
/// hooks ([`Expr::LabelValue`], [`Type::LabelSugar`], [`Item::Labels`])
/// plus the `visit_*` overrides that reject labels in type
/// position and resolve field access/update labels. The binder-scope
/// overrides maintain the `bound` shadow stack so a local type binder
/// shadows a same-named label diagnostic. Every other AST variant
/// clones-and-recurses through the trait's defaults.
struct LabelElabVisitor<'a> {
    table: &'a LabelTable,
    terminal_aliases: TerminalAliases,
    /// This module's path segments, used as the importer identity when
    /// checking an imported label's `pub(path)` accessibility. Empty for
    /// the package-file walk, which has no importing module identity.
    module_path: &'a [PathSegment],
    /// Stack of currently in-scope binder names. Type-position label
    /// diagnostics consult it so a type parameter named `foo` remains an
    /// ordinary type parameter even if a label `foo` is also in scope.
    /// Mirrors `pass/desugar`'s `DesugarState.bound`; a linear scan in
    /// `is_bound` is cheap at realistic scope depths.
    bound: Vec<String>,
    /// Generated newtype spellings already emitted in this module.
    /// Reuse markers emit no item; this set defensively keeps constructed
    /// ASTs from producing duplicate generated declarations.
    minted_newtypes: HashSet<String>,
}

fn lowered_type_member(item: Item<Lowered>) -> TypeRecMember<Lowered> {
    match item {
        Item::TypeAlias(alias) => TypeRecMember::TypeAlias(alias),
        Item::Newtype(newtype) => TypeRecMember::Newtype(newtype),
        _ => unreachable!("label elaboration produces only aliases and newtypes"),
    }
}

fn lowered_type_member_name(member: &TypeRecMember<Lowered>) -> &str {
    match member {
        TypeRecMember::TypeAlias(alias) => &alias.name,
        TypeRecMember::Newtype(newtype) => &newtype.name,
        TypeRecMember::Labels(_, ext) => match *ext {},
    }
}

fn lowered_type_member_name_span(member: &TypeRecMember<Lowered>) -> Span {
    match member {
        TypeRecMember::TypeAlias(alias) => alias.name_span,
        TypeRecMember::Newtype(newtype) => newtype.name_span,
        TypeRecMember::Labels(_, ext) => match *ext {},
    }
}

fn surface_type_member_name_span(member: &TypeRecMember<Desugared>) -> Span {
    match member {
        TypeRecMember::TypeAlias(alias) => alias.name_span,
        TypeRecMember::Newtype(newtype) => newtype.name_span,
        TypeRecMember::Labels(labels, ()) => labels.type_alias_span.unwrap_or_else(|| {
            Span::new(
                labels.meta.span.start,
                labels.meta.span.start.saturating_add(6),
            )
        }),
    }
}

fn lowered_type_member_item(member: TypeRecMember<Lowered>) -> Item<Lowered> {
    match member {
        TypeRecMember::TypeAlias(alias) => Item::TypeAlias(alias),
        TypeRecMember::Newtype(newtype) => Item::Newtype(newtype),
        TypeRecMember::Labels(_, ext) => match ext {},
    }
}

/// Analyze every possibly projected Surface recursive-type group through the
/// same expanded declaration graph used by label elaboration. The module is
/// cloned, desugared, and indexed once; total work is proportional to the
/// module plus the declaration graphs rather than repeating a module walk for
/// each group.
///
/// Only type-declaring items are desugared. This keeps the projection
/// independent of unrelated expression lowering while retaining every local
/// label and type spelling needed to mint and resolve generated label heads.
/// Each returned analysis is paired with its group span and indexed by that
/// group's written members, in source group order.
#[cfg(any(feature = "cli", test))]
pub(crate) fn analyze_surface_type_rec_groups(
    module: &Module<crate::ast::Surface>,
) -> Result<Vec<(Span, Option<crate::pass::resolve::TypeRecAnalysis>)>, Error> {
    let mut analysis_module = module.clone();
    analysis_module.imports.clear();
    analysis_module.items.retain(|item| {
        matches!(
            item,
            Item::TypeAlias(_)
                | Item::Newtype(_)
                | Item::Labels(_, _)
                | Item::TypeRecGroup(_)
                | Item::HostType(_)
        )
    });
    let desugared = crate::pass::desugar::desugar_module(analysis_module)?;
    let table = build_label_table(&desugared)?;
    let module_path = desugared.path.segments;
    let mut analyses = Vec::new();
    for item in desugared.items {
        let Item::TypeRecGroup(group) = item else {
            continue;
        };
        let group_span = group.meta.span;
        let source_members = group.members.len();
        let mut visitor = LabelElabVisitor {
            table: &table,
            module_path: &module_path,
            terminal_aliases: TerminalAliases::default(),
            bound: Vec::new(),
            minted_newtypes: HashSet::new(),
        };
        let (members, owners) = visitor.lower_type_rec_members(group.members)?;
        let analysis = crate::pass::resolve::analyze_type_rec_members(&members);
        analyses.push((
            group_span,
            crate::pass::resolve::source_partition_analysis(&analysis, &owners, source_members),
        ));
    }
    Ok(analyses)
}

fn alias_cycle_error(
    members: &[TypeRecMember<Lowered>],
    analysis: &crate::pass::resolve::TypeRecAnalysis,
) -> Option<Error> {
    let cycle = analysis.alias_cycle.as_ref()?;
    let first = cycle[0];
    let primary = analysis.edge_spans[first]
        .iter()
        .find(|(target, _)| cycle.contains(target))
        .map_or(members[first].meta().span, |(_, span)| *span);
    let mut error = Error::totality(
        primary,
        "recursive type component has no `newtype` boundary",
    );
    for &index in cycle {
        error = error.with_secondary(
            lowered_type_member_name_span(&members[index]),
            format!(
                "transparent alias `{}` participates in this cycle",
                lowered_type_member_name(&members[index])
            ),
        );
    }
    Some(error.with_help("every recursive type cycle must cross a nominal `newtype` boundary"))
}

fn emit_recursive_partition(
    members: Vec<TypeRecMember<Lowered>>,
    analysis: &crate::pass::resolve::TypeRecAnalysis,
    rec_span: Span,
    group_span: Span,
) -> Vec<Item<Lowered>> {
    let mut members = members.into_iter().map(Some).collect::<Vec<_>>();
    let mut out = Vec::new();
    for component_index in crate::pass::resolve::type_rec_component_order(analysis) {
        let component = &analysis.components[component_index];
        let cyclic = analysis.component_is_cyclic(component);
        let mut component_members = component
            .iter()
            .map(|&index| members[index].take().expect("member emitted exactly once"))
            .collect::<Vec<_>>();
        if !cyclic {
            debug_assert_eq!(component_members.len(), 1);
            out.push(lowered_type_member_item(component_members.pop().unwrap()));
        } else if component_members.len() == 1 {
            let member = component_members.pop().unwrap();
            match member {
                TypeRecMember::Newtype(mut newtype) => {
                    newtype.rec_span = Some(rec_span);
                    out.push(Item::Newtype(newtype));
                }
                TypeRecMember::TypeAlias(_) => {
                    unreachable!("alias-only singleton cycles are rejected before emission")
                }
                TypeRecMember::Labels(_, ext) => match ext {},
            }
        } else {
            out.push(Item::TypeRecGroup(TypeRecGroup {
                members: component_members,
                doc: None,
                source_layout: None,
                rec_span: None,
                open_brace_span: None,
                close_brace_span: None,
                deferred_rec_labels_diagnostic: None,
                meta: Meta::new(group_span),
            }));
        }
    }
    out
}

impl DesugaredToLowered for LabelElabVisitor<'_> {
    // The visit_* overrides below re-destructure their variant with a
    // let-else; the walk_* dispatcher routes each variant to its own
    // visit_*, so that let-else can fail only if a dispatcher routed
    // the wrong variant — a bug in this file, hence the bare
    // `unreachable!()`.
    fn rewrite_expr_label_value(
        &mut self,
        labels: Vec<LabelValueLabel<Desugared>>,
        meta: Meta<Desugared>,
        ext: crate::ast::NodeId,
    ) -> Result<Expr<Lowered>, Error> {
        self.lower_label_value(labels, meta.span, ext)
    }

    fn rewrite_type_label_sugar(
        &mut self,
        labels: Vec<LabelSugarLabel<Desugared>>,
        meta: Meta<Desugared>,
    ) -> Result<Type<Lowered>, Error> {
        self.lower_label_sugar_type(labels, meta.span)
    }

    fn rewrite_item_label_forward(
        &mut self,
        _forward: crate::ast::LabelForward<Desugared>,
    ) -> Result<Vec<Item<Lowered>>, Error> {
        Ok(Vec::new())
    }

    fn rewrite_item_labels(&mut self, d: Labels<Desugared>) -> Result<Vec<Item<Lowered>>, Error> {
        let rec_span = d.rec_span;
        let span = d.meta.span;
        let head_span = d
            .type_alias_span
            .unwrap_or_else(|| Span::new(span.start, span.start.saturating_add(6)));
        let has_named_alias = d.type_alias_name.is_some();
        let lowered = self.lower_labels(d)?;
        let members = lowered
            .into_iter()
            .map(lowered_type_member)
            .collect::<Vec<_>>();
        let analysis = crate::pass::resolve::analyze_type_rec_members(&members);
        let Some(rec_span) = rec_span else {
            if let Some(component) = analysis.cyclic_components.first() {
                let primary = component
                    .iter()
                    .find_map(|&from| {
                        analysis.edge_spans[from]
                            .iter()
                            .find(|(target, _)| component.contains(target))
                            .map(|(_, span)| *span)
                    })
                    .unwrap_or(span);
                // Keep the repaired declaration's entire atomic generated
                // scope together until the shared signature checker has
                // validated every member. It then publishes the tailored
                // marker diagnostic; any kind, arity, grounding, or
                // positivity defect wins first with its ordinary error.
                return Ok(vec![Item::TypeRecGroup(TypeRecGroup {
                    members,
                    doc: None,
                    source_layout: None,
                    rec_span: None,
                    open_brace_span: None,
                    close_brace_span: None,
                    deferred_rec_labels_diagnostic: Some(DeferredRecLabelsDiagnostic {
                        reference_span: primary,
                        declaration_span: span,
                        head_span,
                        has_named_alias,
                    }),
                    meta: Meta::new(span),
                })]);
            }
            return Ok(members.into_iter().map(lowered_type_member_item).collect());
        };
        if let Some(error) = alias_cycle_error(&members, &analysis) {
            return Err(error);
        }
        if analysis.cyclic_components.is_empty() {
            return Err(Error::parse(rec_span, "this `rec` marker is unnecessary")
                .with_secondary(
                    head_span,
                    "this labels declaration has no recursive component",
                )
                .with_help("remove `rec` from this acyclic labels declaration")
                .with_fix(
                    Fix::machine_applicable(
                        "Remove unnecessary `rec`",
                        vec![crate::pass::resolve::remove_type_rec_marker_edit(
                            rec_span, span.start,
                        )],
                    )
                    .allowing_follow_on_reanalysis_outside(span),
                ));
        }
        for component in &analysis.cyclic_components {
            if !component
                .iter()
                .any(|&index| matches!(members[index], TypeRecMember::Newtype(_)))
            {
                return Err(Error::totality(
                    members[component[0]].meta().span,
                    "recursive type component has no `newtype` boundary",
                ));
            }
        }
        Ok(emit_recursive_partition(members, &analysis, rec_span, span))
    }

    fn rewrite_type_rec_group(
        &mut self,
        group: TypeRecGroup<Desugared>,
    ) -> Result<Vec<Item<Lowered>>, Error> {
        let source_group = group.clone();
        let group_span = group.meta.span;
        let rec_span = group
            .rec_span
            .unwrap_or_else(|| Span::new(group_span.start, group_span.start.saturating_add(3)));
        let group_delimiters = (
            group.rec_span,
            group.open_brace_span,
            group.close_brace_span,
        );
        let source_member_spans = group
            .source_layout
            .as_ref()
            .map(|layout| layout.member_spans.clone())
            .unwrap_or_default();
        let source_member_marker_offsets = group
            .source_layout
            .as_ref()
            .map(|layout| layout.member_marker_offsets.clone())
            .unwrap_or_default();
        let source_separator_spans = group
            .source_layout
            .as_ref()
            .map(|layout| layout.separator_spans.clone())
            .unwrap_or_default();
        let trailing_comment_span = group
            .source_layout
            .as_ref()
            .and_then(|layout| layout.trailing_comment_span);
        let source_member_name_spans = group
            .members
            .iter()
            .map(surface_type_member_name_span)
            .collect::<Vec<_>>();
        let source_member_names = group
            .members
            .iter()
            .map(|member| match member {
                TypeRecMember::TypeAlias(alias) => alias.name.clone(),
                TypeRecMember::Newtype(newtype) => newtype.name.clone(),
                TypeRecMember::Labels(labels, ()) => labels
                    .type_alias_name
                    .clone()
                    .unwrap_or_else(|| "<anonymous labels>".to_owned()),
            })
            .collect::<Vec<_>>();
        let source_partition_is_safe = source_member_spans.len() == group.members.len()
            && source_member_marker_offsets.len() == group.members.len();

        #[derive(Clone, Copy)]
        enum SingletonKind {
            Alias,
            Newtype,
            Labels,
        }

        let singleton = (group.members.len() == 1).then(|| {
            let member = &group.members[0];
            let kind = match member {
                TypeRecMember::TypeAlias(_) => SingletonKind::Alias,
                TypeRecMember::Newtype(_) => SingletonKind::Newtype,
                TypeRecMember::Labels(_, _) => SingletonKind::Labels,
            };
            (kind, surface_type_member_name_span(member))
        });
        let nested_marker = group.members.iter().find_map(|member| match member {
            TypeRecMember::TypeAlias(_) => None,
            TypeRecMember::Newtype(newtype) => newtype
                .rec_span
                .map(|span| (span, newtype.meta.span.start, newtype.name_span)),
            TypeRecMember::Labels(labels, _) => labels.rec_span.map(|span| {
                (
                    span,
                    labels.meta.span.start,
                    labels.type_alias_span.unwrap_or_else(|| {
                        Span::new(
                            labels.meta.span.start,
                            labels.meta.span.start.saturating_add(6),
                        )
                    }),
                )
            }),
        });

        let surface_members = group.members.len();
        let (members, owners) = self.lower_type_rec_members(group.members)?;
        let analysis = crate::pass::resolve::analyze_type_rec_members(&members);
        let source_analysis =
            crate::pass::resolve::source_partition_analysis(&analysis, &owners, surface_members);
        if let Some((kind, member_name_span)) = singleton {
            let add_singleton_marker = !matches!(kind, SingletonKind::Alias)
                && !analysis.cyclic_components.is_empty()
                && analysis.cyclic_components.iter().all(|component| {
                    component
                        .iter()
                        .any(|&index| matches!(members[index], TypeRecMember::Newtype(_)))
                })
                && crate::pass::resolve::type_rec_marker_fix_is_locally_proven(&members, &analysis);
            let can_unwrap = analysis.cyclic_components.is_empty()
                || add_singleton_marker
                || nested_marker.is_some();
            let mut error = Error::parse(
                rec_span,
                "one recursive data declaration uses a `rec` modifier, not a group",
            )
            .with_secondary(
                group_delimiters.1.unwrap_or(rec_span),
                "the singleton group opens here",
            )
            .with_secondary(
                group_delimiters.2.unwrap_or(rec_span),
                "the singleton group closes here",
            )
            .with_secondary(member_name_span, "this is the group's only declaration")
            .with_help(if nested_marker.is_some() {
                "unwrap the group; the declaration already has its required `rec` modifier"
            } else if add_singleton_marker {
                match kind {
                    SingletonKind::Newtype => {
                        "unwrap the group and write `rec newtype` for this recursive singleton"
                    }
                    SingletonKind::Labels => {
                        "unwrap the group and write `rec labels` for this recursive singleton"
                    }
                    SingletonKind::Alias => unreachable!("aliases have no singleton marker"),
                }
            } else if analysis.cyclic_components.is_empty() {
                "unwrap this acyclic declaration without a `rec` marker"
            } else {
                "unwrap the group only after repairing the declaration's recursive type errors"
            });
            if can_unwrap
                && let Some(fix) = crate::pass::resolve::type_rec_unwrap_fix(
                    &source_group,
                    add_singleton_marker && nested_marker.is_none(),
                )
            {
                error = error.with_fix(fix);
            }
            return Err(error);
        }
        if let Some((marker_span, member_start, member_name_span)) = nested_marker {
            return Err(Error::parse(
                marker_span,
                "the enclosing `rec { ... }` already supplies recursive scope",
            )
            .with_secondary(
                member_name_span,
                "this member is scoped by the enclosing group",
            )
            .with_help("remove the nested `rec` marker")
            .with_fix(
                Fix::machine_applicable(
                    "Fix recursive type groups",
                    vec![crate::pass::resolve::remove_type_rec_marker_edit(
                        marker_span,
                        member_start,
                    )],
                )
                .allowing_follow_on_reanalysis_outside(group_span),
            ));
        }
        if let Some(error) = alias_cycle_error(&members, &analysis) {
            return Err(error);
        }
        if analysis.cyclic_components.is_empty() {
            let mut error = Error::parse(rec_span, "this `rec` group contains no recursive cycle");
            let order = source_analysis
                .as_ref()
                .map(crate::pass::resolve::type_rec_component_order)
                .unwrap_or_else(|| crate::pass::resolve::type_rec_component_order(&analysis));
            for component_index in order {
                let component = source_analysis
                    .as_ref()
                    .map_or(&analysis.components[component_index], |source| {
                        &source.components[component_index]
                    });
                let index = component[0];
                error = error.with_secondary(
                    source_analysis.as_ref().map_or_else(
                        || lowered_type_member_name_span(&members[index]),
                        |_| source_member_name_spans[index],
                    ),
                    format!(
                        "`{}` is acyclic",
                        if source_analysis.is_some() {
                            &source_member_names[index]
                        } else {
                            lowered_type_member_name(&members[index])
                        }
                    ),
                );
            }
            error = error.with_help(
                "move each dependency before its users and remove the unnecessary group",
            );
            if source_partition_is_safe
                && let (Some(open), Some(close)) = (group_delimiters.1, group_delimiters.2)
                && let Some(source_analysis) = source_analysis.as_ref()
                && let Some(fix) = crate::pass::resolve::type_rec_partition_fix_from_spans(
                    group_span,
                    (open, close),
                    &source_member_spans,
                    &source_member_marker_offsets,
                    &source_separator_spans,
                    trailing_comment_span,
                    source_analysis,
                )
            {
                error = error.with_fix(fix);
            }
            return Err(error);
        }
        if analysis.cyclic_components.len() != 1 {
            let mut error = Error::parse(
                rec_span,
                "this `rec` group contains multiple independent components",
            )
            .with_help("split each genuinely mutual component into its own `rec` group");
            for component in &analysis.cyclic_components {
                let lowered_index = component[0];
                let source_index = owners[lowered_index];
                let lowered_name = lowered_type_member_name(&members[lowered_index]);
                let source_name = &source_member_names[source_index];
                let label = if lowered_name == source_name {
                    format!("`{source_name}` starts an independent recursive component")
                } else {
                    format!(
                        "generated declaration `{lowered_name}` starts an independent recursive component of `{source_name}`"
                    )
                };
                error = error.with_secondary(source_member_name_spans[source_index], label);
            }
            if source_partition_is_safe
                && let (Some(open), Some(close)) = (group_delimiters.1, group_delimiters.2)
                && let Some(source_analysis) = source_analysis.as_ref()
                && let Some(fix) = crate::pass::resolve::type_rec_partition_fix_from_spans(
                    group_span,
                    (open, close),
                    &source_member_spans,
                    &source_member_marker_offsets,
                    &source_separator_spans,
                    trailing_comment_span,
                    source_analysis,
                )
            {
                error = error.with_fix(fix);
            }
            return Err(error);
        }
        let recursive_component = &analysis.cyclic_components[0];
        let mut participating = vec![false; surface_members];
        for &index in recursive_component {
            participating[owners[index]] = true;
        }
        if let Some(unrelated) = participating.iter().position(|participates| !participates) {
            let mut error = Error::parse(
                rec_span,
                "this `rec` group contains multiple independent components",
            )
            .with_secondary(
                source_member_name_spans[unrelated],
                format!(
                    "`{}` does not participate in the mutual component",
                    source_member_names[unrelated]
                ),
            )
            .with_help("move the unrelated declaration outside the recursive group");
            if source_partition_is_safe
                && let (Some(open), Some(close)) = (group_delimiters.1, group_delimiters.2)
                && let Some(source_analysis) = source_analysis.as_ref()
                && let Some(fix) = crate::pass::resolve::type_rec_partition_fix_from_spans(
                    group_span,
                    (open, close),
                    &source_member_spans,
                    &source_member_marker_offsets,
                    &source_separator_spans,
                    trailing_comment_span,
                    source_analysis,
                )
            {
                error = error.with_fix(fix);
            }
            return Err(error);
        }
        Ok(emit_recursive_partition(
            members, &analysis, rec_span, group_span,
        ))
    }

    fn visit_expr_elaborator(&mut self, e: Expr<Desugared>) -> Result<Expr<Lowered>, Error> {
        let Expr::Elaborator {
            occurrence: _,
            kind,
            call,
            meta: Meta { span, .. },
            ext,
        } = e
        else {
            unreachable!()
        };
        let call = match call {
            ElaboratorCall::FieldAccess { receiver, labels } => ElaboratorCall::FieldAccess {
                receiver: Box::new(self.walk_expr(*receiver)?),
                labels: labels
                    .into_iter()
                    .map(|label| self.resolve_field_access_label(label))
                    .collect::<Result<_, _>>()?,
            },
            ElaboratorCall::FieldUpdate { receiver, updates } => ElaboratorCall::FieldUpdate {
                receiver: Box::new(self.walk_expr(*receiver)?),
                updates: updates
                    .into_iter()
                    .map(|update| self.resolve_field_update_label(update))
                    .collect::<Result<_, _>>()?,
            },
        };
        Ok(Expr::Elaborator {
            occurrence: Default::default(),
            kind,
            call,
            meta: Meta::new(span),
            ext,
        })
    }

    /// Value-position paths are ordinary value paths. Labels do
    /// not introduce projector aliases; so `foo` remains available for
    /// user-defined values.
    fn visit_expr_path(&mut self, e: Expr<Desugared>) -> Result<Expr<Lowered>, Error> {
        let Expr::Path {
            occurrence: _,
            segments,
            meta: Meta { span, .. },
            ext: (),
        } = e
        else {
            unreachable!()
        };
        Ok(Expr::Path {
            occurrence: Default::default(),
            segments,
            meta: Meta::new(span),
            ext: (),
        })
    }

    // ---- binder-scope overrides --------------------------------------
    //
    // These override the trait defaults only to push / pop the
    // `bound` shadow stack around each binder's body — the
    // recursion itself is identical to the default. `visit_expr_path`
    // reads `bound` to skip the label-name diagnostic for a locally-bound
    // head. `Expr::Seq` carries no binder, so it keeps the default.

    fn walk_fn_def(&mut self, d: FnDef<Desugared>) -> Result<FnDef<Lowered>, Error> {
        let FnDef {
            vis,
            purity,
            name,
            sig,
            ret,
            ret_elided,
            body,
            meta: Meta { span, .. },
            doc,
        } = d;
        // Type-param binders scope over the param types, the return
        // type, and the body — push them before walking any of those.
        let mark = self.save_bound();
        self.push_type_params(&sig.params);
        let params = self.walk_signature_params(sig.params)?;
        let ret = self.walk_type(ret)?;
        for p in &params {
            if let SignatureParam::Value(vp) = p {
                self.bound.push(vp.name.clone());
            }
        }
        let body = self.walk_expr(body)?;
        self.restore_bound(mark);
        Ok(FnDef {
            vis,
            purity,
            name,
            sig: Signature::from_parts(params, sig.groups),
            ret,
            ret_elided,
            body,
            meta: Meta::new(span),
            doc,
        })
    }

    fn visit_expr_fn(&mut self, e: Expr<Desugared>) -> Result<Expr<Lowered>, Error> {
        let Expr::FnExpr {
            occurrence: _,
            sig,
            ret_ty,
            body,
            meta: Meta { span, .. },
            caps: _,
        } = e
        else {
            unreachable!()
        };
        let mark = self.save_bound();
        self.push_type_params(&sig.params);
        let params = self.walk_signature_params(sig.params)?;
        let ret_ty = ret_ty.map(|t| self.walk_type(t)).transpose()?;
        for p in &params {
            if let SignatureParam::Value(vp) = p {
                self.bound.push(vp.name.clone());
            }
        }
        let body = Box::new(self.walk_expr(*body)?);
        self.restore_bound(mark);
        Ok(Expr::FnExpr {
            occurrence: Default::default(),
            sig: Signature::from_parts(params, sig.groups),
            ret_ty,
            body,
            meta: Meta::new(span),
            caps: (),
        })
    }

    fn visit_expr_let(&mut self, e: Expr<Desugared>) -> Result<Expr<Lowered>, Error> {
        let Expr::Let {
            occurrence: _,
            name,
            name_span,
            ty,
            pattern: (),
            value,
            body,
            meta: Meta { span, .. },
        } = e
        else {
            unreachable!()
        };
        // The RHS is checked *before* `name` enters scope — a `let`
        // binding is not in scope of its own value expression.
        let value = Box::new(self.walk_expr(*value)?);
        let mark = self.save_bound();
        self.bound.push(name.clone());
        let body = Box::new(self.walk_expr(*body)?);
        self.restore_bound(mark);
        Ok(Expr::Let {
            occurrence: Default::default(),
            name,
            name_span,
            ty: ty.map(|t| self.walk_type(t)).transpose()?,
            pattern: (),
            value,
            body,
            meta: Meta::new(span),
        })
    }

    fn walk_equiv(&mut self, e: Equiv<Desugared>) -> Result<Equiv<Lowered>, Error> {
        let Equiv {
            name,
            name_span,
            sig,
            terms,
            meta: Meta { span, .. },
        } = e;
        let mark = self.save_bound();
        self.push_type_params(&sig.params);
        let params = self.walk_signature_params(sig.params)?;
        for p in &params {
            if let SignatureParam::Value(vp) = p {
                self.bound.push(vp.name.clone());
            }
        }
        let terms: Vec<crate::ast::EquivTerm<Lowered>> = terms
            .into_iter()
            .map(|t| {
                let crate::ast::EquivTerm {
                    body,
                    meta: Meta { span: tspan, .. },
                } = t;
                Ok(crate::ast::EquivTerm {
                    body: self.walk_expr(body)?,
                    meta: Meta::new(tspan),
                })
            })
            .collect::<Result<_, _>>()?;
        self.restore_bound(mark);
        Ok(Equiv {
            name,
            name_span,
            sig: Signature::from_parts(params, sig.groups),
            terms,
            meta: Meta::new(span),
        })
    }

    /// Walk a type alias Desugared → Lowered. The header type-params
    /// (`type Foo[A] = body`) scope over the body, so push them
    /// onto the shadow stack around the body walk — a `[A]` shadows a
    /// same-named label in `visit_type_path`.
    fn walk_alias(&mut self, a: TypeAlias<Desugared>) -> Result<TypeAlias<Lowered>, Error> {
        let TypeAlias {
            vis,
            name,
            name_span,
            type_params,
            body,
            meta: Meta { span, .. },
            editable_span,
            doc,
        } = a;
        let mark = self.save_bound();
        for tp in &type_params {
            self.bound.push(tp.name.clone());
        }
        let body = self.walk_type(body)?;
        self.restore_bound(mark);
        Ok(TypeAlias {
            vis,
            name,
            name_span,
            type_params,
            body,
            meta: Meta::new(span),
            editable_span,
            doc,
        })
    }

    /// Walk a `newtype` Desugared → Lowered. The header's universal
    /// and existential type-params (`newtype N[A] : payload`) scope
    /// over the payload, so push both around the payload walk.
    fn walk_newtype(&mut self, d: Newtype<Desugared>) -> Result<Newtype<Lowered>, Error> {
        let Newtype {
            vis,
            rec_span,
            name,
            name_span,
            type_params,
            existential_params,
            payload,
            constructor,
            projector,
            meta: Meta { span, .. },
            editable_span,
            doc,
        } = d;
        let mark = self.save_bound();
        for tp in type_params.iter().chain(existential_params.iter()) {
            self.bound.push(tp.name.clone());
        }
        let payload = self.walk_type(payload)?;
        self.restore_bound(mark);
        Ok(Newtype {
            vis,
            rec_span,
            name,
            name_span,
            type_params,
            existential_params,
            payload,
            constructor: convert_type_member(&constructor),
            projector: convert_type_member(&projector),
            meta: Meta::new(span),
            editable_span,
            doc,
        })
    }

    /// Walk a `Type::Forall` Desugared → Lowered. The mid-type
    /// binders (`[A] P -> R`) scope over the body, so push them
    /// around the body walk.
    fn visit_type_forall(&mut self, ty: Type<Desugared>) -> Result<Type<Lowered>, Error> {
        let Type::Forall {
            param,
            body,
            meta: Meta { span, .. },
        } = ty
        else {
            unreachable!()
        };
        let mark = self.save_bound();
        self.bound.push(param.name.clone());
        let body = Box::new(self.walk_type(*body)?);
        self.restore_bound(mark);
        Ok(Type::Forall {
            param,
            body,
            meta: Meta::new(span),
        })
    }

    /// Type-position paths are ordinary type paths. If the head is a
    /// visible label, reject it with a directed diagnostic; type
    /// syntax names the generated newtype (`Foo`), not the value label
    /// (`foo`). A type-parameter binder named `foo` remains ordinary and
    /// shadows this diagnostic.
    fn visit_type_path(&mut self, ty: Type<Desugared>) -> Result<Type<Lowered>, Error> {
        let Type::Path {
            segments,
            args,
            meta: Meta { span, .. },
        } = ty
        else {
            unreachable!()
        };
        let args: Vec<Type<Lowered>> = args
            .into_iter()
            .map(|a| self.walk_type(a))
            .collect::<Result<_, _>>()?;
        if segments.len() == 1 && !self.is_bound(segments[0].as_str()) {
            let label = segments[0].as_str();
            if let Some(info) = self.table.get(label) {
                info.ensure_accessible(label, segments[0].span, self.module_path)?;
                return Err(Error::name_res(
                    segments[0].span,
                    format!(
                        "label `{label}` is value syntax; use generated type `{}` in type position",
                        info.newtype_name
                    ),
                ));
            }
        }
        Ok(Type::synth_path_segments(segments, args, span))
    }
}

impl LabelElabVisitor<'_> {
    fn lower_type_rec_members(
        &mut self,
        source: Vec<TypeRecMember<Desugared>>,
    ) -> Result<(Vec<TypeRecMember<Lowered>>, Vec<usize>), Error> {
        let mut members = Vec::new();
        let mut owners = Vec::new();
        for (owner, member) in source.into_iter().enumerate() {
            match member {
                TypeRecMember::TypeAlias(alias) => {
                    members.push(TypeRecMember::TypeAlias(self.walk_alias(alias)?));
                    owners.push(owner);
                }
                TypeRecMember::Newtype(newtype) => {
                    members.push(TypeRecMember::Newtype(self.walk_newtype(newtype)?));
                    owners.push(owner);
                }
                TypeRecMember::Labels(labels, _) => {
                    for item in self.lower_labels(labels)? {
                        members.push(lowered_type_member(item));
                        owners.push(owner);
                    }
                }
            }
        }
        Ok((members, owners))
    }

    /// True iff `name` is currently in scope as a local binder.
    /// Consulted by [`Self::visit_type_path`] so a type-parameter
    /// binder shadows a same-named label diagnostic.
    fn is_bound(&self, name: &str) -> bool {
        self.bound.iter().any(|n| n == name)
    }

    /// The `label … is not in scope` diagnostic, enriched with a
    /// `did you mean …?` suggestion when a near-miss label is in the
    /// effective table. The candidate pool is every label name the table
    /// knows — local declarations plus selectively- and qualified-curly-
    /// imported labels — so a typo on any earns the nearest-match
    /// suggestion (the phase-10 rubric for name-resolution diagnostics).
    fn label_not_in_scope(&self, label: &str, label_span: Span) -> Error {
        let err = Error::name_res(label_span, format!("label `{label}` is not in scope"));
        let mut candidates: Vec<&str> = self
            .table
            .iter()
            .filter(|(_, info)| info.accessible)
            .map(|(name, _)| name.as_str())
            .collect();
        candidates.sort_unstable();
        match crate::error::closest_name(label, candidates.iter().copied()) {
            Some(near) => err
                .with_help(format!(
                    "a label `{near}` is in scope — did you mean `{near}`?"
                ))
                .with_suggestion(label_span, near.to_owned()),
            None => err,
        }
    }

    fn resolve_field_access_label(
        &mut self,
        label: FieldAccessLabel<Desugared>,
    ) -> Result<FieldAccessLabel<Lowered>, Error> {
        let FieldAccessLabel {
            label,
            label_span,
            label_type: _,
            meta: Meta { span, .. },
        } = label;
        let info = self
            .table
            .get(&label)
            .ok_or_else(|| self.label_not_in_scope(&label, label_span))?;
        info.ensure_accessible(&label, label_span, self.module_path)?;
        let label_type =
            info.newtype_path(self.module_path, label_span, &mut self.terminal_aliases);
        Ok(FieldAccessLabel {
            label,
            label_span,
            label_type: Some(label_type),
            meta: Meta::new(span),
        })
    }

    fn resolve_field_update_label(
        &mut self,
        update: FieldUpdateLabel<Desugared>,
    ) -> Result<FieldUpdateLabel<Lowered>, Error> {
        let FieldUpdateLabel {
            label,
            label_span,
            label_type: _,
            value,
            meta: Meta { span, .. },
        } = update;
        let info = self
            .table
            .get(&label)
            .ok_or_else(|| self.label_not_in_scope(&label, label_span))?;
        info.ensure_accessible(&label, label_span, self.module_path)?;
        let label_type =
            info.newtype_path(self.module_path, label_span, &mut self.terminal_aliases);
        let value = self.walk_expr(value)?;
        Ok(FieldUpdateLabel {
            label,
            label_span,
            label_type: Some(label_type),
            value,
            meta: Meta::new(span),
        })
    }

    /// Push every type-parameter binder in a signature onto the
    /// shadow stack. Used by the signature-bearing walker overrides
    /// (`fn` / `fn` expr / `match!` clause / `equiv`) so a `[A]`
    /// shadows a same-named label in the param types, return type, and
    /// body.
    fn push_type_params(&mut self, params: &[SignatureParam<Desugared>]) {
        for p in params {
            if let SignatureParam::Type(tp) = p {
                self.bound.push(tp.name.clone());
            }
        }
    }

    fn save_bound(&self) -> usize {
        self.bound.len()
    }

    fn restore_bound(&mut self, mark: usize) {
        self.bound.truncate(mark);
    }

    /// Lower one `labels` declaration into the equivalent items: per
    /// entry, one [`Item::Newtype`], plus — for the named form — one
    /// [`Item::TypeAlias`] over the written product/sum of generated label
    /// types. Generated label newtypes expose the uniform members `mk`
    /// and `get`; labels do not introduce top-level value aliases.
    fn lower_labels(&mut self, d: Labels<Desugared>) -> Result<Vec<Item<Lowered>>, Error> {
        // Per-entry leading trivia is dropped here: the Labels item
        // ceases to exist at Lowered (replaced by generated newtypes
        // and the optional alias), so the surface comments don't have
        // a target. They survive `kio fmt` round-trips on the Surface AST.
        let declaration_entries: Vec<LabelEntry<Desugared>> = label_entries_in_source_order(&d)?
            .into_iter()
            .filter(|entry| !entry.is_reuse_marker())
            .cloned()
            .collect();
        let source_owner_span = label_source_owner_span(&d);
        let mut out: Vec<Item<Lowered>> = Vec::with_capacity(declaration_entries.len() + 1);
        // Generate one newtype per explicit declaration. Constructor name `mk`,
        // projector name `get`.
        for entry in &declaration_entries {
            let LabelEntry {
                name,
                name_span: _,
                type_params,
                existential_params,
                payload,
                meta: _,
            } = entry;
            // The entry header's universal and existential type-params
            // scope over the payload — push them so a `[A]` shadows a
            // same-named label in `visit_type_path`.
            let mark = self.save_bound();
            for tp in type_params.iter().chain(existential_params.iter()) {
                self.bound.push(tp.name.clone());
            }
            let lowered_payload = self.walk_type(payload.clone())?;
            self.restore_bound(mark);
            let newtype_name = self
                .table
                .get(name)
                .expect("entry present in build_label_table")
                .newtype_name
                .clone();
            // `build_label_table` rejects every second explicit declaration;
            // the set remains a defensive guard for constructed ASTs.
            if !self.minted_newtypes.insert(newtype_name.clone()) {
                continue;
            }
            out.push(Item::Newtype(label_nominal_declaration(
                &d,
                entry,
                newtype_name,
                lowered_payload,
            )));
        }
        let Labels {
            vis,
            rec_span: _,
            type_alias_name,
            type_alias_span,
            type_alias_params,
            type_alias_arms,
            entries: _,
            meta: Meta { span, .. },
            editable_span,
            doc: _,
        } = d;
        // For the named form, additionally emit a type alias
        // over the product/sum of generated label types.
        if let Some(name) = type_alias_name {
            let arms = type_alias_arms
                .as_deref()
                .expect("named labels carry type-alias arms");
            let body = named_label_alias_body(arms, span, |entry| {
                vec![
                    self.table
                        .get(&entry.name)
                        .expect("entry present in build_label_table")
                        .newtype_name
                        .clone(),
                ]
            });
            out.push(Item::TypeAlias(TypeAlias {
                vis,
                name,
                name_span: type_alias_span.expect("named labels carry a type-name span"),
                type_params: type_alias_params,
                body,
                meta: Meta::new(source_owner_span),
                editable_span,
                doc: None,
            }));
        }
        Ok(out)
    }

    /// Braced label forms are value syntax only. The parser normally
    /// rejects these in type position; this hook keeps the
    /// Desugared → Lowered boundary defensive for constructed ASTs.
    fn lower_label_sugar_type(
        &mut self,
        _labels: Vec<LabelSugarLabel<Desugared>>,
        span: Span,
    ) -> Result<Type<Lowered>, Error> {
        Err(Error::name_res(
            span,
            "labels are value syntax; use generated nominal type names in type position",
        ))
    }

    /// Lower a label-value expression `{f = e1, g = e2}` to the same
    /// private field-update elaborator used by `r.!{...}`, with `()`
    /// as the receiver. The shared typer path decides the default
    /// product order and records the final constructor/projection
    /// elaboration.
    fn lower_label_value(
        &mut self,
        labels: Vec<LabelValueLabel<Desugared>>,
        span: Span,
        ext: crate::ast::NodeId,
    ) -> Result<Expr<Lowered>, Error> {
        if labels.is_empty() {
            return Ok(Expr::Unit {
                occurrence: Default::default(),
                meta: Meta::new(span),
            });
        }
        let mut updates: Vec<FieldUpdateLabel<Lowered>> = Vec::with_capacity(labels.len());
        let mut seen: HashMap<String, Span> = HashMap::new();
        for label in labels {
            let LabelValueLabel {
                label,
                label_span,
                value,
                meta: label_meta,
            } = label;
            let label_full_span = label_meta.span;
            if let Some(first_span) = seen.insert(label.clone(), label_span) {
                return Err(Error::name_res(
                    label_span,
                    format!("duplicate construction label `{label}`"),
                )
                .with_secondary(first_span, format!("`{label}` first written here"))
                .with_help("write each label at most once in a label value"));
            }
            let info = self
                .table
                .get(&label)
                .ok_or_else(|| self.label_not_in_scope(&label, label_span))?;
            info.ensure_accessible(&label, label_span, self.module_path)?;
            let label_type =
                info.newtype_path(self.module_path, label_span, &mut self.terminal_aliases);
            let value = self.walk_expr(value)?;
            updates.push(FieldUpdateLabel {
                label,
                label_span,
                label_type: Some(label_type),
                value,
                meta: Meta::new(label_full_span),
            });
        }
        Ok(Expr::Elaborator {
            occurrence: Default::default(),
            kind: ElaboratorKind::Filtered,
            call: ElaboratorCall::FieldUpdate {
                // Zero-width: this receiver is minted here, not written by the
                // user. Giving it the record literal's own span made it claim
                // that span in the position index — and a query anywhere inside
                // the braces then answered with this unit rather than with the
                // record.
                receiver: Box::new(Expr::Unit {
                    occurrence: Default::default(),
                    meta: Meta::new(Span::new(span.start, span.start)),
                }),
                updates,
            },
            meta: Meta::new(span),
            ext,
        })
    }
}

fn label_source_owner_span<P: crate::ast::Phase>(labels: &Labels<P>) -> Span {
    // One owner span keeps the expansion atomic during cycle analysis;
    // editable_span separately retains the original edit eligibility.
    labels.editable_span.unwrap_or_else(|| {
        labels.doc.as_ref().map_or(labels.meta.span, |doc| {
            Span::new(doc.span.start, labels.meta.span.end)
        })
    })
}

/// Construct the ordinary nominal declaration for an explicit label entry.
/// The caller supplies its selected name and phase-appropriate payload.
pub(crate) fn label_nominal_declaration<P: crate::ast::Phase, Q: crate::ast::Phase>(
    owner: &Labels<P>,
    entry: &LabelEntry<P>,
    name: String,
    payload: Type<Q>,
) -> Newtype<Q> {
    Newtype {
        vis: owner.vis.clone(),
        rec_span: None,
        name,
        name_span: entry.name_span,
        type_params: entry.type_params.clone(),
        existential_params: entry.existential_params.clone(),
        payload,
        constructor: TypeMember {
            vis: owner.vis.clone(),
            name: "mk".to_owned(),
            span: entry.name_span,
            leading_trivia: Q::LeadingTrivia::default(),
        },
        projector: TypeMember {
            vis: owner.vis.clone(),
            name: "get".to_owned(),
            span: entry.name_span,
            leading_trivia: Q::LeadingTrivia::default(),
        },
        meta: Meta::new(label_source_owner_span(owner)),
        editable_span: owner.editable_span,
        // The written labels owner carries documentation; its nominal
        // expansion is not a second documented declaration.
        doc: None,
    }
}

/// Build the named owner's written product/sum with explicit nominal paths.
/// Universal arguments retain the entry's own written names and spans;
/// existential binders remain solely inside the nominal declaration.
pub(crate) fn named_label_alias_body<P, Q>(
    arms: &[LabelsArm<P>],
    span: Span,
    mut nominal_path: impl FnMut(&LabelEntry<P>) -> Vec<String>,
) -> Type<Q>
where
    P: crate::ast::Phase,
    Q: crate::ast::Phase + Clone,
{
    let arm_types = arms
        .iter()
        .map(|arm| {
            let parts = arm
                .entries
                .iter()
                .map(|entry| {
                    let args = entry
                        .type_params
                        .iter()
                        .map(|param| {
                            Type::synth_path(vec![param.name.clone()], Vec::new(), param.span)
                        })
                        .collect();
                    Type::synth_path(nominal_path(entry), args, entry.meta.span)
                })
                .collect::<Vec<Type<Q>>>();
            crate::pass::typecheck_core::right_fold_product(&parts, arm.meta.span)
        })
        .collect::<Vec<_>>();
    crate::pass::typecheck_core::right_fold_sum(&arm_types, span)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Visibility;
    use crate::pass::desugar::desugar_module;
    use crate::pass::parser::parse;

    fn elab(src: &str) -> Module<Lowered> {
        let m = parse(src).expect("parse");
        let m = desugar_module(m).expect("desugar");
        let parsed = vec![(PathBuf::from("test.kio"), m)];
        let (lowered, _) = elaborate_package(parsed, None).expect("elaborate");
        lowered.into_iter().next().unwrap().1
    }

    fn elab_err(src: &str) -> String {
        elab_error(src).diag().1.to_owned()
    }

    fn elab_error(src: &str) -> Error {
        let m = parse(src).expect("parse");
        let m = desugar_module(m).expect("desugar");
        let parsed = vec![(PathBuf::from("test.kio"), m)];
        match elaborate_package(parsed, None) {
            Ok(_) => panic!("expected elab error"),
            Err(e) => e.error,
        }
    }

    fn elab_modules(
        sources: &[(&str, &str)],
    ) -> Result<Vec<(PathBuf, Module<Lowered>)>, LocatedError> {
        let parsed = sources
            .iter()
            .map(|(file, source)| {
                let module = parse(source).expect("parse");
                let module = desugar_module(module).expect("desugar");
                (PathBuf::from(file), module)
            })
            .collect();
        elaborate_package(parsed, None).map(|(modules, _)| modules)
    }

    #[test]
    fn declaration_arm_traversal_preserves_unvalidated_written_occurrences() {
        let module = parse("module m; labels Choice = { foo: . } | { foo: _ };")
            .expect("parse distinct product arms");
        let Item::Labels(mut labels, _) = module.items.into_iter().next().unwrap() else {
            panic!("expected labels");
        };
        let written_arms = labels.type_alias_arms.as_mut().expect("named arms");
        let reuse = written_arms[1].entries[0].clone();
        written_arms[0].entries.push(reuse);
        let arms = labels.arms_in_source_order().collect::<Vec<_>>();
        assert_eq!(arms.len(), 2);
        assert_eq!(
            arms.iter()
                .map(|arm| arm
                    .iter()
                    .map(|entry| entry.name.as_str())
                    .collect::<Vec<_>>())
                .collect::<Vec<_>>(),
            vec![vec!["foo", "foo"], vec!["foo"]],
        );
        assert!(!arms[0][0].is_reuse_marker());
        assert!(arms[0][1].is_reuse_marker());
        assert!(arms[1][0].is_reuse_marker());
    }

    #[test]
    fn declaration_universal_headers_compare_arity_and_kinds_not_binder_names() {
        let module = parse(
            "module m; \
             labels First[*F][A] = { item[*F][A]: F(A) }; \
             labels Later[*G][B] = { item[*G][B]: _ }; \
             labels Short[A] = { item[A]: _ }; \
             labels Wrong[F][A] = { item[F][A]: _ };",
        )
        .expect("parse declaration headers");
        let entries = module
            .items
            .iter()
            .map(|item| {
                let Item::Labels(labels, _) = item else {
                    panic!("expected labels");
                };
                &labels.arms_in_source_order().next().expect("one arm")[0]
            })
            .collect::<Vec<_>>();
        assert!(entries[0].universal_header_matches(entries[1]));
        assert!(!entries[0].universal_header_matches(entries[2]));
        assert!(!entries[0].universal_header_matches(entries[3]));
    }

    #[test]
    fn declaration_alias_binders_match_written_names_and_kinds() {
        let module = parse(
            "module m; labels Choice[A][B] = \
             { okay[A]: _, missing[C]: _, wrong[*A]: _ };",
        )
        .expect("parse declaration headers before reuse validation");
        let Item::Labels(labels, _) = &module.items[0] else {
            panic!("expected labels");
        };
        let entries = labels.arms_in_source_order().next().expect("one arm");
        assert!(labels.unbound_entry_universal(&entries[0]).is_none());
        assert_eq!(
            labels
                .unbound_entry_universal(&entries[1])
                .expect("unbound name")
                .name,
            "C",
        );
        assert_eq!(
            labels
                .unbound_entry_universal(&entries[2])
                .expect("different kind")
                .name,
            "A",
        );
    }

    #[test]
    fn label_nominal_shape_preserves_headers_visibility_and_owner_spans() {
        let module =
            parse("module m;\n/// Label family.\npub(m) labels { box[*F][A]<X>: F(A) & X };")
                .expect("parse explicit universal/existential label header");
        let Item::Labels(owner, _) = &module.items[0] else {
            panic!("expected labels");
        };
        let entry = &owner.entries[0];
        let nominal =
            label_nominal_declaration(owner, entry, "Chosen".to_owned(), entry.payload.clone());
        assert_eq!(nominal.name, "Chosen");
        assert_eq!(nominal.type_params, entry.type_params);
        assert_eq!(nominal.existential_params, entry.existential_params);
        assert_eq!(nominal.type_params.len(), 2);
        assert_ne!(
            nominal.type_params[0].effective_kind(),
            nominal.type_params[1].effective_kind(),
        );
        assert_eq!(nominal.existential_params.len(), 1);
        assert_eq!(nominal.existential_params[0].name, "X");
        assert_eq!(
            nominal.existential_params[0].effective_kind(),
            nominal.type_params[1].effective_kind(),
        );
        assert_eq!(nominal.payload, entry.payload);
        assert_eq!(nominal.name_span, entry.name_span);
        assert_eq!(
            nominal.meta.span,
            owner.editable_span.expect("editable owner")
        );
        assert_eq!(nominal.editable_span, owner.editable_span);
        assert_eq!(nominal.vis, owner.vis);
        assert_eq!(nominal.constructor.vis, owner.vis);
        assert_eq!(nominal.projector.vis, owner.vis);
        assert_eq!(nominal.constructor.name, "mk");
        assert_eq!(nominal.projector.name, "get");
        assert_eq!(nominal.constructor.span, entry.name_span);
        assert_eq!(nominal.projector.span, entry.name_span);
        assert!(nominal.rec_span.is_none());
        assert!(nominal.doc.is_none());
        assert!(owner.doc.is_some());

        let mut uneditable = owner.clone();
        uneditable.editable_span = None;
        let supplied_payload = Type::<Lowered>::synth_path(
            vec!["SelectedPayload".to_owned()],
            Vec::new(),
            entry.meta.span,
        );
        let lowered = label_nominal_declaration(
            &uneditable,
            entry,
            "Chosen".to_owned(),
            supplied_payload.clone(),
        );
        assert_eq!(lowered.payload, supplied_payload);
        assert_eq!(
            lowered.meta.span,
            Span::new(owner.doc.as_ref().unwrap().span.start, owner.meta.span.end),
        );
        assert!(lowered.editable_span.is_none());
    }

    #[test]
    fn named_label_shape_preserves_arm_order_and_universal_arguments() {
        let module = parse(
            "module m; labels Choice[*F][A][B] = \
             { first[*F][B]<X>: F(B) & X, second[A]: A } | { middle: . } | { first[*F][A]: _ };",
        )
        .expect("parse written product/sum before elaboration");
        let Item::Labels(owner, _) = &module.items[0] else {
            panic!("expected labels");
        };
        let arms = owner.type_alias_arms.as_deref().expect("named arms");
        let mut visited = Vec::new();
        let body: Type<crate::ast::Surface> =
            named_label_alias_body(arms, owner.meta.span, |entry| {
                visited.push(entry.name.clone());
                vec!["selected".to_owned(), mint_label_newtype_name(&entry.name)]
            });
        assert_eq!(visited, ["first", "second", "middle", "first"]);
        let Type::Sum { left, right, .. } = body else {
            panic!("expected first arm followed by remaining sum");
        };
        let Type::Product {
            left: first,
            right: second,
            ..
        } = *left
        else {
            panic!("expected written first product arm");
        };
        let assert_path = |ty: &Type<crate::ast::Surface>, name: &str, arguments: &[&str]| {
            let Type::Path { segments, args, .. } = ty else {
                panic!("expected supplied nominal path");
            };
            assert_eq!(
                segments.iter().map(PathSegment::as_str).collect::<Vec<_>>(),
                ["selected", name],
            );
            let names = args
                .iter()
                .map(|argument| {
                    let Type::Path { segments, args, .. } = argument else {
                        panic!("expected written universal binder reference");
                    };
                    assert!(args.is_empty());
                    assert_eq!(segments.len(), 1);
                    segments[0].as_str()
                })
                .collect::<Vec<_>>();
            assert_eq!(names, arguments);
        };
        assert_path(&first, "First", &["F", "B"]);
        assert_path(&second, "Second", &["A"]);
        let Type::Sum {
            left: middle,
            right: last,
            ..
        } = *right
        else {
            panic!("expected right-associated remaining arms");
        };
        assert_path(&middle, "Middle", &[]);
        assert_path(&last, "First", &["F", "A"]);
        let constructed_empty_arm = LabelsArm::<crate::ast::Surface> {
            entries: Vec::new(),
            meta: Meta::new(owner.meta.span),
        };
        let unit: Type<Lowered> =
            named_label_alias_body(&[constructed_empty_arm], owner.meta.span, |_| {
                panic!("constructed empty product visits no entry")
            });
        assert!(matches!(unit, Type::Unit { .. }));
        let empty: Type<Lowered> =
            named_label_alias_body::<crate::ast::Surface, _>(&[], owner.meta.span, |_| {
                panic!("empty arm list visits no entry")
            });
        assert!(matches!(empty, Type::Bottom { .. }));
    }

    #[test]
    fn package_label_error_orders_by_module_path_then_span() {
        let z = desugar_module(
            parse("module pkg/z; labels { dup : . }; labels { dup : I32 };").expect("parse"),
        )
        .expect("desugar");
        let a = desugar_module(
            parse("module pkg/a; labels { dup : . }; labels { dup : I32 };").expect("parse"),
        )
        .expect("desugar");
        let parsed = vec![
            (PathBuf::from("pkg/z.kio"), z),
            (PathBuf::from("pkg/a.kio"), a),
        ];

        let err = elaborate_package(parsed, None).expect_err("expected label error");

        assert_eq!(err.file_path, PathBuf::from("pkg/a.kio"));
    }

    #[test]
    fn labels_lower_to_newtype() {
        let m = elab("module x; labels { foo : . };");
        // Item::Labels is uninhabited in Lowered, so the static
        // proof is in the type — no need to assert at runtime. We do
        // assert the generated newtype shape. The newtype's name is
        // the surface label spelling with the first letter capitalized
        // (`foo` → `Foo`).
        assert!(matches!(m.items[0], Item::Newtype(_)));
        if let Item::Newtype(d) = &m.items[0] {
            assert_eq!(d.name, "Foo");
            assert_eq!(d.constructor.name, "mk");
            assert_eq!(d.projector.name, "get");
        }
    }

    #[test]
    fn recursive_named_labels_lower_to_a_minimal_nominally_grounded_group() {
        let module = elab(
            "module x; rec labels Tree = \
             { leaf: . } | { branch: Tree & Tree };",
        );
        assert!(matches!(&module.items[0], Item::Newtype(newtype) if newtype.name == "Leaf"));
        let Item::TypeRecGroup(group) = &module.items[1] else {
            panic!("recursive labels must retain an explicit Kio' group");
        };
        assert_eq!(group.members.len(), 2);
        assert!(group.members.iter().any(
            |member| matches!(member, TypeRecMember::Newtype(newtype) if newtype.name == "Branch")
        ));
        assert!(group.members.iter().any(
            |member| matches!(member, TypeRecMember::TypeAlias(alias) if alias.name == "Tree")
        ));
        crate::pass::resolve::Resolver::check_module(&module)
            .expect("the normalized group must independently resolve");
    }

    #[test]
    fn one_recursive_labels_declaration_may_lower_to_multiple_nominal_components() {
        let module = elab(
            "module x; rec labels Pair = \
             { first: . | First, second: . | Second };",
        );
        let recursive_heads = module
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Newtype(newtype) if newtype.rec_span.is_some() => Some(newtype.name.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(recursive_heads, vec!["First", "Second"]);
        assert!(matches!(
            module.items.last(),
            Some(Item::TypeAlias(alias)) if alias.name == "Pair"
        ));
        crate::pass::resolve::Resolver::check_module(&module)
            .expect("each generated nominal SCC must resolve independently");
    }

    #[test]
    fn cyclic_component_classification_label_lowering_is_linear() {
        const COUNT: usize = 128;
        let fields = (0..COUNT)
            .map(|index| format!("field{index}: . | Field{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        crate::pass::resolve::reset_type_rec_classification_work();
        let module = elab(&format!("module x; rec labels Product = {{ {fields} }};"));
        let work = crate::pass::resolve::type_rec_classification_work();
        assert_eq!(module.items.len(), COUNT + 1);
        for (index, item) in module.items[..COUNT].iter().enumerate() {
            let Item::Newtype(newtype) = item else {
                panic!("one nominal per explicit field")
            };
            assert_eq!(newtype.name, format!("Field{index}"));
            assert!(newtype.rec_span.is_some());
        }
        assert!(
            matches!(module.items.last(), Some(Item::TypeAlias(alias)) if alias.name == "Product")
        );
        crate::pass::resolve::Resolver::check_module(&module)
            .expect("all generated components independently resolve");
        let mut expected_source = String::from("module x;");
        for index in 0..COUNT {
            expected_source.push_str(&format!(
                " rec newtype Field{index} : . | Field{index} {{ constructor mk; projector get; }};"
            ));
        }
        expected_source.push_str(&format!(
            " type Product = {};",
            (0..COUNT)
                .map(|index| format!("Field{index}"))
                .collect::<Vec<_>>()
                .join(" & ")
        ));
        let expected = parse(&expected_source).expect("explicit nominal expansion parses");
        let rendered = module
            .items
            .iter()
            .map(|item| match item {
                Item::Newtype(newtype) => crate::pretty::pretty_item_source(&Item::Newtype(
                    crate::ast::convert_newtype(newtype),
                )),
                Item::TypeAlias(alias) => crate::pretty::pretty_item_source(&Item::TypeAlias(
                    crate::ast::convert_type_alias(alias),
                )),
                _ => panic!("only the explicit nominals and final alias are emitted"),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            rendered,
            expected
                .items
                .iter()
                .map(crate::pretty::pretty_item_source)
                .collect::<Vec<_>>()
        );
        assert_eq!(work, (COUNT + 1, COUNT * 2));
    }

    #[test]
    fn cyclic_component_classification_preserves_projected_owner_self_edges() {
        for (fields, cyclic) in [
            ("first: . | First, second: . | Second", true),
            ("first: ., second: .", false),
        ] {
            let mut module = parse(&format!(
                "module x; rec {{\n/// Product fields.\npub labels Product = {{ {fields} }};\nnewtype Other : . | Other {{ constructor mk; projector get; }};\n}}"
            )).unwrap();
            let analyses = analyze_surface_type_rec_groups(&module).unwrap();
            let analysis = analyses[0]
                .1
                .as_ref()
                .expect("atomic owner projection is representable");
            assert_eq!(analysis.components, vec![vec![0], vec![1]]);
            assert_eq!(analysis.edges[0], if cyclic { vec![0] } else { vec![] });
            let Item::TypeRecGroup(group) = module.items.pop().unwrap() else {
                unreachable!()
            };
            let TypeRecMember::Labels(mut labels, ext) = group.members[0].clone() else {
                unreachable!()
            };
            labels.rec_span = if cyclic { group.rec_span } else { None };
            let TypeRecMember::Newtype(mut other) = group.members[1].clone() else {
                unreachable!()
            };
            other.rec_span = group.rec_span;
            let expected = vec![Item::Labels(labels, ext), Item::Newtype(other)];
            crate::pass::resolve::reset_type_rec_classification_work();
            let actual = crate::pass::resolve::emit_type_rec_partition(group, analysis);
            assert_eq!(actual, expected);
            assert_eq!(
                crate::pass::resolve::type_rec_classification_work(),
                (2, if cyclic { 2 } else { 1 })
            );
        }
    }

    #[test]
    fn labels_member_of_a_type_rec_group_is_registered_before_lowering() {
        let module = elab(
            "module x; rec { \
               labels A = { to_b: B }; \
               newtype B : A { constructor mk_b; projector un_b; }; \
             }",
        );
        let Item::TypeRecGroup(group) = &module.items[0] else {
            panic!("the mutually recursive labels/newtype component must remain grouped");
        };
        assert!(
            group.members.iter().any(
                |member| matches!(member, TypeRecMember::TypeAlias(alias) if alias.name == "A")
            )
        );
        assert!(group.members.iter().any(
            |member| matches!(member, TypeRecMember::Newtype(newtype) if newtype.name == "To_b")
        ));
        assert!(group.members.iter().any(
            |member| matches!(member, TypeRecMember::Newtype(newtype) if newtype.name == "B")
        ));
    }

    #[test]
    fn projected_surface_group_recomputes_labels_owner_dependencies() {
        let mut module = parse(
            "module x; rec { \
               labels A = { to_b: B }; \
               newtype B : A { constructor mk_b; projector un_b; }; \
             }",
        )
        .expect("parse");
        let Item::TypeRecGroup(group) = &mut module.items[0] else {
            panic!("expected recursive type group")
        };
        group.members.retain(
            |member| !matches!(member, TypeRecMember::Newtype(newtype) if newtype.name == "B"),
        );
        let analyses = analyze_surface_type_rec_groups(&module).expect("projection analysis");
        let [(_, analysis)] = analyses.as_slice() else {
            panic!("expected exactly one recursive type group analysis")
        };
        let analysis = analysis
            .as_ref()
            .expect("one labels owner remains representable");
        assert_eq!(analysis.components, vec![vec![0]]);
        assert!(analysis.cyclic_components.is_empty());
    }

    #[test]
    fn projected_surface_group_reports_indivisible_labels_owner_overlap() {
        let mut module = parse(
            "module x; rec { \
               labels { a: B & X, c: D & X }; \
               labels { b: A, d: C }; \
               newtype X : A & C { constructor mk_x; projector un_x; }; \
             }",
        )
        .expect("parse");
        // The original expanded declarations form one SCC, so this is a
        // valid authored group before dependency retyping removes `X`.
        let desugared = desugar_module(module.clone()).expect("desugar");
        elaborate_package(vec![(PathBuf::from("test.kio"), desugared)], None)
            .expect("original group elaborates");

        let Item::TypeRecGroup(group) = &mut module.items[0] else {
            panic!("expected recursive type group")
        };
        group.members.retain(
            |member| !matches!(member, TypeRecMember::Newtype(newtype) if newtype.name == "X"),
        );
        let analyses = analyze_surface_type_rec_groups(&module).expect("projection analysis");
        let [(_, analysis)] = analyses.as_slice() else {
            panic!("expected exactly one recursive type group analysis")
        };
        assert!(
            analysis.is_none(),
            "two independent expanded SCCs crossing the same two atomic labels declarations cannot be represented by one exact source group"
        );
    }

    #[test]
    fn mutually_recursive_named_labels_form_one_expanded_nominal_component() {
        let module = elab(
            "module x; rec { \
               labels A = { to_b: B }; \
               labels B = { to_a: A }; \
             }",
        );
        let Item::TypeRecGroup(group) = &module.items[0] else {
            panic!("the mutually recursive named labels must remain one Kio' group");
        };
        for expected in ["A", "To_b", "B", "To_a"] {
            assert!(
                group
                    .members
                    .iter()
                    .any(|member| lowered_type_member_name(member) == expected),
                "expanded mutual labels omitted {expected}: {:?}",
                group.members
            );
        }
        crate::pass::resolve::Resolver::check_module(&module)
            .expect("the mutually recursive label aliases must independently resolve");
    }

    #[test]
    fn expanded_multi_component_group_labels_every_source_declaration_head() {
        let source = concat!(
            "module x; rec { ",
            "labels Pair = { first: . | First, second: . | Second }; ",
            "newtype Other : . | Other { constructor mk; projector un; }; ",
            "}",
        );
        let error = elab_error(source);
        let Error::Parse(diagnostic) = error else {
            panic!("expected Parse, got {error:?}");
        };
        assert_eq!(
            diagnostic.message,
            "this `rec` group contains multiple independent components"
        );
        let labels = diagnostic.secondary();
        assert_eq!(labels.len(), 3, "one label per expanded recursive SCC");
        let pair_start = source.find("Pair").expect("Pair head") as u32;
        let other_start = source.find("Other").expect("Other head") as u32;
        assert_eq!(labels[0].span, Span::new(pair_start, pair_start + 4));
        assert_eq!(labels[1].span, Span::new(pair_start, pair_start + 4));
        assert_eq!(labels[2].span, Span::new(other_start, other_start + 5));
        assert!(labels.iter().any(|label| label.text.contains("`First`")));
        assert!(labels.iter().any(|label| label.text.contains("`Second`")));
        assert!(labels.iter().any(|label| label.text.contains("`Other`")));
    }

    #[test]
    fn unrelated_labels_member_points_to_its_written_alias_head() {
        let source = concat!(
            "module x; rec { ",
            "labels A = { to_b: B }; ",
            "newtype B : A { constructor mk; projector un; }; ",
            "labels Spare = { unused: . }; ",
            "}",
        );
        let error = elab_error(source);
        let Error::Parse(diagnostic) = error else {
            panic!("expected Parse, got {error:?}");
        };
        let spare_start = source.find("Spare").expect("Spare head") as u32;
        assert_eq!(diagnostic.secondary().len(), 1);
        assert_eq!(
            diagnostic.secondary()[0].span,
            Span::new(spare_start, spare_start + 5)
        );
        assert_eq!(
            diagnostic.secondary()[0].text,
            "`Spare` does not participate in the mutual component"
        );
    }

    #[test]
    fn alias_only_cycle_labels_exact_alias_heads_after_label_elaboration() {
        let source = concat!(
            "module x; rec { ",
            "type Alias_a = Alias_b; ",
            "type Alias_b = Alias_a; ",
            "newtype Nominal : Nominal { constructor mk; projector un; }; ",
            "}",
        );
        let error = elab_error(source);
        let Error::Totality(diagnostic) = error else {
            panic!("expected Totality, got {error:?}");
        };
        let a_start = source.find("Alias_a").expect("Alias_a head") as u32;
        let b_start = source.find("Alias_b =").expect("Alias_b head") as u32;
        assert_eq!(diagnostic.secondary().len(), 2);
        assert_eq!(
            diagnostic.secondary()[0].span,
            Span::new(a_start, a_start + 7)
        );
        assert_eq!(
            diagnostic.secondary()[1].span,
            Span::new(b_start, b_start + 7)
        );
    }

    #[test]
    fn recursive_component_order_is_iterative_for_a_large_dependency_chain() {
        const NODES: usize = 50_000;
        let edges = (0..NODES)
            .map(|index| {
                (index + 1 < NODES)
                    .then_some(vec![index + 1])
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>();
        let analysis = crate::pass::resolve::TypeRecAnalysis {
            edge_spans: edges
                .iter()
                .map(|targets| {
                    targets
                        .iter()
                        .map(|target| (*target, Span::new(0, 0)))
                        .collect()
                })
                .collect(),
            edges,
            components: (0..NODES).map(|index| vec![index]).collect(),
            cyclic_components: Vec::new(),
            alias_cycle: None,
        };

        let order = crate::pass::resolve::type_rec_component_order(&analysis);
        assert_eq!(order.len(), NODES);
        assert_eq!(order.first(), Some(&(NODES - 1)));
        assert_eq!(order.last(), Some(&0));
    }

    #[test]
    fn nested_singleton_markers_in_a_mutual_group_have_remove_only_fixes() {
        for source in [
            "module x; rec { \
               rec newtype A : B { constructor mk_a; projector un_a; }; \
               newtype B : A { constructor mk_b; projector un_b; }; \
             }",
            "module x; rec { \
               rec labels A = { to_b: B }; \
               newtype B : A { constructor mk_b; projector un_b; }; \
             }",
        ] {
            let error = elab_error(source);
            let Error::Parse(diagnostic) = error else {
                panic!("expected Parse, got {error:?}");
            };
            assert_eq!(
                diagnostic.message,
                "the enclosing `rec { ... }` already supplies recursive scope"
            );
            assert_eq!(diagnostic.fixes().len(), 1);
            assert_eq!(diagnostic.fixes()[0].title, "Fix recursive type groups");
            assert_eq!(diagnostic.fixes()[0].edits.len(), 1);
            assert_eq!(diagnostic.fixes()[0].edits[0].replacement, "");
        }
    }

    #[test]
    fn unmarked_recursive_named_labels_defers_the_marker_until_semantic_validation() {
        let module = elab(
            "module x; labels Tree = \
             { leaf: . } | { branch: Tree & Tree };",
        );
        let [Item::TypeRecGroup(group)] = module.items.as_slice() else {
            panic!(
                "expected one deferred generated type group, got {:?}",
                module.items
            );
        };
        let deferred = group
            .deferred_rec_labels_diagnostic
            .as_ref()
            .expect("the shared checker receives the missing-marker diagnostic");
        assert!(deferred.has_named_alias);
        assert_eq!(group.members.len(), 3);
    }

    #[test]
    fn invalid_unmarked_recursive_labels_also_reach_semantic_validation() {
        let module = elab("module x; labels Loop = { step: Loop -> . };");
        let [Item::TypeRecGroup(group)] = module.items.as_slice() else {
            panic!(
                "expected one deferred generated type group, got {:?}",
                module.items
            );
        };
        assert!(
            group.deferred_rec_labels_diagnostic.is_some(),
            "the shared signature checker must decide between the underlying defect and the marker"
        );
    }

    #[test]
    fn acyclic_rec_labels_reports_the_redundant_marker_and_fix() {
        let error = elab_error("module x; rec labels Pair = { first: ., second: . };");
        let Error::Parse(diagnostic) = error else {
            panic!("expected Parse, got {error:?}");
        };
        assert_eq!(diagnostic.message, "this `rec` marker is unnecessary");
        assert_eq!(diagnostic.fixes()[0].title, "Remove unnecessary `rec`");
    }

    #[test]
    fn scoped_label_is_hidden_from_outside_selective_use() {
        let error = elab_modules(&[
            (
                "scope/origin.kio",
                "module scope/origin; pub(scope) labels { item: . };",
            ),
            (
                "outside.kio",
                "module outside; import scope/origin({item}); fn f() -> . { {item=} }",
            ),
        ])
        .expect_err("the sealed label must not enter an outside selective scope");
        let (_, message) = error.error.diag();
        assert!(
            message.contains("restricted to `pub(scope)`"),
            "got: {message}"
        );
    }

    #[test]
    fn scoped_label_is_hidden_from_outside_qualified_use() {
        let error = elab_modules(&[
            (
                "scope/origin.kio",
                "module scope/origin; pub(scope) labels { item: . };",
            ),
            (
                "outside.kio",
                "module outside; import scope/origin as o; fn f() -> . { {o.item=} }",
            ),
        ])
        .expect_err("a qualified module alias must not expose its sealed labels");
        let (_, message) = error.error.diag();
        assert!(
            message.contains("restricted to `pub(scope)`"),
            "got: {message}"
        );
    }

    #[test]
    fn scoped_label_is_visible_inside_its_module_subtree() {
        elab_modules(&[
            (
                "scope/origin.kio",
                "module scope/origin; pub(scope) labels { item: . };",
            ),
            (
                "scope/inside.kio",
                "module scope/inside; import scope/origin as o; fn f() -> . { {o.item=} }",
            ),
        ])
        .expect("a module in the declared subtree can use the scoped label");
    }

    #[test]
    fn selective_label_import_erases_to_a_qualified_binding() {
        let modules = elab_modules(&[
            ("origin.kio", "module origin; pub labels { item: . };"),
            (
                "consumer.kio",
                "module consumer; import origin({item}); fn f() -> . { {item=} }",
            ),
        ])
        .expect("explicit label import lowers");
        let consumer = modules
            .iter()
            .find(|(_, module)| module.path.segments == ["consumer"])
            .map(|(_, module)| module)
            .expect("consumer module");
        assert_eq!(consumer.imports.len(), 1);
        assert!(matches!(
            &consumer.imports[0].kind,
            ImportKind::Qualified { path, alias }
                if path.segments == ["origin"] && alias.starts_with("_label_")
        ));
    }

    #[cfg(feature = "prime")]
    #[test]
    fn selective_label_import_alias_skips_an_occupied_first_candidate() {
        const OCCUPIED: &str = "_label_gphcgjghgjgo__";
        const GENERATED: &str = "_label_gphcgjghgjgo_n2__";

        let modules = elab_modules(&[
            ("origin.kio", "module origin; pub labels { item: . };"),
            ("decoy.kio", "module decoy;"),
            (
                "consumer.kio",
                "module consumer; \
                 import decoy as _label_gphcgjghgjgo__; \
                 import origin as o; \
                 import origin({item}); \
                 fn f() -> o.Item { {item=} }",
            ),
        ])
        .expect("the label alias allocator skips an occupied candidate");
        let consumer = modules
            .iter()
            .find(|(_, module)| module.path.segments == ["consumer"])
            .map(|(_, module)| module)
            .expect("consumer module");

        assert!(consumer.imports.iter().any(|import_| matches!(
            &import_.kind,
            ImportKind::Qualified { path, alias }
                if path.segments == ["decoy"] && alias == OCCUPIED
        )));
        let generated_alias = consumer
            .imports
            .iter()
            .find_map(|import_| match &import_.kind {
                ImportKind::Qualified { path, alias }
                    if path.segments == ["origin"] && alias == GENERATED =>
                {
                    Some(alias.as_str())
                }
                _ => None,
            })
            .expect("generated qualified import for the selected label");
        assert_eq!(generated_alias, GENERATED);

        let function = consumer
            .items
            .iter()
            .find_map(|item| match item {
                Item::FnDef(function) if function.name == "f" => Some(function),
                _ => None,
            })
            .expect("consumer function");
        let Expr::Elaborator {
            call: ElaboratorCall::FieldUpdate { updates, .. },
            ..
        } = &function.body
        else {
            panic!("label construction must lower to the field-update elaborator")
        };
        let label_path = updates[0]
            .label_type
            .as_deref()
            .expect("selected label carries its generated nominal path")
            .iter()
            .map(PathSegment::as_str)
            .collect::<Vec<_>>();
        assert_eq!(label_path, [generated_alias, "Item"]);

        use std::path::Path;

        use crate::backends::kio_prime::emit_module;
        use crate::pass::resolve::Package;
        use crate::pass::typecheck_full::check_package;
        use crate::pipeline::Pipeline;
        use crate::prime::pipeline::PrimePipeline;

        let package = Package::build(Path::new(""), modules.clone(), None)
            .expect("assemble label-collision package");
        package
            .resolve_imports()
            .expect("resolve label-collision package");
        package
            .check_in_body_resolution()
            .expect("resolve label-collision bodies");
        let prime = check_package(&package).expect("typecheck label-collision package");
        let mut emitted_consumer = None;
        let reparsed = prime
            .modules()
            .map(|(module_path, entry)| {
                let source = emit_module(&entry.module);
                if module_path == "consumer" {
                    emitted_consumer = Some(source.clone());
                }
                (
                    PathBuf::from(format!("{module_path}.kio")),
                    parse(&source).unwrap_or_else(|error| {
                        panic!("fresh Kio' parse of `{module_path}` failed: {error:?}\n{source}")
                    }),
                )
            })
            .collect();
        let emitted_consumer = emitted_consumer.expect("emitted consumer module");
        assert!(
            emitted_consumer.contains(&format!("import origin as {GENERATED};")),
            "emitted consumer must retain the collision-allocated import:\n{emitted_consumer}"
        );
        assert!(
            emitted_consumer.contains(&format!("{GENERATED}.Item")),
            "emitted label reference must use the allocated import:\n{emitted_consumer}"
        );

        let (fresh_modules, _) = PrimePipeline::lower_package(reparsed, None)
            .expect("emitted label package remains Kio'-shaped");
        let fresh = Package::build(Path::new(""), fresh_modules, None)
            .expect("assemble freshly parsed label package");
        fresh
            .resolve_imports()
            .expect("fresh label package resolves");
        fresh
            .check_binding_origins()
            .expect("fresh generated import and reference retain one binding origin");
        fresh
            .check_no_value_cycles()
            .expect("fresh label package has no value cycle");
        fresh
            .check_in_body_resolution()
            .expect("fresh label package resolves bodies");
        PrimePipeline::typecheck(&fresh)
            .expect("standalone Prime validation accepts the generated label import");
    }

    #[test]
    fn bare_selective_item_does_not_import_label_syntax() {
        let error = elab_modules(&[
            ("origin.kio", "module origin; pub labels { item: . };"),
            (
                "consumer.kio",
                "module consumer; import origin(item); fn f() -> . { {item=} }",
            ),
        ])
        .expect_err("bare ordinary import must not widen into the label namespace");
        let (_, message) = error.error.diag();
        assert!(
            message.contains("label `item` is not in scope"),
            "got: {message}"
        );
    }

    #[test]
    fn selective_label_and_unrelated_nominal_import_do_not_collide() {
        let modules = elab_modules(&[
            ("labels.kio", "module labels; pub labels { item: . };") ,
            ("types.kio", "module types; pub type Item = .;") ,
            (
                "consumer.kio",
                "module consumer; import labels({item}); import types(Item); fn f() -> . { {item=} }",
            ),
        ])
        .expect("explicit namespaces keep the two Item identities separate");
        let consumer = modules
            .iter()
            .find(|(_, module)| module.path.segments == ["consumer"])
            .map(|(_, module)| module)
            .expect("consumer module");
        assert!(consumer.imports.iter().any(|import_| matches!(
            &import_.kind,
            ImportKind::Selective { items, from }
                if from.segments == ["types"]
                    && matches!(items.as_slice(), [ImportItem::Name { name, .. }] if name == "Item")
        )));
        assert!(consumer.imports.iter().any(|import_| matches!(
            &import_.kind,
            ImportKind::Qualified { path, alias }
                if path.segments == ["labels"] && alias.starts_with("_label_")
        )));
    }

    #[test]
    fn labels_named_form_emits_type_alias() {
        let m = elab("module x; labels T = { foo : . };");
        assert!(matches!(m.items[0], Item::Newtype(_)));
        let alias = m
            .items
            .iter()
            .find_map(|i| match i {
                Item::TypeAlias(t) => Some(t),
                _ => None,
            })
            .expect("type alias from named form");
        assert_eq!(alias.name, "T");
    }

    #[test]
    fn labels_pub_propagates_to_generated_newtype() {
        let m = elab("module x; pub labels { foo : . };");
        if let Item::Newtype(d) = &m.items[0] {
            assert!(d.vis.is_pub());
            assert!(d.constructor.vis.is_pub());
            assert!(d.projector.vis.is_pub());
        }
    }

    #[test]
    fn explicit_reuse_marker_emits_one_newtype() {
        let m = elab("module x; labels { foo : I32 }; labels T = { foo : _ };");
        let foo_newtypes = m
            .items
            .iter()
            .filter(|i| matches!(i, Item::Newtype(d) if d.name == "Foo"))
            .count();
        assert_eq!(
            foo_newtypes, 1,
            "a reuse marker must not mint another generated newtype"
        );
    }

    #[test]
    fn named_sum_reuses_an_explicit_label_from_an_earlier_arm() {
        let m = elab("module x; labels Foo = { bar : I32, baz : . } | { bar : _, foobar : . };");
        assert_eq!(
            m.items
                .iter()
                .filter(|item| matches!(item, Item::Newtype(newtype) if newtype.name == "Bar"))
                .count(),
            1
        );
        assert!(
            m.items
                .iter()
                .any(|item| matches!(item, Item::TypeAlias(alias) if alias.name == "Foo"))
        );
    }

    #[test]
    fn reuse_does_not_revise_original_visibility() {
        let m = elab("module x; labels { foo : . }; pub labels Public_row = { foo : _ };");
        let foo = m.items.iter().find_map(|item| match item {
            Item::Newtype(newtype) if newtype.name == "Foo" => Some(newtype),
            _ => None,
        });
        let public_row = m.items.iter().find_map(|item| match item {
            Item::TypeAlias(alias) if alias.name == "Public_row" => Some(alias),
            _ => None,
        });

        assert!(matches!(foo.expect("Foo").vis, Visibility::Private));
        assert!(matches!(
            public_row.expect("Public_row").vis,
            Visibility::Public
        ));
    }

    #[test]
    fn repeated_explicit_label_is_rejected_with_safe_replacement() {
        let error =
            elab_error("module x; labels { foo : I32 }; labels T = { foo : I32, other : . };");
        let diagnostic = error.diagnostic();
        assert!(diagnostic.message.contains("already explicitly declared"));
        assert_eq!(diagnostic.secondary().len(), 1);
        assert_eq!(diagnostic.fixes().len(), 1);
        assert_eq!(diagnostic.fixes()[0].edits[0].replacement, "_");
    }

    #[test]
    fn repeated_explicit_label_is_rejected_even_when_payload_differs() {
        let error = elab_error("module x; labels { foo : I32 }; labels T = { foo : String };");
        let diagnostic = error.diagnostic();
        assert!(diagnostic.message.contains("already explicitly declared"));
        assert!(
            diagnostic
                .help()
                .is_some_and(|help| help.contains("payload differs"))
        );
        assert!(diagnostic.fixes().is_empty());
    }

    #[test]
    fn repeated_explicit_generic_label_explains_alias_binder_requirement() {
        let error = elab_error(
            "module x; labels { value[A] : A }; labels Choice[B] = { value[A] : A, other : . };",
        );
        let diagnostic = error.diagnostic();
        assert!(
            diagnostic
                .help()
                .is_some_and(|help| help.contains("parameters of the enclosing alias"))
        );
        assert!(diagnostic.fixes().is_empty());
    }

    #[test]
    fn repeated_explicit_label_has_no_scope_blind_alias_binder_fix() {
        let error = elab_error(
            "module x; type A = .; labels { f[A] : A }; labels Row[A][B] = { f[B] : A };",
        );
        let diagnostic = error.diagnostic();
        assert!(
            diagnostic
                .help()
                .is_some_and(|help| help.contains("could change which type a spelling names"))
        );
        assert!(diagnostic.fixes().is_empty());
    }

    #[test]
    fn repeated_explicit_label_has_no_scope_blind_existential_fix() {
        let error =
            elab_error("module x; type A = .; labels { f <A> : A }; labels Row = { f : A };");
        let diagnostic = error.diagnostic();
        assert!(
            diagnostic
                .help()
                .is_some_and(|help| help.contains("introduces existential binders"))
        );
        assert!(diagnostic.fixes().is_empty());
    }

    #[test]
    fn reuse_requires_earlier_local_explicit_declaration() {
        let msg = elab_err("module x; labels First = { foo : _ }; labels { foo : . };");
        assert!(
            msg.contains("no earlier explicit declaration"),
            "got: {msg}"
        );
    }

    #[test]
    fn reuse_rejects_anonymous_declaration() {
        let msg = elab_err("module x; labels { foo : . }; labels { foo : _ };");
        assert!(msg.contains("anonymous `labels`"), "got: {msg}");
    }

    #[test]
    fn reuse_header_matches_kinds_but_not_binder_names() {
        let m = elab(
            "module x; labels { value[*F][A] : F(A) }; labels Choice[*G][B] = { value[*G][B] : _, other : . };",
        );
        assert!(
            m.items
                .iter()
                .any(|item| matches!(item, Item::TypeAlias(alias) if alias.name == "Choice"))
        );
    }

    #[test]
    fn reuse_header_rejects_arity_and_kind_mismatch() {
        let arity = elab_err(
            "module x; labels { value[A] : A }; labels Choice = { value : _, other : . };",
        );
        assert!(
            arity.contains("incompatible universal binder header"),
            "got: {arity}"
        );
        let kind = elab_err(
            "module x; labels { value[*F] : F(.) }; labels Choice[G] = { value[G] : _, other : . };",
        );
        assert!(
            kind.contains("incompatible universal binder header"),
            "got: {kind}"
        );
    }

    #[test]
    fn reuse_marker_rejects_existential_binders() {
        let msg = elab_err(
            "module x; labels { value <A> : A }; labels Choice = { value <B> : _, other : . };",
        );
        assert!(msg.contains("cannot declare existential"), "got: {msg}");
    }

    #[test]
    fn nested_infer_is_an_explicit_payload_not_a_reuse_marker() {
        let m = elab("module x; labels { value[*F] : F(_) };");
        let value = m.items.iter().find_map(|item| match item {
            Item::Newtype(newtype) if newtype.name == "Value" => Some(newtype),
            _ => None,
        });
        assert!(
            value.is_some(),
            "nested `_` must not suppress the declaration"
        );
    }

    #[test]
    fn reuse_header_names_enclosing_alias_parameters() {
        let msg = elab_err(
            "module x; labels { value[A] : A }; labels Choice[B] = { value[A] : _, other : . };",
        );
        assert!(msg.contains("enclosing alias"), "got: {msg}");
    }

    #[test]
    fn generated_type_name_is_type_position_spelling() {
        let m = elab(
            "module x; \
             labels { foo : . }; \
             type T = Foo;",
        );
        let alias = m
            .items
            .iter()
            .find_map(|i| match i {
                Item::TypeAlias(t) if t.name == "T" => Some(t),
                _ => None,
            })
            .expect("type T");
        if let Type::Path { segments, .. } = alias.type_body() {
            assert_eq!(segments.len(), 1);
            assert_eq!(segments[0], "Foo");
        }
    }

    #[test]
    fn lowercase_label_in_type_position_is_parse_error() {
        let err = parse(
            "module x; \
             labels { foo : . }; \
             type T = foo;",
        )
        .expect_err("lowercase label spelling is not type syntax");
        let msg = err.diag().1.to_owned();
        assert_eq!(msg, "type name `foo` must have an uppercase first letter");
    }

    #[test]
    fn label_value_construction_rewrites() {
        let m = elab(
            "module x; \
             labels { foo : . }; \
             fn f() -> Foo { {foo = ()} }",
        );
        // Construction shares the private field-update surface node
        // until typecheck records the final constructor elaboration.
        let user_fn_def = m
            .items
            .iter()
            .find_map(|i| match i {
                Item::FnDef(d) if d.name == "f" => Some(d),
                _ => None,
            })
            .expect("user-written `fn f`");
        let Expr::Elaborator {
            kind: ElaboratorKind::Filtered,
            call: ElaboratorCall::FieldUpdate { receiver, updates },
            ..
        } = &user_fn_def.body
        else {
            panic!(
                "expected field-update elaborator, got {:?}",
                user_fn_def.body
            );
        };
        assert!(matches!(receiver.as_ref(), Expr::Unit { .. }));
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].label, "foo");
        // A local label keeps the ordinary same-module source spelling;
        // the typer carries its canonical identity separately.
        let label_path: Vec<&str> = updates[0]
            .label_type
            .as_deref()
            .expect("label elaboration populated label_type")
            .iter()
            .map(crate::ast::PathSegment::as_str)
            .collect();
        assert_eq!(label_path, vec!["Foo"]);
        assert!(matches!(updates[0].value, Expr::Unit { .. }));
    }

    #[test]
    fn unknown_label_in_value_sugar_errors() {
        let msg = elab_err(
            "module x; \
             fn f() -> . { {foo = ()} }",
        );
        assert!(msg.contains("`foo`"), "got: {msg}");
    }

    #[test]
    fn rec_label_reference_rewrites() {
        // `list(A)` (which parses as `Path { list, [A] }`) is
        // rewritten by label-elab to a reference to the minted newtype
        // name `List(A)`.
        let m = elab("module x; rec labels { list[A] : (. | (A & List(A))) };");
        if let Item::Newtype(d) = &m.items[0] {
            assert_eq!(d.name, "List");
            let self_ref =
                find_self_reference(&d.payload, &d.name).expect("self-reference inside payload");
            assert_eq!(self_ref, d.name);
        }
    }

    fn find_self_reference(t: &Type<Lowered>, newtype_name: &str) -> Option<String> {
        match t {
            Type::Path { segments, .. } if segments.len() == 1 && segments[0] == newtype_name => {
                Some(segments[0].name.clone())
            }
            Type::Sum { left, right, .. } | Type::Product { left, right, .. } => {
                find_self_reference(left, newtype_name)
                    .or_else(|| find_self_reference(right, newtype_name))
            }
            Type::Function { param, ret, .. } => find_self_reference(param, newtype_name)
                .or_else(|| find_self_reference(ret, newtype_name)),
            _ => None,
        }
    }

    /// The head name of a single-segment `Type::Path`, for asserting
    /// whether a type-position reference was rewritten to the
    /// generated newtype or left as the shadowing type-param.
    fn type_path_head(t: &Type<Lowered>) -> &str {
        match t {
            Type::Path { segments, .. } if segments.len() == 1 => segments[0].as_str(),
            other => panic!("expected single-segment Type::Path, got {other:?}"),
        }
    }

    #[test]
    fn type_param_shadows_label_in_fn_signature() {
        // `A` is a label (generated newtype `A`); the `fn`'s `[A]`
        // binder shadows it in the param and return types.
        let m = elab("module x; labels { a : . }; fn f[A](x: A) -> A { x }");
        let f = m
            .items
            .iter()
            .find_map(|i| match i {
                Item::FnDef(d) if d.name == "f" => Some(d),
                _ => None,
            })
            .expect("fn f");
        assert_eq!(type_path_head(&f.ret), "A", "return type not shadowed");
        let param_ty = f
            .sig
            .value_params()
            .next()
            .expect("value param")
            .ty
            .as_ref()
            .expect("annotated");
        assert_eq!(type_path_head(param_ty), "A", "param type not shadowed");
    }

    #[test]
    fn type_param_shadows_label_in_alias_header() {
        let m = elab("module x; labels { a : . }; type T[A] = A;");
        let alias = m
            .items
            .iter()
            .find_map(|i| match i {
                Item::TypeAlias(t) if t.name == "T" => Some(t),
                _ => None,
            })
            .expect("type T");
        assert_eq!(type_path_head(alias.type_body()), "A");
    }

    #[test]
    fn type_param_shadows_label_in_newtype_header() {
        let m = elab(
            "module x; labels { a : . }; \
             newtype N[A] : A { constructor mk_n; projector un_n; };",
        );
        let nt = m
            .items
            .iter()
            .find_map(|i| match i {
                Item::Newtype(d) if d.name == "N" => Some(d),
                _ => None,
            })
            .expect("newtype N");
        assert_eq!(type_path_head(&nt.payload), "A");
    }

    #[test]
    fn type_param_shadows_label_in_forall() {
        // A `Type::Forall` mid-type binder `[A] A -> A` in a
        // parameter annotation shadows the label `a` in its body.
        let m = elab(
            "module x; labels { a : . }; \
             fn g(h: [A] A -> A) -> . { () }",
        );
        let g = m
            .items
            .iter()
            .find_map(|i| match i {
                Item::FnDef(d) if d.name == "g" => Some(d),
                _ => None,
            })
            .expect("fn g");
        let h_ty = g
            .sig
            .value_params()
            .next()
            .expect("value param")
            .ty
            .as_ref()
            .expect("annotated");
        let Type::Forall { body, .. } = h_ty else {
            panic!("expected Type::Forall, got {h_ty:?}");
        };
        let Type::Function { param, ret, .. } = body.as_ref() else {
            panic!("expected Type::Function body, got {body:?}");
        };
        assert_eq!(type_path_head(param), "A", "forall param not shadowed");
        assert_eq!(type_path_head(ret), "A", "forall ret not shadowed");
    }

    #[test]
    fn type_param_shadows_label_in_fn_expr_signature() {
        let m = elab(
            "module x; labels { a : . }; \
             fn g() -> . { let f = .[A](x: A) -> A { x }; () }",
        );
        let g = m
            .items
            .iter()
            .find_map(|i| match i {
                Item::FnDef(d) if d.name == "g" => Some(d),
                _ => None,
            })
            .expect("fn g");
        let Expr::Let { value, .. } = &g.body else {
            panic!("expected Let, got {:?}", g.body);
        };
        let Expr::FnExpr { sig, ret_ty, .. } = value.as_ref() else {
            panic!("expected FnExpr, got {value:?}");
        };
        let param_ty = sig
            .value_params()
            .next()
            .expect("value param")
            .ty
            .as_ref()
            .expect("annotated");
        assert_eq!(type_path_head(param_ty), "A", "lambda param not shadowed");
        let ret_ty = ret_ty.as_ref().expect("return annotation");
        assert_eq!(type_path_head(ret_ty), "A", "lambda ret not shadowed");
    }

    #[test]
    fn ordinary_value_name_can_match_label() {
        let m = elab(
            "module x; \
             labels { foo : . }; \
             fn foo() -> . { () }",
        );
        assert!(
            m.items
                .iter()
                .any(|item| matches!(item, Item::FnDef(d) if d.name == "foo"))
        );
    }
}
