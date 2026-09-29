//! Per-module typer environment: the catalogue of names in scope
//! plus the per-fn local-binding stack.
//!
//! [`ModuleEnv`] is the input to every typer walk: a module's own
//! items (type_aliases, newtypes, fn_defs), ambient host declarations,
//! and all ordinary imports brought in by `import` statements.
//!
//! [`TypeCtx`] adds the per-fn binding stack and the phase's
//! elaboration table (see [`super::TyperPhase`]) — together with
//! [`ModuleEnv`] it is everything a `synth_*` / `check_*` helper
//! needs to walk a fn body.
//!
//! Extracted from [`super::typecheck_core`] for navigability;
//! depends on the aliases sub-module ([`AliasCtx`], [`PayloadCtx`])
//! and the umbrella's [`super::TyperPhase`] trait.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::hash::Hash;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::ast::FnPurityExt;
use crate::error::Error;
use crate::pass::resolve::{NominalProvider, NominalRoute, NominalSelection};
use crate::span::Span;

use super::{
    AliasCtx, MemoCtx, PackageTypecheckScope, PayloadCtx, TypeInterner, TypecheckExecution,
    TyperPhase,
};

// =========================================================================
// Per-module environment
// =========================================================================

/// Per-module catalogue of names whose meaning is fixed by the
/// module's own items, plus host items and cross-module entries
/// brought in via `import` statements (each tagged with its
/// `role(...)` if any).
///
/// Phase-polymorphic — each map references AST nodes at the typer's
/// input phase. `typecheck_full` uses `ModuleEnv<'m, Lowered>`; the standalone
/// Kio' checker in `crate::prime::typer` uses `ModuleEnv<'m, Prime>`.
#[derive(Clone)]
pub struct ModuleEnv<'m, P>
where
    P: crate::ast::Phase,
{
    pub module: &'m crate::ast::Module<P>,
    pub package: Option<&'m crate::pass::resolve::Package<P>>,
    /// Slash module path (`a/b/c`) for this module. Used as a
    /// stable per-module identity key when recording `Elaborations`
    /// entries — spans are file-local byte offsets and collide
    /// across modules, so the typer prefixes elaboration keys with
    /// this path. Always present; assigned at `ModuleEnv::build`.
    pub module_path: String,
    nominal_declarations: HashMap<&'m str, crate::pass::resolve::TopLevelDeclaration<'m, P>>,
    pub type_aliases: HashMap<&'m str, super::AliasDef<'m, P>>,
    pub newtypes: HashMap<&'m str, &'m crate::ast::Newtype<P>>,
    pub fn_defs: HashMap<&'m str, &'m crate::ast::FnDef<P>>,
    /// User elaborator declarations in this module.
    pub user_elaborators: HashMap<&'m str, &'m crate::ast::UserElaboratorDef<P>>,
    /// Whether `import __intrinsics__;` appears in the module's import list.
    pub intrinsics_in_scope: bool,
    /// Singleton exact-role bindings retained for API consumers. Missing
    /// and ambiguous roles are absent. [`Self::resolve_exact_role`] is the
    /// authoritative query when a caller must distinguish those states.
    pub roles_in_scope: HashMap<crate::ast::Role, &'m str>,
    /// Role-bearing types available to tier-3 literal resolution, as
    /// `(role, env-type-name)`. This is also the authoritative direct
    /// candidate set for uses, such as `if` / `else`, that require one
    /// exact role binding in scope.
    pub host_env_roles: Vec<(crate::ast::Role, &'m str)>,
    /// Source position of every local role-bearing host declaration. Imported
    /// candidates have no entry and are visible throughout the module through
    /// its leading `import` clauses.
    local_host_role_item_indices: HashMap<&'m str, usize>,
    /// `type` aliases (local or imported) whose body resolves to a
    /// role-bearing host type, mapped to that inherited role. Kept
    /// **separate** from [`Self::host_env_roles`] on purpose: an alias is
    /// a valid literal *annotation* (`42(I32)` where `type I32 =
    /// provider.I32`) and a valid expected/fallback literal type, but it
    /// must not enlarge the tier-3 *bare-literal* candidate set — an alias
    /// and the host type it names are the same type, so counting both
    /// would spuriously flag a bare literal as ambiguous. Tier 3 consults
    /// this map only as a fallback when no host type carries the shape's
    /// role (the rehosted-module case, where the host type was replaced by
    /// the alias), grouping every alias spelling that reaches the same
    /// module-qualified host identity into one candidate. Exact-role users
    /// apply the same direct-candidate-first fallback through
    /// [`Self::resolve_exact_role`]. See `synth.rs` literal resolution.
    pub alias_roles: HashMap<&'m str, crate::ast::Role>,
    /// Alias-role bindings retain their declaration origin independently from
    /// the spelling-indexed annotation map above. A later local alias may share
    /// an imported alias's visible name without making that import disappear at
    /// earlier use sites.
    alias_role_candidates: Vec<AliasRoleCandidate<'m>>,
    pub env_type_names: HashSet<&'m str>,
    /// Env fn declarations available at call sites.
    pub env_fn_defs: HashMap<&'m str, &'m crate::ast::HostFn<P>>,
    /// Cross-module value imports brought in via `import pkg/helper(foo);`.
    /// The map points at the source module's `FnDef`,
    /// which the typer treats the same way as a same-module top-level
    /// fn at a call site.
    pub cross_module_fn_defs: HashMap<&'m str, &'m crate::ast::FnDef<P>>,
    /// The module each cross-module fn (`cross_module_fn_defs`) is
    /// *declared* in. The typer qualifies an imported fn's signature in its
    /// declaring module, not the caller's — a bare `Meters` in a
    /// dependency's `fn dep_len(m: Meters)` must resolve to that
    /// dependency's `(module, Meters)`, never the consumer's same-named
    /// type. Mirrors `cross_module_user_elaborator_modules`.
    pub cross_module_fn_def_modules: HashMap<&'m str, &'m crate::ast::Module<P>>,
    /// User elaborator declarations imported for `name!(...)` call sites.
    pub cross_module_user_elaborators: HashMap<&'m str, &'m crate::ast::UserElaboratorDef<P>>,
    pub cross_module_user_elaborator_modules: HashMap<&'m str, &'m crate::ast::Module<P>>,
    /// Cross-module type-alias imports — `import m(Foo);` where `Foo`
    /// is a `pub type`. Used by `unfold_top` so structural equality
    /// sees through imported aliases just like intra-module ones.
    pub cross_module_type_aliases: HashMap<&'m str, super::AliasDef<'m, P>>,
    /// Cross-module `newtype` imports — `import m(Foo);` where `Foo`
    /// is a `pub newtype`. Used
    /// by the typer's 2-segment path resolution so `Foo.mk_foo(...)`
    /// works the same way against an imported newtype as against a
    /// same-module one.
    ///
    /// Keyed by `(defining-module-path, bare-name)` so a nominal type's
    /// identity is `(module, name)`-exact: two distinct dependencies that
    /// each re-root a `pub newtype String` resolve to *distinct* table
    /// entries instead of colliding on the bare name. The defining-module
    /// path is the slash-joined module path the newtype is declared in.
    /// Same-module `newtypes` stays bare-keyed (the module is implicit —
    /// it is this env's [`Self::module_path`]).
    pub cross_module_newtypes: HashMap<(String, String), &'m crate::ast::Newtype<P>>,
    /// The module each cross-module `newtype` ([`Self::cross_module_newtypes`])
    /// is *declared* in. The typer qualifies an imported newtype's
    /// constructor/projector payload (and the member scheme's nominal) in
    /// its declaring module, not the caller's — a bare `String`
    /// in a dependency's `pub newtype Token : String` payload must resolve
    /// to that dependency's `(module, String)`, never the consumer's
    /// same-named type. Mirrors [`Self::cross_module_fn_def_modules`];
    /// same key as [`Self::cross_module_newtypes`].
    pub cross_module_newtype_modules: HashMap<(String, String), &'m crate::ast::Module<P>>,
    /// The module each selectively-imported cross-module `host fn`
    /// ([`Self::env_fn_defs`], for the entries from an `import m(foo);`)
    /// is *declared* in. The typer qualifies an imported host fn's
    /// signature in its declaring module, not the caller's.
    /// Same-module / ambient host fns are absent (their declaring module
    /// *is* this env's module). Mirrors [`Self::cross_module_fn_def_modules`].
    pub cross_module_env_fn_modules: HashMap<&'m str, &'m crate::ast::Module<P>>,
    /// Qualified-import aliases — `import pkg/helper as h;` registers
    /// `h` mapped to the target module. Body references like
    /// `h.foo()` synthesize against the source module's `FnDef`.
    pub qualified_imports: HashMap<&'m str, &'m crate::ast::Module<P>>,
    /// Whether `import __comptime__;` brings compile-time helpers into scope.
    pub comptime_in_scope: bool,
    /// Per-module pool for post-Surface type shapes used by structural
    /// equality and elaborator callbacks.
    pub type_interner: Arc<TypeInterner<P>>,
    /// Package-check-local state shared by every module in this typecheck.
    #[cfg(feature = "surface")]
    pub(crate) typecheck_scope: Arc<PackageTypecheckScope<P>>,
    /// Package-version memo tables shared by every module in this
    /// analysis, and by same-fingerprint LSP analyses only.
    pub memo: Arc<MemoCtx<P>>,
    identity_alias_newtypes: Arc<crate::pass::resolve::IdentityAliasNewtypeIndex>,
    #[cfg(feature = "surface")]
    typecheck_execution: TypecheckExecution,
}

/// One value-member path resolved through either a literal `newtype` head or
/// an identity-preserving transparent alias to a terminal `newtype`.
///
/// `source` is the declaration named by the written path (and therefore the
/// editor/navigation identity for that head). `newtype` and
/// `declaring_module` are the exact terminal nominal and member owner used for
/// typing. Keeping both prevents a re-export alias from being mistaken for a
/// fresh nominal declaration while still letting its public callable surface
/// reach the one shared constructor/projector pair.
pub(crate) struct MemberNewtypeResolution<'m, P: crate::ast::Phase> {
    pub(crate) source: crate::pass::resolve::TopLevelDeclaration<'m, P>,
    pub(crate) source_module: &'m crate::ast::Module<P>,
    pub(crate) newtype: &'m crate::ast::Newtype<P>,
    pub(crate) declaring_module: &'m crate::ast::Module<P>,
}

/// The exact-role candidates visible at one use site.
///
/// Same-role declarations are valid, so multiplicity is retained until a
/// construct asks for a singleton. Keeping the ambiguous state explicit
/// prevents source or map iteration order from selecting a declaration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoleResolution<'m> {
    Missing,
    Unique(&'m str),
    Ambiguous { first: &'m str, second: &'m str },
}

struct DirectRoleCandidate<'m> {
    visible_name: &'m str,
    identity: Option<(String, String)>,
}

#[derive(Clone)]
struct AliasRoleCandidate<'m> {
    visible_name: &'m str,
    role: crate::ast::Role,
    host_identity: (String, String),
    local_item_index: Option<usize>,
}

fn direct_role_candidate_label(candidate: &DirectRoleCandidate<'_>) -> String {
    match &candidate.identity {
        Some((module, leaf)) if !module.is_empty() => format!("`{module}.{leaf}`"),
        Some((_, leaf)) => format!("`{leaf}`"),
        None => format!("`{}`", candidate.visible_name),
    }
}

/// The slash module-path string the typer keys `Elaborations` /
/// `PrimeElaborations` under.
pub fn module_path_key<P: crate::ast::Phase>(
    module: &crate::ast::Module<P>,
    _package_name: Option<&str>,
) -> String {
    module
        .path
        .segments
        .iter()
        .map(|s| s.name.as_str())
        .collect::<Vec<_>>()
        .join("/")
}

impl<'m, P> ModuleEnv<'m, P>
where
    P: crate::pass::resolve::ResolvePhase,
{
    /// Build a per-module env: scan items for type alias / newtype /
    /// fn, add ambient host declarations, then fold in ordinary imports.
    pub fn build(
        module: &'m crate::ast::Module<P>,
        package_file: Option<&'m crate::ast::PackageFile<P>>,
        package_name: Option<&str>,
        package: Option<&'m crate::pass::resolve::Package<P>>,
    ) -> Result<Self, Error> {
        Self::build_with_interner(
            module,
            package_file,
            package_name,
            package,
            Arc::new(TypeInterner::default()),
        )
    }

    pub fn build_with_interner(
        module: &'m crate::ast::Module<P>,
        package_file: Option<&'m crate::ast::PackageFile<P>>,
        package_name: Option<&str>,
        package: Option<&'m crate::pass::resolve::Package<P>>,
        type_interner: Arc<TypeInterner<P>>,
    ) -> Result<Self, Error> {
        Self::build_with_typecheck_scope_and_execution(
            module,
            package_file,
            package_name,
            package,
            PackageTypecheckScope::fresh_for_module(module, package, type_interner),
            TypecheckExecution::AllowParallel,
        )
    }

    #[cfg(feature = "surface")]
    pub(crate) fn build_with_typecheck_scope(
        module: &'m crate::ast::Module<P>,
        package_file: Option<&'m crate::ast::PackageFile<P>>,
        package_name: Option<&str>,
        package: Option<&'m crate::pass::resolve::Package<P>>,
        typecheck_scope: Arc<PackageTypecheckScope<P>>,
    ) -> Result<Self, Error> {
        Self::build_with_typecheck_scope_and_execution(
            module,
            package_file,
            package_name,
            package,
            typecheck_scope,
            TypecheckExecution::AllowParallel,
        )
    }

    pub(crate) fn build_with_typecheck_scope_and_execution(
        module: &'m crate::ast::Module<P>,
        package_file: Option<&'m crate::ast::PackageFile<P>>,
        package_name: Option<&str>,
        package: Option<&'m crate::pass::resolve::Package<P>>,
        typecheck_scope: Arc<PackageTypecheckScope<P>>,
        typecheck_execution: TypecheckExecution,
    ) -> Result<Self, Error> {
        #[cfg(not(feature = "surface"))]
        let _ = typecheck_execution;
        typecheck_scope.assert_module(module, package);
        let module_path_str = module_path_key(module, package_name);
        let type_interner = typecheck_scope.type_interner().clone();
        let memo = typecheck_scope.memo().clone();
        let identity_alias_newtypes = typecheck_scope.identity_alias_newtypes(module, package);
        let mut env = ModuleEnv {
            module,
            package,
            module_path: module_path_str,
            nominal_declarations: HashMap::new(),
            type_aliases: HashMap::new(),
            newtypes: HashMap::new(),
            fn_defs: HashMap::new(),
            user_elaborators: HashMap::new(),
            intrinsics_in_scope: false,
            roles_in_scope: HashMap::new(),
            host_env_roles: Vec::new(),
            local_host_role_item_indices: HashMap::new(),
            alias_roles: HashMap::new(),
            alias_role_candidates: Vec::new(),
            env_type_names: HashSet::new(),
            env_fn_defs: HashMap::new(),
            cross_module_fn_defs: HashMap::new(),
            cross_module_fn_def_modules: HashMap::new(),
            cross_module_user_elaborators: HashMap::new(),
            cross_module_user_elaborator_modules: HashMap::new(),
            cross_module_type_aliases: HashMap::new(),
            cross_module_newtypes: HashMap::new(),
            cross_module_newtype_modules: HashMap::new(),
            cross_module_env_fn_modules: HashMap::new(),
            qualified_imports: HashMap::new(),
            comptime_in_scope: false,
            type_interner,
            #[cfg(feature = "surface")]
            typecheck_scope,
            memo,
            identity_alias_newtypes,
            #[cfg(feature = "surface")]
            typecheck_execution,
        };
        for u in &module.imports {
            if matches!(u.kind, crate::ast::ImportKind::Intrinsics) {
                env.intrinsics_in_scope = true;
            }
            if matches!(u.kind, crate::ast::ImportKind::Comptime) {
                env.comptime_in_scope = true;
                env.host_env_roles
                    .push((crate::ast::Role::Bool, "Comptime_bool"));
                env.host_env_roles
                    .push((crate::ast::Role::Str, "Comptime_str"));
            }
        }
        for (item_index, item) in module.items.iter().enumerate() {
            match item {
                crate::ast::Item::FnDef(d) => {
                    env.fn_defs.insert(d.name.as_str(), d);
                }
                crate::ast::Item::Elaborator(s, _ext) => {
                    env.user_elaborators.insert(s.name.as_str(), s);
                }
                // Every `Item::TypeAlias` reaching a typer phase is a type.
                crate::ast::Item::TypeAlias(a) => {
                    env.nominal_declarations.insert(
                        a.name.as_str(),
                        crate::pass::resolve::TopLevelDeclaration::Item(item),
                    );
                    env.type_aliases.insert(
                        a.name.as_str(),
                        crate::pass::typecheck_core::AliasDef {
                            type_params: &a.type_params,
                            body: a.type_body(),
                            owner_module: None,
                        },
                    );
                }
                crate::ast::Item::Newtype(d) => {
                    env.nominal_declarations.insert(
                        d.name.as_str(),
                        crate::pass::resolve::TopLevelDeclaration::Item(item),
                    );
                    env.newtypes.insert(d.name.as_str(), d);
                }
                crate::ast::Item::TypeRecGroup(group) => {
                    for member in &group.members {
                        match member {
                            crate::ast::TypeRecMember::TypeAlias(alias) => {
                                env.nominal_declarations.insert(
                                    alias.name.as_str(),
                                    crate::pass::resolve::TopLevelDeclaration::TypeRecMember(
                                        member,
                                    ),
                                );
                                env.type_aliases.insert(
                                    alias.name.as_str(),
                                    crate::pass::typecheck_core::AliasDef {
                                        type_params: &alias.type_params,
                                        body: alias.type_body(),
                                        owner_module: None,
                                    },
                                );
                            }
                            crate::ast::TypeRecMember::Newtype(newtype) => {
                                env.nominal_declarations.insert(
                                    newtype.name.as_str(),
                                    crate::pass::resolve::TopLevelDeclaration::TypeRecMember(
                                        member,
                                    ),
                                );
                                env.newtypes.insert(newtype.name.as_str(), newtype);
                            }
                            crate::ast::TypeRecMember::Labels(_, ext) => match *ext {},
                        }
                    }
                }
                // Host items are ordinary scoped declarations (the
                // former `env` block). A role-bearing `host type`
                // contributes its role to lexical scope; ambiguity from
                // two same-role types in scope surfaces at use sites
                // (see `synth.rs` tier-3 literal selection), not here.
                crate::ast::Item::HostType(h) => {
                    env.env_type_names.insert(h.name.as_str());
                    if let Some(role) = h.role {
                        env.local_host_role_item_indices
                            .insert(h.name.as_str(), item_index);
                        env.host_env_roles.push((role.role, h.name.as_str()));
                    }
                }
                crate::ast::Item::HostFn(h) => {
                    env.env_fn_defs.insert(h.name.as_str(), h);
                }
                // Statically uninhabited per the
                // `ItemLabels = Never` bound on `P`.
                crate::ast::Item::LiteralAlias(_, ext) => match *ext {},
                crate::ast::Item::Labels(_, ext) => match *ext {},
                crate::ast::Item::LabelForward(_, ext) => match *ext {},
                // `equiv` decls don't introduce names into the module
                // namespace. The typer validates their bodies in a
                // separate pass; the runner picks them up directly
                // from `module.items`.
                crate::ast::Item::Equiv(_, _ext) => {}
                // Statically uninhabited at every typer phase.
                crate::ast::Item::Op(_, ext) => match *ext {},
                crate::ast::Item::VariadicOperator(_, ext) => match *ext {},
                crate::ast::Item::RecGroup(_, ext) => match *ext {},
            }
        }
        let _ = (package_file, package_name);
        if let Some(pkg) = package {
            populate_cross_module_imports(&mut env, &module.imports, pkg);
        }
        register_alias_inherited_roles(&mut env, module);
        rebuild_singleton_roles_in_scope(&mut env);
        Ok(env)
    }
}

impl<'m, P: crate::ast::Phase> ModuleEnv<'m, P> {
    #[cfg(feature = "surface")]
    pub(crate) fn typecheck_execution(&self) -> TypecheckExecution {
        self.typecheck_execution
    }

    /// Resolve one exact `role(...)` in this lexical scope without
    /// discarding multiplicity. Direct role types win over aliases; aliases
    /// are the fallback only when there is no direct candidate, matching
    /// tier-3 literal resolution. Candidates are sorted, and transparent
    /// aliases are de-duplicated by the nominal identity of the host type they
    /// name, so neither diagnostics nor the selected representative spelling
    /// depend on declaration or hash-map iteration order.
    pub fn resolve_exact_role(&self, role: crate::ast::Role) -> RoleResolution<'m> {
        self.resolve_exact_role_at(role, None)
    }

    pub(crate) fn resolve_exact_role_at(
        &self,
        role: crate::ast::Role,
        local_item_cutoff: Option<usize>,
    ) -> RoleResolution<'m> {
        match self.resolve_direct_role_candidates(
            |candidate_role| candidate_role == role,
            local_item_cutoff,
        ) {
            RoleResolution::Unique(name) => return RoleResolution::Unique(name),
            RoleResolution::Ambiguous { first, second } => {
                return RoleResolution::Ambiguous { first, second };
            }
            RoleResolution::Missing => {}
        }

        self.resolve_alias_role_candidates(
            |candidate_role| candidate_role == role,
            local_item_cutoff,
        )
    }

    pub(crate) fn resolve_direct_role_candidates(
        &self,
        admits: impl Fn(crate::ast::Role) -> bool,
        local_item_cutoff: Option<usize>,
    ) -> RoleResolution<'m> {
        let candidates = self.direct_role_candidates(admits, local_item_cutoff);
        match candidates.as_slice() {
            [] => RoleResolution::Missing,
            [candidate] => RoleResolution::Unique(candidate.visible_name),
            [first, second, ..] => RoleResolution::Ambiguous {
                first: first.visible_name,
                second: second.visible_name,
            },
        }
    }

    pub(crate) fn describe_direct_role_ambiguity(
        &self,
        admits: impl Fn(crate::ast::Role) -> bool,
        local_item_cutoff: Option<usize>,
    ) -> Option<String> {
        let candidates = self.direct_role_candidates(admits, local_item_cutoff);
        let [first, second, ..] = candidates.as_slice() else {
            return None;
        };
        Some(format!(
            "{}, {}, …",
            direct_role_candidate_label(first),
            direct_role_candidate_label(second)
        ))
    }

    pub(crate) fn describe_exact_role_ambiguity(
        &self,
        role: crate::ast::Role,
        first: &str,
        second: &str,
        local_item_cutoff: Option<usize>,
    ) -> String {
        let candidates =
            self.direct_role_candidates(|candidate_role| candidate_role == role, local_item_cutoff);
        let direct = match candidates.as_slice() {
            [first, second, ..] => Some(format!(
                "{}, {}, …",
                direct_role_candidate_label(first),
                direct_role_candidate_label(second)
            )),
            _ => None,
        };
        direct.unwrap_or_else(|| format!("`{first}`, `{second}`, …"))
    }

    fn direct_role_candidates(
        &self,
        admits: impl Fn(crate::ast::Role) -> bool,
        local_item_cutoff: Option<usize>,
    ) -> Vec<DirectRoleCandidate<'m>> {
        let mut raw = self
            .host_env_roles
            .iter()
            .filter_map(|(role, name)| {
                (self.role_binding_is_visible(name, local_item_cutoff) && admits(*role))
                    .then_some((*role, *name))
            })
            .collect::<Vec<_>>();
        raw.sort_unstable_by(|(left_role, left_name), (right_role, right_name)| {
            left_role
                .as_str()
                .cmp(right_role.as_str())
                .then_with(|| left_name.cmp(right_name))
        });

        let mut recovered = self.recover_direct_role_candidates(local_item_cutoff);
        recovered.retain(|(role, _, _)| admits(*role));

        let mut keys = raw.clone();
        keys.dedup();
        let mut candidates = Vec::new();
        for (role, name) in keys {
            let raw_count = raw
                .iter()
                .filter(|(candidate_role, candidate_name)| {
                    *candidate_role == role && *candidate_name == name
                })
                .count();
            let known = recovered
                .iter()
                .filter(|(candidate_role, candidate_name, _)| {
                    *candidate_role == role && *candidate_name == name
                })
                .collect::<Vec<_>>();
            if !known.is_empty() && known.len() <= raw_count {
                candidates.extend(known.into_iter().map(|(_, candidate_name, identity)| {
                    DirectRoleCandidate {
                        visible_name: candidate_name,
                        identity: Some(identity.clone()),
                    }
                }));
            } else {
                candidates.push(DirectRoleCandidate {
                    visible_name: name,
                    identity: None,
                });
            }
        }

        candidates.sort_unstable_by(|left, right| {
            left.visible_name
                .cmp(right.visible_name)
                .then_with(|| left.identity.cmp(&right.identity))
        });
        let mut unique: Vec<DirectRoleCandidate<'m>> = Vec::new();
        for candidate in candidates {
            let duplicate =
                unique
                    .iter()
                    .any(|existing| match (&candidate.identity, &existing.identity) {
                        (Some(left), Some(right)) => left == right,
                        (None, None) => candidate.visible_name == existing.visible_name,
                        (Some(_), None) | (None, Some(_)) => false,
                    });
            if !duplicate {
                unique.push(candidate);
            }
        }
        unique
    }

    fn role_binding_is_visible(&self, name: &str, local_item_cutoff: Option<usize>) -> bool {
        !local_item_cutoff.is_some_and(|cutoff| {
            self.local_host_role_item_indices
                .get(name)
                .is_some_and(|item_index| *item_index >= cutoff)
        })
    }

    fn recover_direct_role_candidates(
        &self,
        local_item_cutoff: Option<usize>,
    ) -> Vec<(crate::ast::Role, &'m str, (String, String))> {
        let Some(module) = self.this_module_entry().map(|entry| &entry.module) else {
            return Vec::new();
        };
        let mut candidates = Vec::new();
        for usage in &module.imports {
            if matches!(usage.kind, crate::ast::ImportKind::Comptime) {
                candidates.push((
                    crate::ast::Role::Bool,
                    "Comptime_bool",
                    ("__comptime__".to_owned(), "Comptime_bool".to_owned()),
                ));
                candidates.push((
                    crate::ast::Role::Str,
                    "Comptime_str",
                    ("__comptime__".to_owned(), "Comptime_str".to_owned()),
                ));
            }
        }
        let local_path = module_path_key(module, None);
        for (item_index, item) in module.items.iter().enumerate() {
            if local_item_cutoff.is_some_and(|cutoff| item_index >= cutoff) {
                continue;
            }
            if let crate::ast::Item::HostType(host) = item
                && let Some(role) = host.role
            {
                candidates.push((
                    role.role,
                    host.name.as_str(),
                    (local_path.clone(), host.name.clone()),
                ));
            }
        }
        let Some(package) = self.package else {
            return candidates;
        };
        for usage in &module.imports {
            let crate::ast::ImportKind::Selective { items, from } = &usage.kind else {
                continue;
            };
            let from_path = from
                .segments
                .iter()
                .map(crate::ast::PathSegment::as_str)
                .collect::<Vec<_>>()
                .join("/");
            let Some(target) = package.module(&from_path) else {
                continue;
            };
            let target_path = module_path_key(&target.module, None);
            for name in items.iter().filter_map(crate::ast::ImportItem::as_name) {
                #[cfg(all(test, feature = "surface"))]
                record_nominal_provider_item_visit();
                let Some(declaration) = target
                    .scope
                    .lookup(name)
                    .and_then(|id| crate::pass::resolve::declaration_by_id(&target.module, id))
                else {
                    continue;
                };
                if let Some(host) = declaration.host_type()
                    && host.name == name
                    && let Some(role) = host.role
                {
                    candidates.push((role.role, name, (target_path.clone(), name.to_owned())));
                }
            }
        }
        candidates
    }

    pub(crate) fn resolve_alias_role_candidates(
        &self,
        admits: impl Fn(crate::ast::Role) -> bool,
        local_item_cutoff: Option<usize>,
    ) -> RoleResolution<'m> {
        let mut candidates = self
            .alias_role_candidates
            .iter()
            .filter(|candidate| {
                let local_is_visible = candidate.local_item_index.is_none_or(|item_index| {
                    local_item_cutoff.is_none_or(|cutoff| item_index < cutoff)
                });
                local_is_visible && admits(candidate.role)
            })
            .collect::<Vec<_>>();
        candidates.sort_unstable_by(|left, right| {
            left.visible_name
                .cmp(right.visible_name)
                .then_with(|| left.host_identity.cmp(&right.host_identity))
        });
        let mut aliases = Vec::with_capacity(candidates.len());
        let mut identities = Vec::with_capacity(candidates.len());
        for candidate in candidates {
            if identities.contains(&candidate.host_identity) {
                continue;
            }
            identities.push(candidate.host_identity.clone());
            aliases.push(candidate.visible_name);
        }
        match aliases.as_slice() {
            [] => RoleResolution::Missing,
            [name] => RoleResolution::Unique(name),
            [first, second, ..] => RoleResolution::Ambiguous { first, second },
        }
    }

    /// Resolve the role of the host type or transparent alias declared at one
    /// exact nominal identity. Imports owned by the target module do not become
    /// declarations in its qualified namespace.
    pub(crate) fn declared_role_for_nominal_identity(
        &self,
        module_path: &str,
        name: &str,
    ) -> Option<crate::ast::Role> {
        let owner = self.module_for_identity(module_path)?;
        let provider = NominalProvider::new(Some(self.module), self.package);
        let scope = provider.scope_for_owner(Some(owner));
        let declaration = provider.declaration(scope, name, NominalRoute::Exact)?;
        if let Some(host) = declaration.declaration.host_type() {
            host.role.map(|role| role.role)
        } else if let Some(alias) = declaration.declaration.type_alias() {
            if module_path != self.module_path
                && !crate::pass::resolve::is_visible(&alias.vis, &self.module.path)
            {
                return None;
            }
            monomorphic_alias_inherited_role(
                &alias.type_params,
                alias.type_body(),
                declaration.owner,
                &provider,
                &mut Vec::new(),
            )
            .map(|binding| binding.role)
        } else {
            None
        }
    }

    /// Resolve one exact module identity without requiring the source module
    /// itself to be indexed in the surrounding package. A matching source path
    /// is terminal; otherwise only the package's exact module entry is used.
    pub(crate) fn module_for_identity(
        &self,
        module_path: &str,
    ) -> Option<&'m crate::ast::Module<P>> {
        if module_path == self.module_path {
            Some(self.module)
        } else {
            self.package
                .and_then(|package| package.module(module_path))
                .map(|entry| &entry.module)
        }
    }

    /// This env's module entry in the package, when a package is in scope.
    fn this_module_entry(&self) -> Option<&'m crate::pass::resolve::ModuleEntry<P>> {
        self.package
            .and_then(|p| p.module(&self.module_path))
            .filter(|entry| std::ptr::eq(&entry.module, self.module))
    }
}

impl<'m, P> ModuleEnv<'m, P>
where
    P: crate::pass::resolve::ResolvePhase,
{
    /// Lift the env's two type-alias maps into the shared
    /// [`AliasCtx`] view consumed by [`unfold_top`] / [`type_equiv`] /
    /// [`require_type_equiv`]. Lets callers pass an env-shaped reference
    /// at call sites without making the shared helpers depend on
    /// `ModuleEnv`'s shape.
    pub fn alias_ctx(&self) -> AliasCtx<'_, 'm, P> {
        AliasCtx {
            local: &self.type_aliases,
            cross_module: &self.cross_module_type_aliases,
            type_interner: Some(self.type_interner.as_ref()),
            source_module: Some(self.module),
            package: self.package,
            binder_locals: None,
        }
    }

    /// Resolve a nominal `Type::Path`'s segments to its identity-exact
    /// `(defining-module-path, bare-name)` — the open-world-safe key a
    /// nominal type is matched by. A bare head is
    /// resolved through this module's imports / local declarations (a
    /// selective `import M(X);` → `M`; a same-module declaration → this
    /// module; an already-qualified `m.b.X` → `m/b`). Returns `None` when
    /// the head is a type parameter, an intrinsic, or otherwise carries
    /// no module identity (a local type-param keeps a single segment and
    /// the qualifier leaves it bare).
    pub fn nominal_ref_identity(
        &self,
        segments: &[crate::ast::PathSegment],
    ) -> Option<(String, String)> {
        let qualified = self.qualify_nominal_segments(segments);
        let (name, module_segments) = qualified.split_last()?;
        if module_segments.is_empty() {
            return None;
        }
        let module_path = module_segments
            .iter()
            .map(crate::ast::PathSegment::as_str)
            .collect::<Vec<_>>()
            .join("/");
        Some((module_path, name.as_str().to_owned()))
    }

    /// The fully-qualified segment vector for a nominal `Type::Path`'s
    /// head, via the module's open-world-safe qualifier. Thin wrapper
    /// over [`crate::pass::resolve::qualify_type_segments_in_module`]
    /// pinned directly to this env's source module, including for standalone
    /// and compiler-created auxiliary modules outside the package index.
    pub fn qualify_nominal_segments(
        &self,
        segments: &[crate::ast::PathSegment],
    ) -> Vec<crate::ast::PathSegment> {
        self.this_module_entry().map_or_else(
            || {
                crate::pass::resolve::qualify_type_segments_in_module(
                    segments,
                    self.module,
                    &HashMap::new(),
                )
            },
            |entry| {
                crate::pass::resolve::qualify_type_segments_in_entry(
                    segments,
                    entry,
                    &HashMap::new(),
                )
            },
        )
    }

    /// Look up an imported `newtype` by its identity-exact key. A bare
    /// head is resolved to its defining module first (so `Box` and the
    /// qualified `dep.lib.Box` agree). Selective imports use the
    /// `(module, name)` table; qualified imports retain their alias-only
    /// scope and are read from the package by that exact identity.
    pub fn cross_module_newtype(
        &self,
        segments: &[crate::ast::PathSegment],
    ) -> Option<&'m crate::ast::Newtype<P>> {
        let key = self.nominal_ref_identity(segments)?;
        self.cross_module_newtypes.get(&key).copied().or_else(|| {
            self.package_public_newtype(&key.0, &key.1)
                .map(|(newtype, _)| newtype)
        })
    }

    /// The module a cross-module `newtype` (resolved by [`Self::cross_module_newtype`])
    /// is declared in. The typer qualifies the imported newtype's member
    /// payload in this module so a bare nominal head in the payload keeps
    /// the declaring module's identity. Keyed identity-exact,
    /// exactly as [`Self::cross_module_newtype`].
    pub fn cross_module_newtype_module(
        &self,
        segments: &[crate::ast::PathSegment],
    ) -> Option<&'m crate::ast::Module<P>> {
        let key = self.nominal_ref_identity(segments)?;
        self.cross_module_newtype_modules
            .get(&key)
            .copied()
            .or_else(|| {
                self.package_public_newtype(&key.0, &key.1)
                    .map(|(_, module)| module)
            })
    }

    /// Resolve a term-level newtype member head through the same exact
    /// declaration/import authority as type paths. A transparent alias is
    /// admitted only when every edge forwards its binders positionally and
    /// the terminal newtype has the same fully-saturated binder kinds. This is
    /// precisely the identity-alias class used by `retype` re-exports; a
    /// structural, reordered, partial, cyclic, or missing alias chain exposes
    /// no constructor/projector surface.
    pub(crate) fn resolve_member_newtype(
        &self,
        segments: &[crate::ast::PathSegment],
    ) -> Option<MemberNewtypeResolution<'m, P>> {
        if segments.is_empty()
            || segments.len() > 2
            || (segments.len() == 2 && !self.qualified_imports.contains_key(segments[0].as_str()))
        {
            return None;
        }
        if let [head] = segments
            && let Some(newtype) = self.newtypes.get(head.as_str()).copied()
            && let Some(source) = self.nominal_declarations.get(head.as_str()).copied()
        {
            return Some(MemberNewtypeResolution {
                source,
                source_module: self.module,
                newtype,
                declaring_module: self.module,
            });
        }
        let (source_module_path, source_name) = self.nominal_ref_identity(segments)?;
        let (source_owner, source) = self.type_declaration(&source_module_path, &source_name)?;
        if let Some(newtype) = source.newtype() {
            return Some(MemberNewtypeResolution {
                source,
                source_module: source_owner,
                newtype,
                declaring_module: source_owner,
            });
        }
        source.type_alias()?;
        let (terminal_module_path, terminal_name) = self
            .identity_alias_newtypes
            .terminal_key(&source_module_path, &source_name)?;
        let (terminal_owner, terminal) =
            self.type_declaration(terminal_module_path, terminal_name)?;
        let newtype = terminal.newtype()?;
        Some(MemberNewtypeResolution {
            source,
            source_module: source_owner,
            newtype,
            declaring_module: terminal_owner,
        })
    }

    fn type_declaration(
        &self,
        module_path: &str,
        name: &str,
    ) -> Option<(
        &'m crate::ast::Module<P>,
        crate::pass::resolve::TopLevelDeclaration<'m, P>,
    )> {
        if module_path == self.module_path {
            return self
                .nominal_declarations
                .get(name)
                .copied()
                .map(|declaration| (self.module, declaration));
        }
        let entry = self.package?.module(module_path)?;
        entry
            .scope
            .lookup(name)
            .and_then(|id| crate::pass::resolve::declaration_by_id(&entry.module, id))
            .map(|declaration| (&entry.module, declaration))
    }

    /// The **cross-module** declaring module of a `newtype` referenced by
    /// `segments`, or `None` when the newtype is same-module (or unresolved).
    /// The typer qualifies a projected field's payload through this module so
    /// a bare *cross-module* nominal head in the payload keeps the declaring
    /// module's identity. A **same-module** newtype deliberately
    /// returns `None`: its payload needs no up-front qualification (the
    /// checking module *is* the declaring one, so the later identity-exact
    /// canonicalizer resolves a bare head correctly), and qualifying it to a
    /// fully-qualified same-module path would surface a spurious cross-module
    /// reference in the emitted Kio' — e.g. an `import testapi/main(Bar);`
    /// import of a private label newtype. Mirrors the cross-module-only
    /// routing of the other declaring-module surfaces (`cross_module_newtype_module`,
    /// `cross_module_fn_def_modules`, `cross_module_env_fn_modules`).
    pub fn cross_module_newtype_owner(
        &self,
        segments: &[crate::ast::PathSegment],
    ) -> Option<&'m crate::ast::Module<P>> {
        let (module, name) = self.nominal_ref_identity(segments)?;
        if module == self.module_path {
            None
        } else {
            self.cross_module_newtype_modules
                .get(&(module.clone(), name.clone()))
                .copied()
                .or_else(|| {
                    self.package_public_newtype(&module, &name)
                        .map(|(_, module)| module)
                })
        }
    }

    fn package_public_newtype(
        &self,
        module_path: &str,
        name: &str,
    ) -> Option<(&'m crate::ast::Newtype<P>, &'m crate::ast::Module<P>)> {
        let entry = self.package?.module(module_path)?;
        let declaration = entry
            .scope
            .lookup(name)
            .and_then(|id| crate::pass::resolve::declaration_by_id(&entry.module, id))?;
        let newtype = declaration
            .newtype()
            .filter(|newtype| newtype.vis.is_pub() && newtype.name == name)?;
        Some((newtype, &entry.module))
    }

    /// Lift the env's alias and newtype maps into the shared
    /// [`PayloadCtx`] view consumed by [`check_newtype_payload`] /
    /// [`param_appears_negatively`].
    pub fn payload_ctx(&self) -> PayloadCtx<'_, 'm, P> {
        PayloadCtx {
            aliases: self.alias_ctx(),
            newtypes: &self.newtypes,
        }
    }
}

#[cfg(all(test, feature = "surface"))]
thread_local! {
    static NOMINAL_PROVIDER_ITEM_VISITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

#[cfg(all(test, feature = "surface"))]
fn record_nominal_provider_item_visit() {
    NOMINAL_PROVIDER_ITEM_VISITS.with(|visits| visits.set(visits.get() + 1));
}

#[cfg(all(test, feature = "surface"))]
fn reset_nominal_provider_item_visits() {
    NOMINAL_PROVIDER_ITEM_VISITS.with(|visits| visits.set(0));
    crate::pass::resolve::reset_nominal_provider_work();
}

#[cfg(all(test, feature = "surface"))]
fn nominal_provider_item_visits() -> usize {
    let direct = NOMINAL_PROVIDER_ITEM_VISITS.with(std::cell::Cell::get);
    let provider = crate::pass::resolve::nominal_provider_work();
    direct
        + provider.declaration_items_indexed
        + provider.edge_target_lookups
        + provider.exact_target_lookups
}

/// Walk same-package selective imports — `import pkg/helper(foo);`
/// brings the named values and types into scope — and register the
/// targets in the appropriate maps. The resolver has already verified
/// that the target module exists and the names are pub; here we just
/// remember the source items so the typer can pull schemes
/// (`cross_module_fn_defs`) and unfold cross-module aliases
/// (`cross_module_type_aliases`) at the consumer's site.
pub fn populate_cross_module_imports<'m, P>(
    env: &mut ModuleEnv<'m, P>,
    imports: &'m [crate::ast::Import],
    package: &'m crate::pass::resolve::Package<P>,
) where
    P: crate::pass::resolve::ResolvePhase,
{
    for u in imports {
        // Compiler pseudo-module imports are handled elsewhere. `import`
        // statements route here; they target same-package modules.
        match &u.kind {
            crate::ast::ImportKind::Selective { items, from } => {
                let from_path = from
                    .segments
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join("/");
                let Some(target) = package.module(&from_path) else {
                    continue;
                };
                for name in items.iter().filter_map(crate::ast::ImportItem::as_name) {
                    #[cfg(all(test, feature = "surface"))]
                    record_nominal_provider_item_visit();
                    if let Some(declaration) = target
                        .scope
                        .lookup(name)
                        .and_then(|id| crate::pass::resolve::declaration_by_id(&target.module, id))
                    {
                        if let Some(d) = declaration.fn_def().filter(|d| d.name == name) {
                            env.cross_module_fn_defs.insert(name, d);
                            env.cross_module_fn_def_modules.insert(name, &target.module);
                        } else if let Some(a) = declaration.type_alias().filter(|a| a.name == name)
                        {
                            env.cross_module_type_aliases.insert(
                                name,
                                crate::pass::typecheck_core::AliasDef {
                                    type_params: &a.type_params,
                                    body: a.type_body(),
                                    owner_module: Some(&target.module),
                                },
                            );
                        } else if let Some(d) = declaration.newtype().filter(|d| d.name == name) {
                            env.cross_module_newtypes
                                .insert((from_path.clone(), name.to_owned()), d);
                            env.cross_module_newtype_modules
                                .insert((from_path.clone(), name.to_owned()), &target.module);
                        } else if let Some(s) = declaration.elaborator().filter(|s| s.name == name)
                        {
                            env.cross_module_user_elaborators.insert(name, s);
                            env.cross_module_user_elaborator_modules
                                .insert(name, &target.module);
                        // Host items are ordinary public declarations, so a
                        // selective `import` brings them into scope like any
                        // other item. Their host-boundary provenance is
                        // recovered by module name-scan in `recover_to_low`,
                        // so resolving the name here suffices. Mirrors the
                        // local items-loop handling above.
                        } else if let Some(h) = declaration.host_type().filter(|h| h.name == name) {
                            env.env_type_names.insert(name);
                            if let Some(role) = h.role {
                                env.host_env_roles.push((role.role, name));
                            }
                        } else if let Some(h) = declaration.host_fn().filter(|h| h.name == name) {
                            env.env_fn_defs.insert(name, h);
                            env.cross_module_env_fn_modules.insert(name, &target.module);
                        }
                    }
                }
            }
            crate::ast::ImportKind::Qualified { path, alias } => {
                let from_path = path
                    .segments
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join("/");
                if let Some(target) = package.module(&from_path) {
                    env.qualified_imports.insert(alias.as_str(), &target.module);
                }
            }
            _ => {}
        }
    }
    rebuild_singleton_roles_in_scope(env);
}

/// Record every `type` alias (local or imported) whose body resolves —
/// possibly through a chain of aliases — to a `role(...)`-bearing host type.
/// The spelling-indexed [`ModuleEnv::alias_roles`] map serves exact bare
/// annotation lookups; the origin-preserving candidate list serves fallback
/// and singleton resolution at a particular source position. This
/// is what makes a host type rebound onto an alias (`type I32 =
/// provider.I32`, the local form a `rehost` materializes) usable as a
/// literal annotation (`42(I32)`) and as an expected / fallback literal
/// type, the same way the original `host type I32 role(i32)` was. Runs
/// after every host type and every alias is registered, so the role lookup
/// sees the complete scope.
///
/// Deliberately populates `alias_roles`, **not** `host_env_roles`: an
/// alias and the host type it names are the same type, so adding the alias
/// to the tier-3 bare-literal candidate pool alongside the host type would
/// spuriously flag a bare literal as ambiguous (a `type Flag =
/// Comptime_bool;` alongside `Comptime_bool` must not make `true` two-way
/// ambiguous). Tier 3 falls back to this map only when no host type
/// carries the role — see `synth.rs`.
///
/// Only an alias that genuinely resolves to a role-bearing host type is
/// recorded: an alias to a compound, to a non-role host type, or to a type
/// parameter contributes nothing, so a `type Pair = (A, B)` is never
/// recorded.
fn register_alias_inherited_roles<'m, P>(
    env: &mut ModuleEnv<'m, P>,
    module: &'m crate::ast::Module<P>,
) where
    P: crate::pass::resolve::ResolvePhase,
{
    let provider = NominalProvider::new(Some(module), env.package);
    // Resolve each alias's role with the scope read-only, then record the
    // hits — the resolution borrows the env's maps immutably, so the
    // mutation is deferred to a second step.
    let mut inherited = Vec::new();
    for (item_index, item) in module.items.iter().enumerate() {
        if let crate::ast::Item::TypeAlias(alias) = item
            && let Some(binding) = monomorphic_alias_inherited_role(
                &alias.type_params,
                alias.type_body(),
                provider.root(),
                &provider,
                &mut Vec::new(),
            )
        {
            inherited.push(AliasRoleCandidate {
                visible_name: alias.name.as_str(),
                role: binding.role,
                host_identity: binding.host_identity,
                local_item_index: Some(item_index),
            });
        }
    }
    for (name, alias) in &env.cross_module_type_aliases {
        // A cross-module alias resolves in its *owner* module — its body
        // names the owner's imports / host types, not the consumer's.
        let owner = provider.scope_for_owner(alias.owner_module.or(Some(module)));
        if let Some(binding) = monomorphic_alias_inherited_role(
            alias.type_params,
            alias.body,
            owner,
            &provider,
            &mut Vec::new(),
        ) {
            inherited.push(AliasRoleCandidate {
                visible_name: name,
                role: binding.role,
                host_identity: binding.host_identity,
                local_item_index: None,
            });
        }
    }
    for candidate in inherited {
        env.alias_roles
            .insert(candidate.visible_name, candidate.role);
        env.alias_role_candidates.push(candidate);
    }
}

fn rebuild_singleton_roles_in_scope<P>(env: &mut ModuleEnv<'_, P>)
where
    P: crate::ast::Phase,
{
    let mut roles = env
        .host_env_roles
        .iter()
        .map(|(role, _)| *role)
        .chain(
            env.alias_role_candidates
                .iter()
                .map(|candidate| candidate.role),
        )
        .collect::<Vec<_>>();
    roles.sort_unstable_by_key(|role| role.as_str());
    roles.dedup();
    let singletons = roles
        .into_iter()
        .filter_map(|role| match env.resolve_exact_role(role) {
            RoleResolution::Unique(name) => Some((role, name)),
            RoleResolution::Missing | RoleResolution::Ambiguous { .. } => None,
        })
        .collect::<Vec<_>>();
    env.roles_in_scope.clear();
    env.roles_in_scope.extend(singletons);
}

/// Resolve a type-alias body to the [`crate::ast::Role`] it inherits, if
/// any. `owner` is the module the body's names resolve in (the alias's
/// declaring module). `seen` guards against an alias chain that re-enters
/// itself by module/name identity (`type A = m.A`), terminating the walk
/// with no role rather than looping.
///
/// Recognizes the shapes a rebound host type produces — a bare alias head
/// (`type J = I32`) chaining to a role-bearing host type, and a
/// qualified head (`type I32 = provider.I32`) naming a host type or
/// role-inheriting alias through an `import <module> as provider;` qualified
/// import — plus their transitive composition. A body that is a compound,
/// carries type arguments, or resolves to a non-role host type yields
/// `None`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct InheritedRole {
    role: crate::ast::Role,
    host_identity: (String, String),
}

fn monomorphic_alias_inherited_role<'m, P>(
    type_params: &[crate::ast::TypeParam],
    body: &crate::ast::Type<P>,
    owner: crate::pass::resolve::NominalScope<'m, P>,
    provider: &NominalProvider<'m, P>,
    seen: &mut Vec<(usize, u64)>,
) -> Option<InheritedRole>
where
    P: crate::ast::Phase,
{
    if !type_params.is_empty() {
        return None;
    }
    alias_inherited_role(body, owner, provider, seen)
}

fn alias_inherited_role<'m, P>(
    body: &crate::ast::Type<P>,
    owner: crate::pass::resolve::NominalScope<'m, P>,
    provider: &NominalProvider<'m, P>,
    seen: &mut Vec<(usize, u64)>,
) -> Option<InheritedRole>
where
    P: crate::ast::Phase,
{
    let crate::ast::Type::Path { segments, args, .. } = body else {
        return None;
    };
    if !args.is_empty() {
        return None;
    }
    let declaration = match provider.select(owner, segments, false) {
        NominalSelection::Selected(declaration) => declaration,
        NominalSelection::Missing | NominalSelection::Opaque => return None,
    };
    if let Some(host) = declaration.declaration.host_type() {
        Some(InheritedRole {
            role: host.role?.role,
            host_identity: (
                module_path_key(declaration.owner.module()?, None),
                host.name.clone(),
            ),
        })
    } else if let Some(alias) = declaration.declaration.type_alias() {
        let owner_key = declaration.owner.module()? as *const crate::ast::Module<P> as usize;
        let key = (owner_key, declaration.id.0);
        if seen.contains(&key) {
            return None;
        }
        seen.push(key);
        let result = monomorphic_alias_inherited_role(
            &alias.type_params,
            alias.type_body(),
            declaration.owner,
            provider,
            seen,
        );
        seen.pop();
        result
    } else {
        None
    }
}

// =========================================================================
// Per-fn local context
// =========================================================================

/// Process-local proof identity for one lexical type binder.
///
/// This capability is deliberately opaque and never reaches a serialized
/// phase artifact. Its numeric representation is not diagnostic information.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct TypeBinderId(u64);

impl fmt::Debug for TypeBinderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TypeBinderId(<opaque>)")
    }
}

impl TypeBinderId {
    #[cfg(all(test, feature = "surface"))]
    pub(crate) const fn for_test(value: u64) -> Self {
        Self(value)
    }
}

/// Opaque pairing of one explicit source binder with its transient proof.
///
/// Retained planner states store this value rather than zipping an independent
/// parameter list with independent IDs on every retry.
#[cfg(feature = "surface")]
#[derive(Clone, Debug)]
pub(crate) struct RetainedTypeBinder<'m> {
    id: TypeBinderId,
    param: Cow<'m, crate::ast::TypeParam>,
}

#[cfg(feature = "surface")]
impl RetainedTypeBinder<'_> {
    pub(crate) fn id(&self) -> TypeBinderId {
        self.id
    }

    pub(crate) fn param(&self) -> &crate::ast::TypeParam {
        self.param.as_ref()
    }
}

static NEXT_TYPE_BINDER_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_TYPE_CONTEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Opaque identity of one live type-checking context.
///
/// Goal inference binds its transient capabilities to this identity so a
/// caller cannot reinterpret a scoped bare name through another module or
/// package-analysis environment.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct TypeContextId(u64);

impl TypeContextId {
    #[cfg(feature = "surface")]
    pub(crate) const fn isolated_specialization() -> Self {
        Self(u64::MAX)
    }
}

impl fmt::Debug for TypeContextId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TypeContextId(<opaque>)")
    }
}

/// One entry on the local-binding stack used during expression
/// type-checking. Entries are pushed when a `fn`, `type`,
/// `newtype`, or embedded `Forall` introduces a binder and popped when the body's walk
/// finishes (the typer uses a save/restore mark to scope blocks).
#[derive(Debug, Clone)]
pub enum Local<'m, P>
where
    P: crate::ast::Phase,
{
    /// A type parameter `[A]` / `[*F]` introduced by a `fn`,
    /// `type`, or `newtype`. Treated as a rigid type variable;
    /// equality is by name. Carries the binder's kind (the default
    /// `*` for an unannotated binder) so the kind-checking pass can
    /// resolve `F(A)` against `F`'s declared kind. `binder_id` is the
    /// phase-local proof that distinguishes shadowed or reconstructed binders
    /// independently of their spelling and source span.
    TypeParam {
        name: Cow<'m, str>,
        kind: crate::ast::Kind,
        decl_span: Span,
        binder_id: TypeBinderId,
    },
    /// A value parameter `name: T` or a `let` local with a known type.
    Value {
        name: String,
        ty: super::InternedType<P>,
        decl_span: Option<Span>,
        #[cfg(feature = "surface")]
        retained_type: Option<Arc<RetainedLocalType>>,
    },
    PendingRecOrder {
        name: String,
        source: &'m crate::ast::Expr<P>,
        state: std::cell::RefCell<PendingRecOrderState<P>>,
        span: Span,
        decl_span: Option<Span>,
    },
}

/// Exact scope authority retained by an unfinished ordinary lambda parameter.
/// The opaque payload is captured with the resolved local, not recovered from
/// its spelling after the surrounding inference cursor resumes.
#[cfg(feature = "surface")]
#[derive(Debug)]
pub struct RetainedLocalType {
    context: TypeContextId,
    owner: crate::ast::TypeGoalOwner,
    ty: super::ScopedType,
}

#[cfg(all(test, feature = "surface"))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct RetainedLocalWork {
    pub allocations: usize,
    pub captures: usize,
    pub consumes: usize,
    pub open_forall_consumes: usize,
    pub lookup_entries: usize,
    pub proof_slots: usize,
}

#[cfg(all(test, feature = "surface"))]
thread_local! {
    static RETAINED_LOCAL_WORK: std::cell::Cell<RetainedLocalWork> = const {
        std::cell::Cell::new(RetainedLocalWork {
            allocations: 0, captures: 0, consumes: 0, open_forall_consumes: 0, lookup_entries: 0, proof_slots: 0,
        })
    };
}

#[cfg(all(test, feature = "surface"))]
pub(crate) fn reset_retained_local_work() {
    RETAINED_LOCAL_WORK.with(|work| work.set(RetainedLocalWork::default()));
}

#[cfg(all(test, feature = "surface"))]
pub(crate) fn retained_local_work() -> RetainedLocalWork {
    RETAINED_LOCAL_WORK.with(std::cell::Cell::get)
}

#[cfg(all(test, feature = "surface"))]
fn record_retained_local_work(update: impl FnOnce(&mut RetainedLocalWork)) {
    RETAINED_LOCAL_WORK.with(|work| {
        let mut value = work.get();
        update(&mut value);
        work.set(value);
    });
}

#[cfg(feature = "surface")]
impl RetainedLocalType {
    pub(crate) fn at_owner(
        &self,
        owner: crate::ast::TypeGoalOwner,
        store: &super::GoalStore,
        raw: &super::InternedType<crate::ast::Lowered>,
        span: Span,
        tcx: &TypeCtx<'_, '_, crate::ast::Lowered>,
    ) -> Result<super::ScopedType, Error> {
        assert_eq!(
            tcx.context_id(),
            self.context,
            "a retained local crossed type contexts"
        );
        assert!(
            raw.ptr_eq(self.ty.ty()),
            "a retained local lost its exact resolved type"
        );
        let ty = store.use_retained_type_at(self.owner, owner, self.ty.clone(), span)?;
        #[cfg(test)]
        record_retained_local_work(|work| {
            work.consumes += 1;
            if super::contains_goal_beneath_forall(raw.as_type(), false) {
                work.open_forall_consumes += 1;
            }
        });
        Ok(ty)
    }
}

#[cfg(feature = "surface")]
impl TypeCtx<'_, '_, crate::ast::Lowered> {
    pub(crate) fn push_retained_value(
        &mut self,
        name: String,
        owner: crate::ast::TypeGoalOwner,
        ty: super::ScopedType,
    ) {
        let raw = ty.ty().clone();
        #[cfg(test)]
        record_retained_local_work(|work| work.allocations += 1);
        let retained_type = Arc::new(RetainedLocalType {
            context: self.context_id(),
            owner,
            ty,
        });
        self.locals.push(Local::Value {
            name,
            ty: raw,
            decl_span: None,
            retained_type: Some(retained_type),
        });
        self.bump_local_revision();
    }

    pub(crate) fn capture_retained_local_type(
        &self,
        index: usize,
    ) -> Option<Arc<RetainedLocalType>> {
        #[cfg(test)]
        record_retained_local_work(|work| work.proof_slots += 1);
        match &self.locals[index] {
            Local::Value { retained_type, .. } => {
                #[cfg(test)]
                if retained_type.is_some() {
                    record_retained_local_work(|work| work.captures += 1);
                }
                retained_type.clone()
            }
            Local::PendingRecOrder { .. } => None,
            Local::TypeParam { .. } => unreachable!("value lookup selected a type parameter"),
        }
    }
}

#[derive(Debug)]
pub enum PendingRecOrderState<P>
where
    P: crate::ast::Phase,
{
    Pending,
    Runtime {
        exact_ty: super::InternedType<P>,
        source_checked: bool,
    },
    TypeOnly,
}

/// Preflighted, exact-local recursive-order mutations. Construction finds the
/// live local by source identity without changing it; commit rechecks that
/// identity and performs only infallible `RefCell` replacements.
#[derive(Debug)]
pub(crate) struct PreparedRecOrderCommit<'m, P>
where
    P: crate::ast::Phase,
{
    context_id: TypeContextId,
    local_revision: u64,
    updates: Vec<PreparedRecOrderUpdate<'m, P>>,
}

#[derive(Debug)]
struct PreparedRecOrderUpdate<'m, P>
where
    P: crate::ast::Phase,
{
    local_index: usize,
    source: &'m crate::ast::Expr<P>,
    state: PendingRecOrderState<P>,
}

impl<P: crate::ast::Phase> Clone for PendingRecOrderState<P> {
    fn clone(&self) -> Self {
        match self {
            Self::Pending => Self::Pending,
            Self::Runtime {
                exact_ty,
                source_checked,
            } => Self::Runtime {
                exact_ty: exact_ty.clone(),
                source_checked: *source_checked,
            },
            Self::TypeOnly => Self::TypeOnly,
        }
    }
}

/// Per-fn type-checking context: the per-module env, a stack of
/// in-scope locals, and the phase-specific elaboration side-channel.
///
/// At `P = Lowered` the `elaborations` field is the real
/// `typecheck_full::Elaborations` table recorded for surface
/// replacement-bearing forms.
/// At `P = Prime` surface elaboration variants are uninhabited, while the
/// narrower `PrimeElaborations` table records only call/lambda completions to
/// bake into the checked AST.
pub struct TypeCtx<'m, 'e, P>
where
    P: TyperPhase,
{
    context_id: std::cell::Cell<Option<TypeContextId>>,
    local_revision: std::cell::Cell<u64>,
    pub env: &'e ModuleEnv<'m, P>,
    /// Stack of locals, innermost last. Lookup walks from end to start.
    pub locals: Vec<Local<'m, P>>,
    /// Earliest recursive-ordering local in `locals`. `None` keeps ordinary
    /// and Prime expression checking on the pre-carrier fast path.
    pending_rec_order_start: Option<usize>,
    /// Phase-specific elaboration table. See [`TyperPhase::Elaborations`].
    pub elaborations: &'e mut P::Elaborations,
    pub pure_context: bool,
    item_index: Option<usize>,
    next_application_hole: usize,
}

impl<'m, 'e, P> TypeCtx<'m, 'e, P>
where
    P: TyperPhase,
{
    pub fn new(env: &'e ModuleEnv<'m, P>, elaborations: &'e mut P::Elaborations) -> Self {
        TypeCtx {
            context_id: std::cell::Cell::new(None),
            local_revision: std::cell::Cell::new(0),
            env,
            locals: Vec::new(),
            pending_rec_order_start: None,
            elaborations,
            pure_context: false,
            item_index: None,
            next_application_hole: 0,
        }
    }

    pub(crate) fn context_id(&self) -> TypeContextId {
        if let Some(context_id) = self.context_id.get() {
            return context_id;
        }
        let context_id = NEXT_TYPE_CONTEXT_ID
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
                (next < u64::MAX).then_some(next + 1)
            })
            .map(TypeContextId)
            .expect("process exhausted unique type-checking context identities");
        self.context_id.set(Some(context_id));
        context_id
    }

    #[cfg(all(test, feature = "surface"))]
    pub(crate) fn has_context_id(&self) -> bool {
        self.context_id.get().is_some()
    }

    #[cfg(all(test, feature = "surface"))]
    pub(crate) fn planning_state_fingerprint(&self) -> (bool, u64, usize, usize) {
        (
            self.context_id.get().is_some(),
            self.local_revision.get(),
            self.locals.len(),
            self.next_application_hole,
        )
    }

    fn next_local_revision(&self) -> u64 {
        self.local_revision
            .get()
            .checked_add(1)
            .expect("type-checking local revision overflowed")
    }

    fn bump_local_revision(&self) {
        self.local_revision.set(self.next_local_revision());
    }

    pub(crate) fn fresh_application_hole_name(&mut self) -> String {
        let id = self.next_application_hole;
        self.next_application_hole += 1;
        format!("\0expected{id}")
    }

    pub fn at_item(mut self, item_index: usize) -> Self {
        self.item_index = Some(item_index);
        self
    }

    pub(crate) fn local_item_cutoff(&self) -> Option<usize> {
        self.item_index
    }

    pub fn resolve_exact_role(&self, role: crate::ast::Role) -> RoleResolution<'m> {
        self.env.resolve_exact_role_at(role, self.item_index)
    }

    pub fn describe_exact_role_ambiguity(
        &self,
        role: crate::ast::Role,
        first: &str,
        second: &str,
    ) -> String {
        self.env
            .describe_exact_role_ambiguity(role, first, second, self.item_index)
    }

    pub fn require_pure_fn(
        &self,
        span: Span,
        kind: &str,
        name: &str,
        purity: &P::FnPurity,
    ) -> Result<(), Error> {
        if self.pure_context && !purity.is_pure_fn() {
            return Err(Error::type_(
                span,
                format!(
                    "pure function cannot reference unrestricted {kind} `{name}`; every ordinary function in its call graph must be declared `pure`"
                ),
            ));
        }
        Ok(())
    }

    pub fn reject_pure_external(&self, span: Span, kind: &str, name: &str) -> Result<(), Error> {
        if self.pure_context {
            return Err(Error::type_(
                span,
                format!(
                    "pure function cannot reference {kind} `{name}`; `pure` means that its call graph contains no host-function calls"
                ),
            ));
        }
        Ok(())
    }

    /// Push a kind-`*` type parameter. Most binders are kind-`*`
    /// (ordinary type variables); higher-kinded binders go through
    /// [`Self::push_type_param_kinded`].
    pub fn push_type_param(&mut self, name: &'m str, decl_span: Span) {
        let binder_id = self.fresh_type_binder_id();
        self.locals.push(Local::TypeParam {
            name: Cow::Borrowed(name),
            kind: crate::ast::Kind::Star,
            decl_span,
            binder_id,
        });
        self.bump_local_revision();
    }

    /// Push a type parameter carrying its declared kind. Used at the
    /// declaration sites that scope binders for the kind-checking
    /// pass (fn / type / newtype headers and embedded `Forall`s).
    pub fn push_type_param_kinded(
        &mut self,
        name: &'m str,
        kind: crate::ast::Kind,
        decl_span: Span,
    ) {
        let binder_id = self.fresh_type_binder_id();
        self.locals.push(Local::TypeParam {
            name: Cow::Borrowed(name),
            kind: kind.clone(),
            decl_span,
            binder_id,
        });
        self.bump_local_revision();
    }

    /// Push a kinded binder whose declaration is owned by a transient plan or
    /// mechanical publication walk rather than borrowed for the module
    /// lifetime.
    pub(crate) fn push_owned_type_param_kinded(&mut self, param: &crate::ast::TypeParam) {
        let binder_id = self.fresh_type_binder_id();
        self.locals.push(Local::TypeParam {
            name: Cow::Owned(param.name.clone()),
            kind: param.effective_kind(),
            decl_span: param.span,
            binder_id,
        });
        self.bump_local_revision();
    }

    /// Push an explicit source binder and atomically retain its retry proof.
    ///
    /// Keeping allocation, local insertion, and proof construction together
    /// prevents a planner from zipping a reconstructed parameter with another
    /// same-shaped binder's opaque identity.
    #[cfg(feature = "surface")]
    pub(crate) fn push_retained_type_param(
        &mut self,
        param: &'m crate::ast::TypeParam,
    ) -> RetainedTypeBinder<'m> {
        let binder = RetainedTypeBinder {
            id: self.fresh_type_binder_id(),
            param: Cow::Borrowed(param),
        };
        self.push_retained_type_binder(&binder);
        binder
    }

    /// Retain a binder reconstructed from an unfolded type rather than
    /// borrowed from the alpha-normalized source tree.
    #[cfg(feature = "surface")]
    pub(crate) fn push_owned_retained_type_param(
        &mut self,
        param: &crate::ast::TypeParam,
    ) -> RetainedTypeBinder<'m> {
        let binder = RetainedTypeBinder {
            id: self.fresh_type_binder_id(),
            param: Cow::Owned(param.clone()),
        };
        self.push_retained_type_binder(&binder);
        binder
    }

    /// Restore one previously allocated lexical binder proof.
    ///
    /// Retained planner states call this when they re-enter the same normalized
    /// signature. The explicit declaration arguments prevent a proof from
    /// silently being reused for a different binder.
    #[cfg(feature = "surface")]
    pub(crate) fn push_retained_type_binder(&mut self, binder: &RetainedTypeBinder<'m>) {
        let param = binder.param();
        let name = match &binder.param {
            Cow::Borrowed(param) => Cow::Borrowed(param.name.as_str()),
            Cow::Owned(param) => Cow::Owned(param.name.clone()),
        };
        self.locals.push(Local::TypeParam {
            name,
            kind: param.effective_kind(),
            decl_span: param.span,
            binder_id: binder.id,
        });
        self.bump_local_revision();
    }

    fn fresh_type_binder_id(&self) -> TypeBinderId {
        TypeBinderId(
            NEXT_TYPE_BINDER_ID
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
                    next.checked_add(1)
                })
                .expect("process exhausted rigid-binder identities"),
        )
    }

    /// Every lexical type-parameter binding currently on the local stack,
    /// including same-spelled shadowed entries, in outer-to-inner order.
    ///
    /// The iterator borrows the context, performs no allocation or proof
    /// minting, and is double-ended so exact ambient-scope consumers can share
    /// this one enumeration without reconstructing names or binder identity.
    pub(crate) fn in_scope_type_param_bindings(
        &self,
    ) -> impl DoubleEndedIterator<Item = (&str, &crate::ast::Kind, TypeBinderId)> + '_ {
        self.locals.iter().filter_map(|local| match local {
            Local::TypeParam {
                name,
                kind,
                binder_id,
                ..
            } => Some((name.as_ref(), kind, *binder_id)),
            _ => None,
        })
    }

    /// The kind of an in-scope type parameter, or `None` if `name`
    /// is not a bound type parameter.
    pub fn type_param_kind(&self, name: &str) -> Option<crate::ast::Kind> {
        self.in_scope_type_param_bindings()
            .rev()
            .find_map(|(bound, kind, _)| (bound == name).then(|| kind.clone()))
    }

    pub fn push_value(&mut self, name: String, ty: crate::ast::Type<P>)
    where
        P: Clone,
    {
        let ty = self.intern_type(&ty);
        self.push_value_interned(name, ty);
    }

    pub fn push_value_interned(&mut self, name: String, ty: super::InternedType<P>) {
        self.locals.push(Local::Value {
            name,
            ty,
            decl_span: None,
            #[cfg(feature = "surface")]
            retained_type: None,
        });
        self.bump_local_revision();
    }

    pub fn intern_type(&self, ty: &crate::ast::Type<P>) -> super::InternedType<P>
    where
        P: Clone,
    {
        self.env.type_interner.intern(ty)
    }

    pub fn push_pending_rec_order(
        &mut self,
        name: String,
        source: &'m crate::ast::Expr<P>,
        span: Span,
    ) -> usize {
        let index = self.locals.len();
        self.pending_rec_order_start.get_or_insert(index);
        self.locals.push(Local::PendingRecOrder {
            name,
            source,
            state: std::cell::RefCell::new(PendingRecOrderState::Pending),
            span,
            decl_span: None,
        });
        self.bump_local_revision();
        index
    }

    pub fn pending_rec_order_state(&self, index: usize) -> PendingRecOrderState<P> {
        let Local::PendingRecOrder { state, .. } = &self.locals[index] else {
            unreachable!("recursive-ordering local index did not name its pending binding")
        };
        state.borrow().clone()
    }

    pub fn has_pending_rec_order(&self) -> bool {
        self.pending_rec_order_start.is_some()
    }

    fn pending_rec_order_local_index(&self, source: &'m crate::ast::Expr<P>) -> Option<usize> {
        let start = self.pending_rec_order_start?;
        self.locals[start..]
            .iter()
            .enumerate()
            .rev()
            .find_map(|(offset, local)| match local {
                Local::PendingRecOrder {
                    source: pending, ..
                } if std::ptr::eq(*pending, source) => Some(start + offset),
                _ => None,
            })
    }

    #[cfg(feature = "surface")]
    pub(crate) fn is_pending_rec_order_source(&self, source: &'m crate::ast::Expr<P>) -> bool {
        self.pending_rec_order_local_index(source).is_some()
    }

    pub fn effective_rec_order_source(
        &self,
        expr: &'m crate::ast::Expr<P>,
    ) -> &'m crate::ast::Expr<P> {
        let Some(start) = self.pending_rec_order_start else {
            return expr;
        };
        let crate::ast::Expr::Path { segments, .. } = expr else {
            return expr;
        };
        let [segment] = segments.as_slice() else {
            return expr;
        };
        for local in self.locals[start..].iter().rev() {
            match local {
                Local::PendingRecOrder { name, source, .. } if name == segment.as_str() => {
                    return source;
                }
                Local::Value { name, .. } if name == segment.as_str() => {
                    return expr;
                }
                _ => {}
            }
        }
        expr
    }

    pub fn is_pending_rec_order_path(&self, expr: &'m crate::ast::Expr<P>) -> bool {
        !std::ptr::eq(expr, self.effective_rec_order_source(expr))
    }

    pub fn finish_rec_order_source(
        &self,
        source: &'m crate::ast::Expr<P>,
        state: PendingRecOrderState<P>,
    ) {
        let Some(start) = self.pending_rec_order_start else {
            return;
        };
        for local in self.locals[start..].iter().rev() {
            if let Local::PendingRecOrder {
                source: pending,
                state: slot,
                ..
            } = local
                && std::ptr::eq(*pending, source)
            {
                *slot.borrow_mut() = state;
                self.bump_local_revision();
                return;
            }
        }
    }

    pub(crate) fn prepare_rec_order_updates(
        &self,
        updates: Vec<(&'m crate::ast::Expr<P>, PendingRecOrderState<P>)>,
    ) -> PreparedRecOrderCommit<'m, P> {
        let context_id = self.context_id();
        let local_revision = self.local_revision.get();
        if self.pending_rec_order_start.is_none() {
            return PreparedRecOrderCommit {
                context_id,
                local_revision,
                updates: Vec::new(),
            };
        };
        let mut prepared = Vec::new();
        for (source, state) in updates {
            if let Some(local_index) = self.pending_rec_order_local_index(source) {
                prepared.push(PreparedRecOrderUpdate {
                    local_index,
                    source,
                    state,
                });
            }
        }
        PreparedRecOrderCommit {
            context_id,
            local_revision,
            updates: prepared,
        }
    }

    pub(crate) fn validate_prepared_rec_order(&self, prepared: &PreparedRecOrderCommit<'m, P>) {
        assert_eq!(
            self.context_id.get(),
            Some(prepared.context_id),
            "prepared recursive-order update belongs to another type-checking context"
        );
        assert_eq!(
            prepared.local_revision,
            self.local_revision.get(),
            "prepared recursive-order update became stale before commit"
        );
        if !prepared.updates.is_empty() {
            let _ = self.next_local_revision();
        }
        for update in &prepared.updates {
            let Local::PendingRecOrder { source, state, .. } = self
                .locals
                .get(update.local_index)
                .expect("preflighted recursive-order local disappeared before commit")
            else {
                panic!("preflighted recursive-order local changed kind before commit")
            };
            assert!(
                std::ptr::eq(*source, update.source),
                "preflighted recursive-order update targeted a different live local"
            );
            let _ = state
                .try_borrow_mut()
                .expect("prepared recursive-order local was already mutably borrowed");
        }
    }

    pub(crate) fn commit_prepared_rec_order(&self, prepared: PreparedRecOrderCommit<'m, P>) {
        self.validate_prepared_rec_order(&prepared);
        let next_revision = (!prepared.updates.is_empty()).then(|| self.next_local_revision());
        for update in prepared.updates {
            let Local::PendingRecOrder { state, .. } = &self.locals[update.local_index] else {
                unreachable!("validated recursive-order local changed before commit")
            };
            *state.borrow_mut() = update.state;
        }
        if let Some(next_revision) = next_revision {
            self.local_revision.set(next_revision);
        }
    }

    pub fn save(&self) -> usize {
        self.locals.len()
    }

    pub fn restore(&mut self, mark: usize) {
        let changed = self.locals.len() != mark;
        if self
            .pending_rec_order_start
            .is_some_and(|start| start >= mark)
        {
            self.pending_rec_order_start = None;
        }
        self.locals.truncate(mark);
        if changed {
            self.bump_local_revision();
        }
    }

    /// Returns true if `name` is a type-parameter currently in scope.
    pub fn is_type_param(&self, name: &str) -> bool {
        self.in_scope_type_param_bindings()
            .any(|(bound, _, _)| bound == name)
    }

    /// The names of every scheme type-parameter binder currently in
    /// scope (`fn` / `type` / `newtype` / `fn`-expr / `equiv` headers and
    /// embedded `Forall`s). These are rigid type variables: a bare
    /// nominal head naming one is *not* a nominal that head
    /// canonicalization may qualify to a same-named module type, so the
    /// set is fed to [`Self::alias_ctx`] / [`Self::binder_alias_ctx`] as
    /// [`AliasCtx::binder_locals`]. Owned (the locals stack mutates as
    /// the body is walked); the caller holds it across the comparison
    /// the ctx serves.
    pub fn in_scope_type_param_binders(&self) -> HashSet<String> {
        self.in_scope_type_param_bindings()
            .map(|(name, _, _)| name.to_owned())
            .collect()
    }

    #[cfg(feature = "surface")]
    pub(crate) fn in_scope_type_param_presentation(
        &self,
    ) -> crate::pass::binder_presentation::TypeBinderScope {
        crate::pass::binder_presentation::TypeBinderScope(
            self.locals
                .iter()
                .filter_map(|local| match local {
                    Local::TypeParam {
                        name, decl_span, ..
                    } => Some((name.to_string(), Some(*decl_span))),
                    _ => None,
                })
                .collect(),
        )
    }

    /// The module env's [`AliasCtx`] re-scoped with the in-scope scheme
    /// binders (`binders`) so head canonicalization leaves a bare binder
    /// bare. Use this — not the bare `env.alias_ctx()` — at every typer
    /// comparison site that may see a generic `[A]` shadowing a
    /// same-named module type (a label-generated `A`, a `newtype A`, a
    /// `type A`); otherwise the binder is mis-qualified to that concrete
    /// type and the unification / equivalence arm that should treat it as
    /// a variable never fires. Compute `binders` once with
    /// [`Self::in_scope_type_param_binders`] and hold it across the call.
    pub fn binder_alias_ctx<'b>(&'b self, binders: &'b HashSet<String>) -> AliasCtx<'b, 'm, P> {
        AliasCtx {
            binder_locals: Some(binders),
            ..self.env.alias_ctx()
        }
    }

    /// Look up a value binding (parameter or let-local) by name.
    pub fn lookup_value(&self, name: &str) -> Option<super::InternedType<P>> {
        self.lookup_value_binding(name).map(|(ty, _)| ty)
    }

    pub fn lookup_value_binding(
        &self,
        name: &str,
    ) -> Option<(super::InternedType<P>, Option<Span>)> {
        self.lookup_value_binding_with_index(name)
            .map(|(_, ty, span)| (ty, span))
    }

    pub(crate) fn lookup_value_binding_with_index(
        &self,
        name: &str,
    ) -> Option<(usize, super::InternedType<P>, Option<Span>)> {
        for (index, l) in self.locals.iter().enumerate().rev() {
            #[cfg(all(test, feature = "surface"))]
            record_retained_local_work(|work| work.lookup_entries += 1);
            match l {
                Local::Value {
                    name: n,
                    ty,
                    decl_span,
                    ..
                } if n == name => return Some((index, ty.clone(), *decl_span)),
                Local::PendingRecOrder {
                    name: n,
                    state,
                    decl_span,
                    ..
                } if n == name => {
                    return match &*state.borrow() {
                        PendingRecOrderState::Runtime { exact_ty, .. } => {
                            Some((index, exact_ty.clone(), *decl_span))
                        }
                        PendingRecOrderState::Pending | PendingRecOrderState::TypeOnly => None,
                    };
                }
                _ => {}
            }
        }
        None
    }

    pub fn lookup_pending_rec_order(&self, name: &str) -> Option<Span> {
        for l in self.locals.iter().rev() {
            match l {
                Local::Value { name: n, .. } if n == name => return None,
                Local::PendingRecOrder {
                    name: n,
                    state,
                    span,
                    ..
                } if n == name => {
                    return if matches!(*state.borrow(), PendingRecOrderState::Pending) {
                        Some(*span)
                    } else {
                        None
                    };
                }
                _ => {}
            }
        }
        None
    }

    pub fn fill_pending_rec_order_interned(&self, name: &str, ty: super::InternedType<P>) {
        for l in self.locals.iter().rev() {
            match l {
                Local::Value { name: n, .. } if n == name => return,
                Local::PendingRecOrder { name: n, state, .. } if n == name => {
                    *state.borrow_mut() = PendingRecOrderState::Runtime {
                        exact_ty: ty.clone(),
                        source_checked: false,
                    };
                    self.bump_local_revision();
                    return;
                }
                _ => {}
            }
        }
    }

    pub fn attach_decl_span_to_local(&mut self, name: &str, decl_span: Span) {
        for local in self.locals.iter_mut().rev() {
            match local {
                Local::TypeParam {
                    name: n,
                    decl_span: slot,
                    ..
                } if n == name => {
                    *slot = decl_span;
                    self.bump_local_revision();
                    return;
                }
                Local::Value {
                    name: n,
                    decl_span: slot,
                    ..
                } if n == name => {
                    *slot = Some(decl_span);
                    self.bump_local_revision();
                    return;
                }
                Local::PendingRecOrder {
                    name: n,
                    decl_span: slot,
                    ..
                } if n == name => {
                    *slot = Some(decl_span);
                    self.bump_local_revision();
                    return;
                }
                _ => {}
            }
        }
    }

    pub fn type_param_decl_span(&self, name: &str) -> Option<Span> {
        self.locals.iter().rev().find_map(|local| match local {
            Local::TypeParam {
                name: n, decl_span, ..
            } if n.as_ref() == name => Some(*decl_span),
            _ => None,
        })
    }

    pub fn value_decl_span(&self, name: &str) -> Option<Span> {
        self.locals.iter().rev().find_map(|local| match local {
            Local::Value {
                name: n, decl_span, ..
            }
            | Local::PendingRecOrder {
                name: n, decl_span, ..
            } if n == name => *decl_span,
            _ => None,
        })
    }
}

impl<P> super::aliases::AliasBinderLookup for TypeCtx<'_, '_, P>
where
    P: TyperPhase,
{
    fn contains_alias_binder(&self, name: &str) -> bool {
        self.in_scope_type_param_bindings()
            .rev()
            .any(|(bound, _, _)| bound == name)
    }

    fn for_each_alias_binder(&self, visit: &mut dyn FnMut(&str)) {
        for (name, _, _) in self.in_scope_type_param_bindings() {
            visit(name);
        }
    }
}

#[cfg(all(test, feature = "surface"))]
mod tests {
    use super::*;
    use crate::ast::Lowered;
    use std::path::{Path, PathBuf};

    fn package(sources: &[(&str, &str)]) -> crate::pass::resolve::Package<Lowered> {
        let parsed = sources
            .iter()
            .map(|(path, source)| {
                let module = crate::pass::parser::parse(source).expect("parse");
                let module = crate::pass::desugar::desugar_module(module).expect("desugar");
                (PathBuf::from(path), module)
            })
            .collect::<Vec<_>>();
        let (lowered, _) =
            crate::pass::label_elab::elaborate_package(parsed, None).expect("label elaboration");
        let package =
            crate::pass::resolve::Package::build(Path::new(""), lowered, None).expect("package");
        package.resolve_imports().expect("resolve imports");
        package
    }

    fn open_rank_n_local(
        store: &mut super::super::GoalStore,
        tcx: &TypeCtx<'_, '_, Lowered>,
    ) -> (crate::ast::TypeGoalOwner, super::super::ScopedType) {
        use super::super::goals::{GoalOrigin, GoalOwnerKind, GoalRole, GoalSolutionPolicy};
        use crate::ast::{Meta, Type, TypeParam};
        let span = Span::new(1, 2);
        let owner = store
            .begin_owner(
                None,
                store.scope_from_type_ctx(tcx),
                GoalOwnerKind::Application,
                span,
            )
            .expect("root owner");
        let goal = store
            .alloc_goal(
                owner,
                crate::ast::Kind::Star,
                GoalSolutionPolicy::Monotype,
                GoalOrigin::named(span, GoalRole::TypeArgument, "Result"),
            )
            .expect("result goal");
        let path = |name: &str| Type::synth_path(vec![name.to_owned()], Vec::new(), span);
        let forall = |name: &str, body| Type::Forall {
            param: TypeParam {
                name: name.to_owned(),
                span,
                kind: None,
            },
            body: Box::new(body),
            meta: Meta::new(span),
        };
        let scheme = store
            .scoped_type(
                owner,
                super::super::InternedType::fresh_canonical(forall(
                    "Result",
                    forall(
                        "A",
                        Type::synth_function(vec![path("A")], path("Result"), span),
                    ),
                )),
                span,
            )
            .expect("closed rank-N schema");
        let argument = store
            .scoped_type(
                owner,
                super::super::InternedType::fresh_canonical(Type::Goal {
                    goal,
                    args: Vec::new(),
                    meta: Meta::new(span),
                    ext: (),
                }),
                span,
            )
            .expect("same-owner argument");
        let ty = scheme
            .projected_public_forall_prefix_application(&[argument])
            .expect("retained public forall instantiation");
        assert!(super::super::contains_goal_beneath_forall(
            ty.ty().as_type(),
            false
        ));
        (owner, ty)
    }

    #[test]
    fn retained_local_common_lookup_reuses_the_selected_slot_without_scope_allocation() {
        let package = package(&[("main.kio", "module main;")]);
        let module = &package.module("main").expect("main").module;
        let env = ModuleEnv::build(module, None, None, Some(&package)).expect("environment");
        let mut elaborations = crate::pass::typecheck_full::Elaborations::new();
        let mut tcx = TypeCtx::new(&env, &mut elaborations);
        for index in 0..128 {
            tcx.push_value(
                format!("value{index}"),
                crate::ast::Type::Unit {
                    meta: crate::ast::Meta::new(Span::new(0, 0)),
                },
            );
        }
        reset_retained_local_work();
        for _ in 0..100 {
            let (index, _, _) = tcx
                .lookup_value_binding_with_index("value0")
                .expect("oldest local");
            assert_eq!(index, 0);
            assert!(tcx.capture_retained_local_type(index).is_none());
        }
        assert!(!tcx.has_context_id());
        let work = retained_local_work();
        assert_eq!(
            work,
            RetainedLocalWork {
                lookup_entries: 12_800,
                proof_slots: 100,
                ..RetainedLocalWork::default()
            }
        );
        eprintln!("plain local work: {work:?}");
    }

    #[test]
    fn retained_local_capture_survives_shadow_restore_without_reselection() {
        use super::super::goals::GoalOwnerKind;
        let package = package(&[("main.kio", "module main;")]);
        let module = &package.module("main").expect("main").module;
        let env = ModuleEnv::build(module, None, None, Some(&package)).expect("environment");
        let mut elaborations = crate::pass::typecheck_full::Elaborations::new();
        let mut tcx = TypeCtx::new(&env, &mut elaborations);
        let mut store = super::super::GoalStore::new();
        let (owner, ty) = open_rank_n_local(&mut store, &tcx);
        let raw = ty.ty().clone();
        let span = Span::new(1, 2);
        let child = store
            .begin_owner(
                Some(owner),
                store.scope_from_type_ctx(&tcx),
                GoalOwnerKind::RetainedValue,
                span,
            )
            .expect("child owner");
        reset_retained_local_work();
        for _ in 0..32 {
            let base = tcx.save();
            tcx.push_retained_value("step".to_owned(), owner, ty.clone());
            let (index, exact, _) = tcx.lookup_value_binding_with_index("step").unwrap();
            assert!(exact.ptr_eq(&raw));
            let captured = tcx
                .capture_retained_local_type(index)
                .expect("original proof");
            let weak = Arc::downgrade(&captured);
            let shadow = tcx.save();
            tcx.push_value_interned("step".to_owned(), raw.clone());
            let (inner_index, _, _) = tcx.lookup_value_binding_with_index("step").unwrap();
            assert!(tcx.capture_retained_local_type(inner_index).is_none());
            captured
                .at_owner(child, &store, &raw, span, &tcx)
                .expect("captured original, not shadow");
            tcx.restore(shadow);
            let (restored_index, _, _) = tcx.lookup_value_binding_with_index("step").unwrap();
            let restored = tcx.capture_retained_local_type(restored_index).unwrap();
            assert!(Arc::ptr_eq(&captured, &restored));
            drop(restored);
            tcx.restore(base);
            assert!(tcx.lookup_value_binding("step").is_none());
            captured
                .at_owner(child, &store, &raw, span, &tcx)
                .expect("prepared proof survives restore");
            drop(captured);
            assert!(weak.upgrade().is_none());
        }
        let work = retained_local_work();
        assert_eq!(
            (
                work.allocations,
                work.captures,
                work.consumes,
                work.proof_slots
            ),
            (32, 64, 64, 96)
        );
        eprintln!(
            "retained local work: {work:?}; proof bytes={}",
            std::mem::size_of::<RetainedLocalType>()
        );
    }

    #[test]
    fn retained_local_rejects_foreign_context_store_peer_and_replaced_raw_type() {
        use super::super::goals::GoalOwnerKind;
        use std::panic::{AssertUnwindSafe, catch_unwind};
        let package = package(&[("main.kio", "module main;")]);
        let module = &package.module("main").expect("main").module;
        let env = ModuleEnv::build(module, None, None, Some(&package)).expect("environment");
        let mut elaborations = crate::pass::typecheck_full::Elaborations::new();
        let mut tcx = TypeCtx::new(&env, &mut elaborations);
        let mut store = super::super::GoalStore::new();
        let (root, ty) = open_rank_n_local(&mut store, &tcx);
        let span = Span::new(1, 2);
        let child = store
            .begin_owner(
                Some(root),
                store.scope_from_type_ctx(&tcx),
                GoalOwnerKind::RetainedValue,
                span,
            )
            .unwrap();
        let peer = store
            .begin_owner(
                Some(root),
                store.scope_from_type_ctx(&tcx),
                GoalOwnerKind::RetainedValue,
                span,
            )
            .unwrap();
        let raw = ty.ty().clone();
        tcx.push_retained_value("step".to_owned(), child, ty);
        let (index, _, _) = tcx.lookup_value_binding_with_index("step").unwrap();
        let proof = tcx.capture_retained_local_type(index).unwrap();
        assert!(
            catch_unwind(AssertUnwindSafe(
                || proof.at_owner(peer, &store, &raw, span, &tcx)
            ))
            .is_err()
        );
        let replaced = super::super::InternedType::fresh_canonical(raw.as_type().clone());
        assert!(
            catch_unwind(AssertUnwindSafe(
                || proof.at_owner(child, &store, &replaced, span, &tcx)
            ))
            .is_err()
        );
        let foreign_store = super::super::GoalStore::new();
        let foreign = catch_unwind(AssertUnwindSafe(|| {
            proof.at_owner(child, &foreign_store, &raw, span, &tcx)
        }));
        assert!(foreign.is_err() || foreign.is_ok_and(|result| result.is_err()));
        let mut foreign_elaborations = crate::pass::typecheck_full::Elaborations::new();
        let foreign_tcx = TypeCtx::new(&env, &mut foreign_elaborations);
        assert!(
            catch_unwind(AssertUnwindSafe(|| proof.at_owner(
                child,
                &store,
                &raw,
                span,
                &foreign_tcx
            )))
            .is_err()
        );
    }

    #[test]
    fn type_param_binding_iterator_preserves_shadowed_exact_stack_entries() {
        let package = package(&[("main.kio", "module main;")]);
        let module = &package.module("main").expect("main module").module;
        let env = ModuleEnv::build(module, None, None, Some(&package)).expect("environment");
        let mut elaborations = crate::pass::typecheck_full::Elaborations::new();
        let mut tcx = TypeCtx::new(&env, &mut elaborations);
        let star = crate::ast::Kind::Star;
        let arrow = crate::ast::Kind::Arrow(Box::new(star.clone()), Box::new(star.clone()));

        tcx.push_type_param_kinded("A", star.clone(), Span::new(1, 2));
        tcx.push_value(
            "value".to_owned(),
            crate::ast::Type::<Lowered>::Unit {
                meta: crate::ast::Meta::new(Span::new(2, 3)),
            },
        );
        tcx.push_type_param_kinded("A", arrow.clone(), Span::new(3, 4));
        tcx.push_type_param_kinded("B", star.clone(), Span::new(4, 5));

        let revision = tcx.local_revision.get();
        assert!(
            !tcx.has_context_id(),
            "enumerating existing binders must not mint a context capability"
        );
        let (outer, last, inner) = {
            let mut bindings = tcx.in_scope_type_param_bindings();
            let outer = bindings.next().expect("outer A");
            let last = bindings.next_back().expect("inner B");
            let inner = bindings.next().expect("shadowing A");
            assert!(bindings.next().is_none(), "value locals are not binders");
            (
                (outer.0.to_owned(), outer.1.clone(), outer.2),
                (last.0.to_owned(), last.1.clone(), last.2),
                (inner.0.to_owned(), inner.1.clone(), inner.2),
            )
        };

        assert_eq!((outer.0.as_str(), &outer.1), ("A", &star));
        assert_eq!((inner.0.as_str(), &inner.1), ("A", &arrow));
        assert_eq!((last.0.as_str(), &last.1), ("B", &star));
        assert_ne!(outer.2, inner.2, "shadowed binders retain exact identity");
        assert_ne!(inner.2, last.2, "distinct binders retain exact identity");
        assert_eq!(tcx.type_param_kind("A"), Some(arrow));
        assert!(tcx.is_type_param("B"));
        assert!(!tcx.is_type_param("value"));
        assert_eq!(
            tcx.in_scope_type_param_binders(),
            HashSet::from(["A".to_owned(), "B".to_owned()]),
            "the legacy name view remains deduplicated"
        );
        assert_eq!(
            tcx.in_scope_type_param_bindings()
                .map(|(_, _, binder_id)| binder_id)
                .collect::<Vec<_>>(),
            vec![outer.2, inner.2, last.2],
            "re-enumeration preserves the already-minted proofs"
        );
        assert_eq!(
            tcx.local_revision.get(),
            revision,
            "read-only binder enumeration and lookup do not mutate TypeCtx"
        );
        assert!(
            !tcx.has_context_id(),
            "read-only binder enumeration does not mint a scope capability"
        );
    }

    #[test]
    fn selective_type_imports_reuse_the_target_declaration_provider() {
        const DECLARATIONS: usize = 64;
        const IMPORTS: usize = 48;

        let mut provider = String::from("module provider;");
        let mut imported = Vec::with_capacity(IMPORTS);
        for index in 0..DECLARATIONS {
            let name = format!("Type{index}");
            provider.push_str(&format!(" pub type {name} = .;"));
            if index < IMPORTS {
                imported.push(name);
            }
        }
        let consumer = format!("module consumer; import provider({});", imported.join(", "));
        let package = package(&[
            ("provider.kio", provider.as_str()),
            ("consumer.kio", consumer.as_str()),
        ]);
        let consumer = &package.module("consumer").expect("consumer module").module;

        reset_nominal_provider_item_visits();
        let env = ModuleEnv::build(consumer, None, None, Some(&package)).expect("environment");
        assert_eq!(env.cross_module_type_aliases.len(), IMPORTS);
        assert!(
            nominal_provider_item_visits() <= DECLARATIONS,
            "one indexed target declaration provider must serve every selective type import in O(D + U), not scan all D declarations for each of U names: {} visits",
            nominal_provider_item_visits()
        );
    }

    #[test]
    fn selective_import_projects_a_recursive_group_member() {
        let package = package(&[
            (
                "provider.kio",
                "module provider; rec { \
                 pub newtype A : B { pub constructor mk_a; pub projector un_a; }; \
                 pub newtype B : A { pub constructor mk_b; pub projector un_b; }; \
                 }",
            ),
            (
                "consumer.kio",
                "module consumer; import provider(B); type Selected = B;",
            ),
        ]);
        let consumer = &package.module("consumer").expect("consumer module").module;
        let env = ModuleEnv::build(consumer, None, None, Some(&package)).expect("environment");

        let imported = env
            .cross_module_newtype(&[crate::ast::PathSegment::new(
                "B".to_owned(),
                crate::span::Span::new(0, 0),
            )])
            .expect("selective import must project the exact group member");
        assert_eq!(imported.constructor.name, "mk_b");
    }

    #[test]
    fn qualified_import_projects_recursive_group_members() {
        let package = package(&[
            (
                "provider.kio",
                "module provider; rec { \
                 pub newtype A : B { pub constructor mk_a; pub projector un_a; }; \
                 pub newtype B : A { pub constructor mk_b; pub projector un_b; }; \
                 }",
            ),
            (
                "consumer.kio",
                "module consumer; import provider as p; \
                 type First = p.A; type Second = p.B;",
            ),
        ]);
        let consumer = &package.module("consumer").expect("consumer module").module;
        let env = ModuleEnv::build(consumer, None, None, Some(&package)).expect("environment");
        let path = |name: &str| {
            [
                crate::ast::PathSegment::new("p".to_owned(), crate::span::Span::new(0, 0)),
                crate::ast::PathSegment::new(name.to_owned(), crate::span::Span::new(0, 0)),
            ]
        };

        assert_eq!(
            env.cross_module_newtype(&path("A"))
                .expect("p.A group member")
                .constructor
                .name,
            "mk_a"
        );
        assert_eq!(
            env.cross_module_newtype(&path("B"))
                .expect("p.B group member")
                .constructor
                .name,
            "mk_b"
        );
    }

    #[test]
    fn selective_role_recovery_reuses_the_target_declaration_provider() {
        const DECLARATIONS: usize = 64;
        const IMPORTS: usize = 48;

        let mut provider = String::from("module provider;");
        let mut imported = Vec::with_capacity(IMPORTS);
        for index in 0..DECLARATIONS {
            let name = format!("Type{index}");
            provider.push_str(&format!(" pub host type {name} role(bool);"));
            if index < IMPORTS {
                imported.push(name);
            }
        }
        let consumer = format!("module consumer; import provider({});", imported.join(", "));
        let package = package(&[
            ("provider.kio", provider.as_str()),
            ("consumer.kio", consumer.as_str()),
        ]);
        let consumer = &package.module("consumer").expect("consumer module").module;
        let env = ModuleEnv::build(consumer, None, None, Some(&package)).expect("environment");

        reset_nominal_provider_item_visits();
        assert!(matches!(
            env.resolve_direct_role_candidates(|role| role == crate::ast::Role::Bool, None),
            RoleResolution::Ambiguous { .. }
        ));
        assert!(
            nominal_provider_item_visits() <= DECLARATIONS,
            "role recovery must reuse the indexed target provider in O(D + U), not scan all D declarations for each of U imports: {} visits",
            nominal_provider_item_visits()
        );
    }

    #[test]
    fn inherited_roles_reuse_the_declaration_provider() {
        const DECLARATIONS: usize = 64;
        const ALIASES: usize = 48;

        let mut provider = String::from("module provider;");
        for index in 0..DECLARATIONS {
            provider.push_str(&format!(" pub host type Type{index} role(bool);"));
        }
        let mut consumer = String::from("module consumer; import provider as p;");
        for index in 0..ALIASES {
            consumer.push_str(&format!(" type Alias{index} = p.Type{index};"));
        }
        let package = package(&[
            ("provider.kio", provider.as_str()),
            ("consumer.kio", consumer.as_str()),
        ]);
        let consumer = &package.module("consumer").expect("consumer module").module;

        reset_nominal_provider_item_visits();
        let env = ModuleEnv::build(consumer, None, None, Some(&package)).expect("environment");
        assert_eq!(env.alias_roles.len(), ALIASES);
        assert!(
            nominal_provider_item_visits() <= DECLARATIONS,
            "inherited-role lookup must project each exact provider declaration in O(D + K), not rescan all D declarations for every alias: {} visits",
            nominal_provider_item_visits()
        );
    }

    #[test]
    fn exact_role_alias_fallback_deduplicates_terminal_host_identity() {
        let package = package(&[
            (
                "pkg/provider.kio",
                "module pkg/provider; pub host type Bool role(bool);",
            ),
            (
                "pkg/adapter.kio",
                "module pkg/adapter; import pkg/provider as provider; \
                 pub type Rehost = provider.Bool;",
            ),
            (
                "pkg/consumer.kio",
                "module pkg/consumer; import pkg/adapter as adapter; \
                 type Zeta = adapter.Rehost; type Alpha = adapter.Rehost;",
            ),
        ]);
        let consumer = &package.module("pkg/consumer").expect("consumer").module;
        let env = ModuleEnv::build(consumer, None, None, Some(&package)).expect("environment");

        assert_eq!(
            env.resolve_exact_role(crate::ast::Role::Bool),
            RoleResolution::Unique("Alpha")
        );
        assert_eq!(
            env.roles_in_scope.get(&crate::ast::Role::Bool),
            Some(&"Alpha")
        );
    }

    #[test]
    fn exact_same_source_role_resolves_without_package_membership() {
        let package = package(&[(
            "standalone.kio",
            "module standalone; host type I32 role(i32);",
        )]);
        let module = &package.module("standalone").expect("standalone").module;
        let env = ModuleEnv::build(module, None, None, None).expect("standalone environment");

        assert_eq!(
            env.declared_role_for_nominal_identity("standalone", "I32"),
            Some(crate::ast::Role::I32),
            "a canonical same-source identity must resolve through the source module without a package entry"
        );
        assert_eq!(
            env.qualify_nominal_segments(&[crate::ast::PathSegment::new(
                "I32".to_owned(),
                Span::new(0, 0),
            )])
            .iter()
            .map(crate::ast::PathSegment::as_str)
            .collect::<Vec<_>>(),
            ["standalone", "I32"],
            "unique role resolution must not mark a bare standalone name as canonical"
        );
    }

    #[test]
    fn exact_role_alias_fallback_keeps_distinct_same_leaf_origins() {
        let package = package(&[
            (
                "pkg/first.kio",
                "module pkg/first; pub host type Bool role(bool);",
            ),
            (
                "pkg/second.kio",
                "module pkg/second; pub host type Bool role(bool);",
            ),
            (
                "pkg/consumer.kio",
                "module pkg/consumer; import pkg/first as first; import pkg/second as second; \
                 type Zeta = second.Bool; type Alpha = first.Bool;",
            ),
        ]);
        let consumer = &package.module("pkg/consumer").expect("consumer").module;
        let env = ModuleEnv::build(consumer, None, None, Some(&package)).expect("environment");

        assert_eq!(
            env.resolve_exact_role(crate::ast::Role::Bool),
            RoleResolution::Ambiguous {
                first: "Alpha",
                second: "Zeta",
            }
        );
        assert!(!env.roles_in_scope.contains_key(&crate::ast::Role::Bool));
    }

    #[test]
    fn alias_role_cutoff_keeps_imported_origin_separate_from_later_local_origin() {
        let package = package(&[
            (
                "pkg/first.kio",
                "module pkg/first; pub host type Bool role(bool);",
            ),
            (
                "pkg/second.kio",
                "module pkg/second; pub host type Bool role(bool);",
            ),
            (
                "pkg/provider.kio",
                "module pkg/provider; import pkg/first as first; \
                 pub type Flag = first.Bool;",
            ),
            (
                "pkg/consumer.kio",
                "module pkg/consumer; import pkg/provider(Flag); \
                 import pkg/second as second; fn earlier() -> . { () } \
                 type Flag = second.Bool;",
            ),
        ]);
        let consumer = &package.module("pkg/consumer").expect("consumer").module;
        let env = ModuleEnv::build(consumer, None, None, Some(&package)).expect("environment");

        assert_eq!(
            env.resolve_exact_role_at(crate::ast::Role::Bool, Some(0)),
            RoleResolution::Unique("Flag")
        );
        assert_eq!(
            env.resolve_exact_role(crate::ast::Role::Bool),
            RoleResolution::Ambiguous {
                first: "Flag",
                second: "Flag",
            }
        );
    }

    #[test]
    fn repeated_direct_import_of_one_identity_remains_unique() {
        let package = package(&[
            (
                "pkg/provider.kio",
                "module pkg/provider; pub host type Bool role(bool);",
            ),
            (
                "pkg/consumer.kio",
                "module pkg/consumer; import pkg/provider(Bool); \
                 import pkg/provider(Bool);",
            ),
        ]);
        let consumer = &package.module("pkg/consumer").expect("consumer").module;
        let env = ModuleEnv::build(consumer, None, None, Some(&package)).expect("environment");

        assert_eq!(
            env.resolve_exact_role(crate::ast::Role::Bool),
            RoleResolution::Unique("Bool")
        );
    }

    #[test]
    fn qualified_same_leaf_newtypes_resolve_without_flat_registry_entries() {
        let package = package(&[
            (
                "pkg/first.kio",
                "module pkg/first; pub newtype Tag : . { pub constructor first; pub projector un_first; };",
            ),
            (
                "pkg/second.kio",
                "module pkg/second; pub newtype Tag : . { pub constructor second; pub projector un_second; };",
            ),
            (
                "pkg/consumer.kio",
                "module pkg/consumer; import pkg/first as first; import pkg/second as second;",
            ),
        ]);
        let consumer = &package.module("pkg/consumer").expect("consumer").module;
        let env = ModuleEnv::build(consumer, None, None, Some(&package)).expect("environment");
        let path = |alias: &str| {
            [
                crate::ast::PathSegment::new(alias.to_owned(), crate::span::Span::new(0, 0)),
                crate::ast::PathSegment::new("Tag".to_owned(), crate::span::Span::new(0, 0)),
            ]
        };

        assert!(
            env.cross_module_newtypes.is_empty(),
            "qualified imports must not populate the bare evaluator registry"
        );
        let first = env.cross_module_newtype(&path("first")).expect("first.Tag");
        let second = env
            .cross_module_newtype(&path("second"))
            .expect("second.Tag");
        assert_eq!(first.constructor.name, "first");
        assert_eq!(second.constructor.name, "second");
        assert_eq!(
            crate::pass::typecheck_core::types::lookup_newtype_by_name(&env, &path("first"))
                .expect("lookup first.Tag")
                .constructor
                .name,
            "first"
        );
        assert_eq!(
            crate::pass::typecheck_core::types::lookup_newtype_by_name(&env, &path("second"))
                .expect("lookup second.Tag")
                .constructor
                .name,
            "second"
        );
    }

    #[test]
    fn repeated_direct_member_heads_keep_the_indexed_newtype_fast_path() {
        let package = package(&[(
            "main.kio",
            "module main; newtype Tag : . { constructor make; projector open; };",
        )]);
        let module = &package.module("main").expect("main module").module;
        let env = ModuleEnv::build(module, None, None, Some(&package)).expect("environment");
        let head = [crate::ast::PathSegment::new(
            "Tag".to_owned(),
            crate::span::Span::new(0, 0),
        )];

        reset_nominal_provider_item_visits();
        for _ in 0..1024 {
            assert_eq!(
                env.resolve_member_newtype(&head)
                    .expect("direct newtype")
                    .newtype
                    .name,
                "Tag"
            );
        }
        assert_eq!(
            nominal_provider_item_visits(),
            0,
            "direct member heads must stay on indexed module/newtype maps"
        );
    }

    #[test]
    fn repeated_identity_alias_member_heads_reuse_the_package_terminal_index() {
        const ALIASES: usize = 128;
        let mut source = String::from(
            "module main; newtype Terminal : . { constructor make; projector open; };",
        );
        for index in 0..ALIASES {
            let target = if index == 0 {
                "Terminal".to_owned()
            } else {
                format!("Alias{}", index - 1)
            };
            source.push_str(&format!(" type Alias{index} = {target};"));
        }
        let package = package(&[("main.kio", &source)]);
        let module = &package.module("main").expect("main module").module;

        crate::pass::resolve::reset_identity_alias_index_edge_visits();
        let env = ModuleEnv::build(module, None, None, Some(&package)).expect("environment");
        assert_eq!(
            crate::pass::resolve::identity_alias_index_edge_visits(),
            ALIASES
        );
        let head = [crate::ast::PathSegment::new(
            format!("Alias{}", ALIASES - 1),
            crate::span::Span::new(0, 0),
        )];
        for _ in 0..1024 {
            assert_eq!(
                env.resolve_member_newtype(&head)
                    .expect("identity alias")
                    .newtype
                    .name,
                "Terminal"
            );
        }
        assert_eq!(
            crate::pass::resolve::identity_alias_index_edge_visits(),
            ALIASES,
            "member occurrences must not retraverse the identity-alias chain"
        );
    }
}
