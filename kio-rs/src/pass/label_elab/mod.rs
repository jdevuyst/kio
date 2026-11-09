//! Label elaboration.
//!
//! Runs after `pass/desugar` and before `Package::build`. Strips the
//! label-related surface forms, which need a label table
//! (a per-module name -> label-info index) that name resolution doesn't
//! itself produce:
//!
//! - [`Item::Labels`] -> N [`Item::Newtype`] entries (one per nominal declaration),
//!   plus one [`Item::TypeAlias`] for the named form.
//!
//! This pass is also the [`Desugared`] → [`Lowered`] phase
//! boundary: `ExprTuple` and `ItemLabels` collapse to `Never` in
//! `Lowered`. `Tuple` was stripped by `desugar` already (its
//! Desugared-phase ext is also `Never`, so the match-arm here
//! discharges via `match ext {}`); the item is stripped by
//! the lowering rules below.
//!
//! Compact entries retain their exact ordinary nominal names. Named owners
//! compose those identities with ordinary type leaves; declaration conflicts
//! are checked against the module's written declarations before lowering.
//!
//! The Desugared → Lowered walk is shared with any other future pass
//! at the same boundary via the [`DesugaredToLowered`] trait in
//! [`crate::pass::label_elab::walk`]: [`LabelElabVisitor`] implements
//! a rewrite hook for [`Item::Labels`]. Every
//! other AST variant clones-and-recurses through the trait's defaults.

pub mod walk;

use std::path::PathBuf;

use crate::ast::{
    Desugared, Item, LabelEntry, Labels, Lowered, Meta, Module, ModulePath, PackageFile, Signature,
    Type, TypeAlias, TypeRecGroup, TypeRecMember, mint_label_newtype_name,
};
use crate::error::Error;
use crate::pass::label_elab::walk::DesugaredToLowered;
use crate::pass::nominal_declarations::{
    label_nominal_declaration, label_source_owner_span, named_nominal_alias_body,
    normalize_compact_declaration, normalize_recursive_group,
};
use crate::pass::resolve::LocatedError;
use crate::span::Span;
#[cfg(feature = "parallel")]
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};

/// A compact declaration’s nominal identity.
#[derive(Debug, Clone)]
struct LabelInfo {
    /// The exact written nominal name selected by the compact declaration.
    newtype_name: String,
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
/// generated `Item::Newtype` chains and any named alias.
pub fn elaborate_package(
    parsed: Vec<(PathBuf, Module<Desugared>)>,
    package_file: Option<PackageFile<Desugared>>,
) -> Result<ElaboratedPackage, LocatedError> {
    let package_tables = PackageLabelTables::build(&parsed)?;

    // Pass 2: lower each module against its local declaration table.
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
    /// (`pkg/helper`). The summary supplies each module walk with its
    /// local compact declarations without re-scanning the package.
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
        Ok(Self { local })
    }

    /// Elaborate one module against the package label summary.
    pub fn elaborate_module(&self, module: Module<Desugared>) -> Result<Module<Lowered>, Error> {
        let table = self
            .local
            .get(&module_path_to_string(&module.path))
            .expect("each elaborated module has a completed package label table");
        elaborate_module(module, table)
    }
}

/// Elaborate a package file. Package files have no `labels` of their
/// own; exported fn bodies currently lower against an empty
/// label table, matching [`elaborate_package`].
pub fn elaborate_package_file(
    export: PackageFile<Desugared>,
) -> Result<PackageFile<Lowered>, Error> {
    let empty = LabelTable::new();
    LabelElabVisitor { table: &empty }.walk_package_file(export)
}

/// Contract projections have already removed labels and their imports.
pub(crate) fn lower_contract_signature(
    sig: Signature<Desugared>,
    ret: Type<Desugared>,
) -> Result<(Signature<Lowered>, Type<Lowered>), Error> {
    let empty = LabelTable::new();
    LabelElabVisitor { table: &empty }.walk_function_header(sig, ret)
}

pub(crate) fn lower_contract_declaration(
    declaration: Item<Desugared>,
) -> Result<Vec<Item<Lowered>>, Error> {
    let empty = LabelTable::new();
    LabelElabVisitor { table: &empty }.walk_item(declaration)
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

/// Collect compact nominal declarations and check their module-local conflicts.
/// Ordinary RHS type references do not introduce declarations into this table.
fn build_label_table(module: &Module<Desugared>) -> Result<LabelTable, Error> {
    let mut table = LabelTable::new();
    let mut declarations: HashMap<String, Span> = HashMap::new();
    let reserved = collect_module_type_names(module);
    for item in &module.items {
        match item {
            Item::Labels(labels, _) => {
                add_labels_to_table(labels, &reserved, &mut declarations, &mut table)?;
            }
            Item::TypeRecGroup(group) => {
                for member in &group.members {
                    if let TypeRecMember::Labels(labels, _) = member {
                        add_labels_to_table(labels, &reserved, &mut declarations, &mut table)?;
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
    reserved: &HashSet<String>,
    declarations: &mut HashMap<String, Span>,
    table: &mut LabelTable,
) -> Result<(), Error> {
    for entry in label_entries_in_source_order(labels)? {
        if let Some(&first_span) = declarations.get(entry.name.as_str()) {
            return Err(Error::name_res(
                entry.name_span,
                format!("nominal `{}` is already declared in this module", entry.name),
            )
            .with_secondary(first_span, format!("`{}` was first declared here", entry.name))
            .with_help(
                "use an ordinary type reference outside the declaration group to refer to the existing nominal, or rename this declaration",
            ));
        }

        let newtype_name = mint_label_newtype_name(&entry.name);
        if reserved.contains(&newtype_name) {
            return Err(Error::name_res(
                entry.name_span,
                format!(
                    "nominal `{newtype_name}` collides with an existing top-level declaration of the same name"
                ),
            )
            .with_help(format!(
                "rename one of the colliding `{newtype_name}` declarations"
            )));
        }
        declarations.insert(entry.name.clone(), entry.name_span);
        table.insert(entry.name.clone(), LabelInfo { newtype_name });
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

/// Gather ordinary type and named compact-owner declarations for exact-name
/// conflict checking. Compact entries are checked in source order separately.
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

            // Macro decls are compile-time metadata; no type name.
            Item::Macro(_, _) => {}
            // Statically uninhabited at `Desugared`.
            Item::LiteralAlias(_, ext) => match *ext {},
            Item::Op(_, ext) => match *ext {},
            Item::VariadicOperator(_, ext) => match *ext {},
            Item::RecGroup(_, ext) => match *ext {},
        }
    }
    names
}

/// Canonical module-map key: segments `/`-joined.
fn module_path_to_string(p: &ModulePath) -> String {
    p.segments.join("/")
}

/// Expand compact declarations.
fn elaborate_module(
    module: Module<Desugared>,
    table: &LabelTable,
) -> Result<Module<Lowered>, Error> {
    LabelElabVisitor { table }.walk_module(module)
}

// ---- Visitor ------------------------------------------------------

/// Per-module elaboration visitor.
///
/// Holds the local declaration table for the module under elaboration.
/// Implements [`DesugaredToLowered`] with the required type and item
/// rewrite hooks. Every other AST variant clones-and-recurses
/// through the trait's defaults.
struct LabelElabVisitor<'a> {
    table: &'a LabelTable,
}

fn lowered_type_member(item: Item<Lowered>) -> TypeRecMember<Lowered> {
    match item {
        Item::TypeAlias(alias) => TypeRecMember::TypeAlias(alias),
        Item::Newtype(newtype) => TypeRecMember::Newtype(newtype),
        _ => unreachable!("label elaboration produces only aliases and newtypes"),
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
    let mut analyses = Vec::new();
    for item in desugared.items {
        let Item::TypeRecGroup(group) = item else {
            continue;
        };
        let group_span = group.meta.span;
        let source_members = group.members.len();
        let mut visitor = LabelElabVisitor { table: &table };
        let (members, owners) = visitor.lower_type_rec_members(group.members)?;
        let analysis = crate::pass::resolve::analyze_type_rec_members(&members);
        analyses.push((
            group_span,
            crate::pass::resolve::source_partition_analysis(&analysis, &owners, source_members),
        ));
    }
    Ok(analyses)
}

impl DesugaredToLowered for LabelElabVisitor<'_> {
    // The visit_* overrides below re-destructure their variant with a
    // let-else; the walk_* dispatcher routes each variant to its own
    // visit_*, so that let-else can fail only if a dispatcher routed
    // the wrong variant — a bug in this file, hence the bare
    // `unreachable!()`.
    fn rewrite_item_labels(&mut self, d: Labels<Desugared>) -> Result<Vec<Item<Lowered>>, Error> {
        let members = self
            .lower_labels(&d)?
            .into_iter()
            .map(lowered_type_member)
            .collect();
        normalize_compact_declaration(&d, members)
    }

    fn rewrite_type_rec_group(
        &mut self,
        group: TypeRecGroup<Desugared>,
    ) -> Result<Vec<Item<Lowered>>, Error> {
        let source_group = group.clone();
        let (members, owners) = self.lower_type_rec_members(group.members)?;
        normalize_recursive_group(&source_group, members, owners)
    }
}

impl LabelElabVisitor<'_> {
    fn walk_function_header(
        &mut self,
        sig: Signature<Desugared>,
        ret: Type<Desugared>,
    ) -> Result<(Signature<Lowered>, Type<Lowered>), Error> {
        let params = self.walk_signature_params(sig.params)?;
        let ret = self.walk_type(ret)?;
        Ok((Signature::from_parts(params, sig.groups), ret))
    }

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
                    for item in self.lower_labels(&labels)? {
                        members.push(lowered_type_member(item));
                        owners.push(owner);
                    }
                }
            }
        }
        Ok((members, owners))
    }

    /// Lower compact entries to ordinary singleton nominals and a named
    /// owner's complete ordinary alias body.
    fn lower_labels(&mut self, d: &Labels<Desugared>) -> Result<Vec<Item<Lowered>>, Error> {
        // Per-entry leading trivia is dropped here: the Labels item
        // ceases to exist at Lowered (replaced by generated newtypes
        // and the optional alias), so the surface comments don't have
        // a target. They survive `kio fmt` round-trips on the Surface AST.
        let declaration_entries: Vec<LabelEntry<Desugared>> = label_entries_in_source_order(d)?
            .into_iter()
            .cloned()
            .collect();
        let source_owner_span = label_source_owner_span(d);
        let mut out: Vec<Item<Lowered>> = Vec::with_capacity(declaration_entries.len() + 1);
        // Generate one newtype per explicit declaration. Constructor name `mk`,
        // projector name `get`.
        for entry in &declaration_entries {
            let name = &entry.name;
            let lowered_payload = self.walk_type(entry.payload.clone())?;
            let newtype_name = self
                .table
                .get(name)
                .expect("entry present in build_label_table")
                .newtype_name
                .clone();
            out.push(Item::Newtype(label_nominal_declaration(
                d,
                entry,
                newtype_name,
                lowered_payload,
            )));
        }
        let alias_body = d.type_alias_name.as_ref().map(|_| {
            named_nominal_alias_body(d, |entry| {
                vec![
                    self.table
                        .get(&entry.name)
                        .expect("entry present in build_label_table")
                        .newtype_name
                        .clone(),
                ]
            })
        });
        // For the named form, additionally emit a type alias
        // over the product/sum of generated label types.
        if let Some(name) = &d.type_alias_name {
            let body = self.walk_type(alias_body.expect("named owner has an alias body"))?;
            out.push(Item::TypeAlias(TypeAlias {
                vis: d.vis.clone(),
                name: name.clone(),
                name_span: d
                    .type_alias_span
                    .expect("named labels carry a type-name span"),
                type_params: d.type_alias_params.clone(),
                body,
                meta: Meta::new(source_owner_span),
                editable_span: d.editable_span,
                doc: None,
            }));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Expr, LabelsArm, PathSegment, Visibility};
    use crate::pass::desugar::desugar_module;
    use crate::pass::nominal_declarations::{
        named_label_alias_body, ordinary_type_member_name as lowered_type_member_name,
    };
    use crate::pass::parser::parse;

    fn elab(src: &str) -> Module<Lowered> {
        let m = parse(src).expect("parse");
        let m = desugar_module(m).expect("desugar");
        let parsed = vec![(PathBuf::from("test.kio"), m)];
        let (lowered, _) = elaborate_package(parsed, None).expect("elaborate");
        lowered.into_iter().next().unwrap().1
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
        let module = parse("module m; type Choice = { Foo: .; } | { Bar: .; };")
            .expect("parse distinct compact groups");
        let Item::Labels(mut labels, _) = module.items.into_iter().next().unwrap() else {
            panic!("expected compact nominal declaration");
        };
        let written_arms = labels.type_alias_arms.as_mut().expect("named groups");
        let repeated = written_arms[1].entries[0].clone();
        written_arms[0].entries.push(repeated);
        let arms = labels.arms_in_source_order().collect::<Vec<_>>();
        assert_eq!(
            arms.iter()
                .map(|arm| arm
                    .iter()
                    .map(|entry| entry.name.as_str())
                    .collect::<Vec<_>>())
                .collect::<Vec<_>>(),
            vec![vec!["Foo", "Bar"], vec!["Bar"]],
        );
        assert_eq!(arms[0][1].name_span, arms[1][0].name_span);
    }

    #[test]
    fn nominal_rhs_reference_preserves_written_type_arguments() {
        let module = elab(
            "module x; type { Value[*F][A]: F[A]; }; \
             type Choice[*G][B] = Value[G][B] & { Other: .; };",
        );
        let alias = module
            .items
            .iter()
            .find_map(|item| match item {
                Item::TypeAlias(alias) if alias.name == "Choice" => Some(alias),
                _ => None,
            })
            .expect("ordinary owner alias");
        let Type::Product { left, .. } = alias.type_body() else {
            panic!("expected product alias");
        };
        let Type::Path { segments, args, .. } = left.as_ref() else {
            panic!("expected existing nominal reference");
        };
        assert_eq!(
            segments.iter().map(PathSegment::as_str).collect::<Vec<_>>(),
            ["Value"]
        );
        assert_eq!(args.len(), 2);
        for (arg, expected) in args.iter().zip(["G", "B"]) {
            assert!(matches!(arg, Type::Path { segments, args, .. }
                if segments.len() == 1 && segments[0] == expected && args.is_empty()));
        }
    }

    #[test]
    fn label_nominal_shape_preserves_headers_visibility_and_owner_spans() {
        let module =
            parse("module m;\n/// Label family.\npub(m) type { Box[*F][A]<X>: F[A] & X };")
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
        assert!(owner.doc.is_some());
        assert_eq!(nominal.doc, owner.doc);

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
    fn shared_nominal_owner_does_not_duplicate_documentation() {
        for source in [
            "module m;\n/// Owner documentation.\ntype { First: .; Second: .; };",
            "module m;\n/// Owner documentation.\ntype Choice = { First: .; };",
        ] {
            let module = parse(source).unwrap();
            let Item::Labels(owner, _) = &module.items[0] else {
                panic!("expected shared nominal owner");
            };
            assert!(owner.doc.is_some());
            for entry in &owner.entries {
                let nominal = label_nominal_declaration(
                    owner,
                    entry,
                    entry.name.clone(),
                    entry.payload.clone(),
                );
                assert!(nominal.doc.is_none(), "{source}");
            }
        }
    }

    #[test]
    fn named_nominal_shape_preserves_groups_and_ordinary_type_arguments() {
        let module = parse(
            "module m; type Choice[*F][A][B] = \
             { First[*F][B]<X>: F[B] & X; Second[A]: A } | { Middle: . } | First[F][A];",
        )
        .expect("parse written product/sum before elaboration");
        let Item::Labels(owner, _) = &module.items[0] else {
            panic!("expected labels");
        };
        let mut visited = Vec::new();
        let body: Type<crate::ast::Surface> = named_nominal_alias_body(owner, |entry| {
            visited.push(entry.name.clone());
            vec!["selected".to_owned(), mint_label_newtype_name(&entry.name)]
        });
        assert_eq!(visited, ["First", "Second", "Middle"]);
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
        let assert_path = |ty: &Type<crate::ast::Surface>, path: &[&str], arguments: &[&str]| {
            let Type::Path { segments, args, .. } = ty else {
                panic!("expected supplied nominal path");
            };
            assert_eq!(
                segments.iter().map(PathSegment::as_str).collect::<Vec<_>>(),
                path,
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
        assert_path(&first, &["selected", "First"], &["F", "B"]);
        assert_path(&second, &["selected", "Second"], &["A"]);
        let Type::Sum {
            left: middle,
            right: last,
            ..
        } = *right
        else {
            panic!("expected right-associated remaining arms");
        };
        assert_path(&middle, &["selected", "Middle"], &[]);
        assert_path(&last, &["First"], &["F", "A"]);
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
            parse("module pkg/z; type { Dup : . }; type { Dup : I32 };").expect("parse"),
        )
        .expect("desugar");
        let a = desugar_module(
            parse("module pkg/a; type { Dup : . }; type { Dup : I32 };").expect("parse"),
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
        let m = elab("module x; type { Foo : . };");
        // Compact declarations lower to ordinary nominals with the exact written name.
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
            "module x; rec type Tree = \
             { Leaf: . } | { Branch: Tree & Tree };",
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
            "module x; rec type Pair = \
             { First: . | First; Second: . | Second };",
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
            .map(|index| format!("Field{index}: . | Field{index}"))
            .collect::<Vec<_>>()
            .join("; ");
        crate::pass::resolve::reset_type_rec_classification_work();
        let module = elab(&format!("module x; rec type Product = {{ {fields} }};"));
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
                " rec type {{ Field{index} : . | Field{index} {{ mk; get; }}; }};"
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
            ("First: . | First; Second: . | Second", true),
            ("First: .; Second: .", false),
        ] {
            let mut module = parse(&format!(
                "module x; rec {{\n/// Product fields.\npub type Product = {{ {fields} }};\ntype {{ Other : . | Other {{ mk; get; }}; }};\n}}"
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
               type A = { To_b: B }; \
               type { B : A { mk; get; }; }; \
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
               type A = { To_b: B }; \
               type { B : A { mk; get; }; }; \
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
               type { A: B & X; C: D & X }; \
               type { B: A; D: C }; \
               type { X : A & C { mk; get; }; }; \
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
               type A = { To_b: B }; \
               type B = { To_a: A }; \
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
            "type Pair = { First: . | First; Second: . | Second; }; ",
            "type { Other : . | Other { mk; get; }; }; ",
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
            "type A = { To_b: B; }; ",
            "type { B : A { mk; get; }; }; ",
            "type Spare = { Unused: .; }; ",
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
            "type { Nominal : Nominal { mk; get; }; }; ",
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
               rec type { A : B { mk; get; }; }; \
               type { B : A { mk; get; }; }; \
             }",
            "module x; rec { \
               rec type A = { To_b: B; }; \
               type { B : A { mk; get; }; }; \
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
    fn unmarked_compact_recursion_retains_written_diagnostic_heads() {
        for (source, head, members) in [
            (
                "module x; type Tree = { Leaf: . } | { Branch: Tree & Tree };",
                "Tree",
                3,
            ),
            ("module x; type { Node: Node; Leaf: . };", "type", 2),
            (
                "module x;\n/// Recursive family.\npub type { Node: Node; Leaf: . };",
                "type",
                2,
            ),
        ] {
            let module = elab(source);
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
            assert_eq!(
                &source[deferred.head_span.start as usize..deferred.head_span.end as usize],
                head,
            );
            assert_eq!(
                deferred.declaration_span.start as usize,
                source.find("type ").unwrap()
            );
            assert_eq!(group.members.len(), members);
        }
    }

    #[test]
    fn anonymous_compact_group_diagnostics_select_the_type_keyword() {
        for source in [
            "module x; rec { type { First: .; Second: . }; type Extra = .; }",
            "module x; rec { rec type { Node: Node; Leaf: . }; type Extra = .; }",
        ] {
            let error = elab_error(source);
            let Error::Parse(diagnostic) = error else {
                panic!("expected recursive-group diagnostic, got {error:?}");
            };
            let start = source.find("type {").unwrap() as u32;
            assert!(
                diagnostic
                    .secondary()
                    .iter()
                    .any(|label| label.span == Span::new(start, start + 4)),
                "anonymous compact head must select exactly `type`: {diagnostic:?}"
            );
        }
    }

    #[test]
    fn invalid_unmarked_recursive_labels_also_reach_semantic_validation() {
        let module = elab("module x; type Loop = { Step: Loop -> .; };");
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
        let error = elab_error("module x; rec type Pair = { First: .; Second: .; };");
        let Error::Parse(diagnostic) = error else {
            panic!("expected Parse, got {error:?}");
        };
        assert_eq!(diagnostic.message, "this `rec` marker is unnecessary");
        assert_eq!(diagnostic.fixes()[0].title, "Remove unnecessary `rec`");
    }

    #[test]
    fn labels_named_form_emits_type_alias() {
        let m = elab("module x; type T = { Foo : . };");
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
        let m = elab("module x; pub type { Foo : . };");
        if let Item::Newtype(d) = &m.items[0] {
            assert!(d.vis.is_pub());
            assert!(d.constructor.vis.is_pub());
            assert!(d.projector.vis.is_pub());
        }
    }

    #[test]
    fn nominal_rhs_reference_emits_no_second_declaration() {
        for source in [
            "module x; type { Foo: .; }; type Row = Foo & { Other: .; };",
            "module x; type Row = Foo & { Other: .; }; type { Foo: .; };",
        ] {
            let module = elab(source);
            let names = module
                .items
                .iter()
                .filter_map(|item| match item {
                    Item::Newtype(nominal) => Some(nominal.name.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(names.iter().filter(|name| **name == "Foo").count(), 1);
            assert_eq!(names.iter().filter(|name| **name == "Other").count(), 1);
            assert_eq!(names.len(), 2);
        }
    }

    #[test]
    fn nominal_rhs_reference_shares_identity_between_sum_arms() {
        let module = elab("module x; type Choice = { Bar: .; Baz: .; } | (Bar & { Other: .; });");
        assert_eq!(
            module
                .items
                .iter()
                .filter(|item| matches!(item,
            Item::Newtype(nominal) if nominal.name == "Bar"))
                .count(),
            1
        );
        let alias = module
            .items
            .iter()
            .find_map(|item| match item {
                Item::TypeAlias(alias) if alias.name == "Choice" => Some(alias),
                _ => None,
            })
            .expect("sum alias");
        let Type::Sum { left, right, .. } = alias.type_body() else {
            panic!("expected sum alias");
        };
        for arm in [left.as_ref(), right.as_ref()] {
            assert!(matches!(arm, Type::Product { left, .. }
                if matches!(left.as_ref(), Type::Path { segments, .. }
                    if segments.len() == 1 && segments[0] == "Bar")));
        }
    }

    #[test]
    fn nominal_rhs_reference_preserves_original_visibility() {
        let module = elab("module x; type { Foo: .; }; pub type Public_row = Foo & { Other: .; };");
        for item in &module.items {
            match item {
                Item::Newtype(nominal) if nominal.name == "Foo" => {
                    assert!(matches!(nominal.vis, Visibility::Private));
                    assert!(matches!(nominal.constructor.vis, Visibility::Private));
                    assert!(matches!(nominal.projector.vis, Visibility::Private));
                }
                Item::Newtype(nominal) if nominal.name == "Other" => {
                    assert!(matches!(nominal.vis, Visibility::Public));
                }
                Item::TypeAlias(alias) if alias.name == "Public_row" => {
                    assert!(matches!(alias.vis, Visibility::Public));
                }
                _ => panic!("unexpected declaration: {item:?}"),
            }
        }
        assert_eq!(module.items.len(), 3);
    }

    #[test]
    fn nominal_rhs_reference_keeps_ordinary_import_paths() {
        for (import, reference) in [
            ("import origin as dep;", "dep.Field"),
            ("import origin(Field);", "Field"),
        ] {
            let source =
                format!("module consumer; {import} type Row = {reference} & {{ Other: .; }};");
            let modules = elab_modules(&[
                ("origin.kio", "module origin; pub type { Field: .; };"),
                ("consumer.kio", &source),
            ])
            .expect("ordinary imported nominal reference");
            let consumer = &modules[1].1;
            let alias = consumer
                .items
                .iter()
                .find_map(|item| match item {
                    Item::TypeAlias(alias) if alias.name == "Row" => Some(alias),
                    _ => None,
                })
                .expect("ordinary owner alias");
            let Type::Product { left, .. } = alias.type_body() else {
                panic!("expected product alias");
            };
            assert!(matches!(left.as_ref(), Type::Path { segments, .. }
                if segments.iter().map(PathSegment::as_str).collect::<Vec<_>>().join(".") == reference));
            assert_eq!(consumer.imports.len(), 1);
        }
    }

    #[test]
    fn nominal_duplicate_declarations_have_no_payload_rewrite() {
        for source in [
            "module x; type { Foo: .; }; type Row = { Foo: .; Other: .; };",
            "module x; type { Foo: .; }; type Row = { Foo: !; };",
            "module x; type { Foo[A]: A; }; type Row[B] = { Foo[B]: B; };",
            "module x; type A = .; type { Foo <A>: A; }; type Row = { Foo: A; };",
        ] {
            let error = elab_error(source);
            let diagnostic = error.diagnostic();
            assert!(diagnostic.message.contains("already declared"), "{error:?}");
            assert_eq!(diagnostic.secondary().len(), 1);
            assert!(
                diagnostic
                    .help()
                    .unwrap()
                    .contains("ordinary type reference")
            );
            assert!(diagnostic.fixes().is_empty());
        }
    }

    #[test]
    fn nominal_nested_infer_does_not_suppress_declaration() {
        let module = elab("module x; type { Value[*F]: F[_]; };");
        assert!(matches!(&module.items[0], Item::Newtype(nominal) if nominal.name == "Value"));
    }

    #[test]
    fn generated_type_name_is_type_position_spelling() {
        let m = elab(
            "module x; \
             type { Foo : . }; \
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
        let Type::Path { segments, .. } = alias.type_body() else {
            panic!("the alias retains the written nominal type path");
        };
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0], "Foo");
    }

    #[test]
    fn lowercase_label_in_type_position_is_parse_error() {
        let err = parse(
            "module x; \
             type { Foo : . }; \
             type T = foo;",
        )
        .expect_err("lowercase label spelling is not type syntax");
        let msg = err.diag().1.to_owned();
        assert_eq!(msg, "type name `foo` must have an uppercase first letter");
    }

    #[test]
    fn recursive_nominal_reference_preserves_written_identity() {
        // The recursive payload keeps the written nominal identity through lowering.
        let m = elab("module x; rec type { List[A] : (. | (A & List[A])) };");
        let Item::Newtype(d) = &m.items[0] else {
            panic!("expected the recursive nominal declaration");
        };
        assert_eq!(d.name, "List");
        let self_ref =
            find_self_reference(&d.payload, &d.name).expect("self-reference inside payload");
        assert_eq!(self_ref, d.name);
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

    /// Read the type path retained across lowering for binder-shadowing assertions.
    fn type_path_head(t: &Type<Lowered>) -> &str {
        match t {
            Type::Path { segments, .. } if segments.len() == 1 => segments[0].as_str(),
            other => panic!("expected single-segment Type::Path, got {other:?}"),
        }
    }

    #[test]
    fn type_param_shadows_nominal_in_fn_signature() {
        // The function's `[A]` binder shadows the nominal in its parameter and return types.
        let m = elab("module x; type { A : . }; fn f[A](x: A) -> A { x }");
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
    fn type_param_shadows_nominal_in_alias_header() {
        let m = elab("module x; type { A : . }; type T[A] = A;");
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
    fn type_param_shadows_nominal_in_newtype_header() {
        let m = elab(
            "module x; type { A : . }; \
             type { N[A] : A { mk; get; }; };",
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
    fn type_param_shadows_nominal_in_forall() {
        // A `Type::Forall` mid-type binder `[A] A -> A` in a
        // parameter annotation shadows the nominal `A` in its body.
        let m = elab(
            "module x; type { A : . }; \
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
    fn type_param_shadows_nominal_in_fn_expr_signature() {
        let m = elab(
            "module x; type { A : . }; \
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
    fn ordinary_value_name_can_match_nominal_stem() {
        let m = elab(
            "module x; \
             type { Foo : . }; \
             fn foo() -> . { () }",
        );
        assert!(
            m.items
                .iter()
                .any(|item| matches!(item, Item::Newtype(d) if d.name == "Foo"))
        );
        assert!(
            m.items
                .iter()
                .any(|item| matches!(item, Item::FnDef(d) if d.name == "foo"))
        );
    }
}

#[cfg(test)]
mod nominal_lowering_tests {
    use crate::ast::{Item, Type, Visibility};

    fn lower(source: &str) -> crate::ast::Module<crate::ast::Lowered> {
        let module = crate::pass::parser::parse(source).unwrap();
        let module = crate::pass::desugar::desugar_module(module).unwrap();
        super::elaborate_package(vec![(std::path::PathBuf::from("x.kio"), module)], None)
            .unwrap()
            .0
            .remove(0)
            .1
    }

    #[test]
    fn nominal_lowering_preserves_grouped_mixed_type_shape() {
        let module = lower(
            "module x; type { Name: .; Age: .; } type Result = { Success: .; } | (Name & Age);",
        );
        let names = module
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Newtype(nominal) => Some(nominal.name.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(names, ["Name", "Age", "Success"]);
        let Item::TypeAlias(alias) = &module.items[3] else {
            panic!("alias");
        };
        let Type::Sum { left, right, .. } = &alias.body else {
            panic!("sum");
        };
        assert!(
            matches!(left.as_ref(), Type::Path { segments, .. } if segments[0].name == "Success")
        );
        assert!(matches!(right.as_ref(), Type::Product { .. }));
    }

    #[test]
    fn nominal_lowering_uses_independent_member_visibility() {
        let module = lower("module x; pub type Pair = { Open: .; Closed: . { pub mk; get; }; };");
        let Item::Newtype(open) = &module.items[0] else {
            panic!("open");
        };
        assert_eq!(open.constructor.vis, Visibility::Public);
        assert_eq!(open.projector.vis, Visibility::Public);
        let Item::Newtype(closed) = &module.items[1] else {
            panic!("closed");
        };
        assert_eq!(closed.constructor.vis, Visibility::Public);
        assert_eq!(closed.projector.vis, Visibility::Private);
    }
}
