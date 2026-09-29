use std::collections::HashMap;
use std::fmt::Write as _;

use crate::ast::{Role, Surface};
use crate::comptime::{ComptimeBuiltin, PUBLIC_COMPTIME_NAMES};
use crate::pass::resolve::PRIME_INTRINSICS;
use crate::span::Span;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinModule {
    Intrinsics,
    Comptime,
}

impl BuiltinModule {
    pub fn name(self) -> &'static str {
        match self {
            Self::Intrinsics => "__intrinsics__",
            Self::Comptime => "__comptime__",
        }
    }

    pub fn import_line(self) -> &'static str {
        match self {
            Self::Intrinsics => "import __intrinsics__;",
            Self::Comptime => "import __comptime__;",
        }
    }

    pub fn summary(self) -> &'static str {
        match self {
            Self::Intrinsics => {
                "Compiler-provided module containing the Kio' core value intrinsics."
            }
            Self::Comptime => {
                "Compiler-provided block import containing reflected type handles, checked-term constructors, diagnostic text, and compile-time recursion helpers for elaborator implementations."
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuiltinSignature {
    #[cfg(any(feature = "lsp", feature = "repl", test))]
    ModuleImport,
    Type,
    Value(String),
}

impl BuiltinSignature {
    #[cfg(feature = "lsp")]
    pub fn detail(&self) -> Option<String> {
        match self {
            Self::ModuleImport => None,
            Self::Type => Some("type".to_owned()),
            Self::Value(scheme) => Some(scheme.clone()),
        }
    }

    pub fn reference_line(&self, name: &str) -> String {
        match self {
            #[cfg(any(feature = "lsp", feature = "repl", test))]
            Self::ModuleImport => format!("use {name};"),
            Self::Type => format!("type {name}"),
            Self::Value(scheme) => format!("{name} : {scheme}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltinDoc {
    pub module: BuiltinModule,
    pub name: &'static str,
    pub signature: BuiltinSignature,
    pub group: &'static str,
    pub summary: &'static str,
    pub details: &'static str,
    pub example: Option<&'static str>,
}

#[cfg(feature = "lsp")]
pub fn builtin_detail(label: &str) -> Option<String> {
    doc_for_label(label).and_then(|doc| doc.signature.detail())
}

#[cfg(feature = "lsp")]
pub fn builtin_markdown(label: &str) -> Option<String> {
    doc_for_label(label).map(|doc| render_entry_markdown(&doc, "kio"))
}

#[cfg(any(feature = "lsp", feature = "repl", test))]
pub fn doc_for_label(label: &str) -> Option<BuiltinDoc> {
    if label == BuiltinModule::Intrinsics.name() {
        return Some(BuiltinDoc {
            module: BuiltinModule::Intrinsics,
            name: BuiltinModule::Intrinsics.name(),
            signature: BuiltinSignature::ModuleImport,
            group: "Module",
            summary: BuiltinModule::Intrinsics.summary(),
            details: "The import is available only as the block form shown. Its values are runtime Kio' constructors and eliminators, not reflection helpers.",
            example: None,
        });
    }
    if label == BuiltinModule::Comptime.name() {
        return Some(BuiltinDoc {
            module: BuiltinModule::Comptime,
            name: BuiltinModule::Comptime.name(),
            signature: BuiltinSignature::ModuleImport,
            group: "Module",
            summary: BuiltinModule::Comptime.summary(),
            details: "The import is available only as the block form shown. Its types and values are usable while an elaborator implementation is evaluated and do not form a runtime library API.",
            example: None,
        });
    }
    intrinsic_doc(label).or_else(|| comptime_doc(label))
}

pub fn docs_for_module(module: BuiltinModule) -> Vec<BuiltinDoc> {
    match module {
        BuiltinModule::Intrinsics => PRIME_INTRINSICS
            .iter()
            .filter_map(|name| intrinsic_doc(name))
            .collect(),
        BuiltinModule::Comptime => PUBLIC_COMPTIME_NAMES
            .iter()
            .filter_map(|name| comptime_doc(name))
            .collect(),
    }
}

pub fn render_markdown_guide() -> String {
    let mut out = String::new();
    out.push_str("# Builtin Modules\n\n");
    out.push_str(
        "Kio has two compiler-provided import blocks. `__intrinsics__` exposes the Kio' core value operations. `__comptime__` exposes the reflection and checked-term helpers used by elaborator implementations. Ordinary source should start with surface forms — tuples, labels, `match!`, and structural elaborators — and use `__intrinsics__` only when the core operation itself is the subject.\n\n",
    );
    out.push_str(
        "If you are trying to construct or inspect sums and products in a normal\nprogram, use the structural guides instead of this reference. The\n`import __intrinsics__;` snippet below is a reference entry for core-level code,\nnot a package or module starter.\n\n",
    );
    out.push_str("Import and signature blocks are reference fragments and are marked `{ignore}`. Worked examples use standalone checked fences. The first paragraph of each entry is the concise summary used by editor hover; the following text is the long-form reference.\n\n");
    render_module_section(&mut out, BuiltinModule::Intrinsics);
    render_module_section(&mut out, BuiltinModule::Comptime);
    let trimmed_len = out.trim_end().len();
    out.truncate(trimmed_len);
    out.push('\n');
    out
}

fn render_module_section(out: &mut String, module: BuiltinModule) {
    let _ = writeln!(out, "## `{}`\n", module.name());
    let _ = writeln!(out, "{}\n", module.summary());
    push_fence(out, "kio {ignore}", module.import_line());
    out.push('\n');

    let docs = docs_for_module(module);
    let mut current_group = "";
    for doc in docs {
        if doc.group != current_group {
            current_group = doc.group;
            let _ = writeln!(out, "### {current_group}\n");
        }
        let _ = writeln!(out, "#### `{}`\n", doc.name);
        push_fence(out, "kio {ignore}", &doc.signature.reference_line(doc.name));
        out.push('\n');
        let _ = writeln!(out, "{}\n", doc.summary);
        let _ = writeln!(out, "{}\n", doc.details);
        if let Some(example) = doc.example {
            let _ = writeln!(out, "##### Checked example: `{}`\n", doc.name);
            push_fence(out, "kio {}", example);
            out.push('\n');
        }
    }
}

#[cfg(feature = "lsp")]
fn render_entry_markdown(doc: &BuiltinDoc, fence_info: &str) -> String {
    let mut markdown = String::new();
    push_fence(
        &mut markdown,
        fence_info,
        &doc.signature.reference_line(doc.name),
    );
    markdown.push('\n');
    markdown.push_str(doc.summary);
    markdown
}

fn push_fence(out: &mut String, fence_info: &str, body: &str) {
    let _ = writeln!(out, "```{fence_info}");
    out.push_str(body.trim_end());
    out.push('\n');
    out.push_str("```\n");
}

fn intrinsic_doc(label: &str) -> Option<BuiltinDoc> {
    let name = PRIME_INTRINSICS
        .iter()
        .copied()
        .find(|name| *name == label)?;
    let signature = BuiltinSignature::Value(intrinsic_scheme(name)?);
    Some(BuiltinDoc {
        module: BuiltinModule::Intrinsics,
        name,
        signature,
        group: "Value Intrinsics",
        summary: intrinsic_summary(name),
        details: intrinsic_details(name),
        example: None,
    })
}

fn intrinsic_scheme(label: &str) -> Option<String> {
    if label == "__if_then_else__" {
        return Some("[A](Bool & (. -> A) & (. -> A)) -> A".to_owned());
    }
    let scheme = crate::pass::typecheck_core::intrinsic_scheme::<Surface>(
        label,
        Span::new(0, 0),
        &HashMap::new(),
    )?
    .ok()?;
    Some(crate::pretty::pretty_type(&scheme.ty))
}

fn intrinsic_summary(name: &str) -> &'static str {
    match name {
        "__left__" => "Constructs the left branch of a sum value.",
        "__right__" => "Constructs the right branch of a sum value.",
        "__either__" => {
            "Eliminates a sum by applying the left handler or the right handler to the carried value."
        }
        "__pair__" => "Constructs a product value from its two fields.",
        "__fst__" => "Projects the first field from a product value.",
        "__snd__" => "Projects the second field from a product value.",
        "__if_then_else__" => {
            "Branches on the single in-scope `role(bool)` identity. The true and false branches are thunks so only the selected branch is evaluated."
        }
        "__absurd__" => "Eliminates a bottom value into any requested result type.",
        _ => "Kio' core intrinsic.",
    }
}

fn intrinsic_details(name: &str) -> &'static str {
    match name {
        "__left__" => {
            "The two type arguments fix the complete `A | B` result and the value must have type `A`. No branch search occurs. A missing or incompatible type argument is a type error."
        }
        "__right__" => {
            "The two type arguments fix the complete `A | B` result and the value must have type `B`. No branch search occurs. A missing or incompatible type argument is a type error."
        }
        "__either__" => {
            "The scrutinee must have type `A | B`; the two handlers receive `A` and `B` respectively and must return one common `C`. Only the selected handler runs. An uncovered type, a mismatched handler parameter, or different handler results is a type error."
        }
        "__pair__" => {
            "The two argument types determine the ordered product `A & B`. This constructor neither flattens nor reorders nested products. Wrong explicit type arguments are rejected as type errors."
        }
        "__fst__" => {
            "The value must have exactly the product type `A & B`; the result is its left component. The helper does not search a wider product spine, so a different product shape is a type error."
        }
        "__snd__" => {
            "The value must have exactly the product type `A & B`; the result is its right component. The helper does not search a wider product spine, so a different product shape is a type error."
        }
        "__if_then_else__" => {
            "The condition uses the unique selected `role(bool)` type identity from unqualified lexical scope. Direct host types are selected when present; aliases form the fallback pool otherwise, with repeated paths to one declaration counted once. A qualified module import alone does not add its members. Both branches are unit-domain functions returning the same `A`; the intrinsic calls only the chosen thunk. With no Boolean-role identity, more than one selected identity, a non-boolean condition, or disagreeing branch results, typechecking fails."
        }
        "__absurd__" => {
            "The argument must have bottom type `!`; because `!` has no values, a well-typed closed program cannot reach this eliminator with a constructed value. It supplies no fallback behavior: an argument of any inhabited type is rejected, and a stuck host computation returning `!` remains opaque during compile-time evaluation."
        }
        _ => {
            "The intrinsic follows its displayed Kio' scheme. Calls outside that scheme are type errors."
        }
    }
}

fn comptime_doc(label: &str) -> Option<BuiltinDoc> {
    let builtin = comptime_builtin(label)?;
    let name = builtin.public_name();
    let signature = if builtin.is_type_name() {
        BuiltinSignature::Type
    } else {
        BuiltinSignature::Value(comptime_scheme(builtin)?)
    };
    Some(BuiltinDoc {
        module: BuiltinModule::Comptime,
        name,
        signature,
        group: comptime_group(builtin),
        summary: comptime_summary(builtin),
        details: comptime_details(builtin),
        example: comptime_example(builtin),
    })
}

fn comptime_builtin(label: &str) -> Option<ComptimeBuiltin> {
    ComptimeBuiltin::from_public_name(label)
}

fn comptime_scheme(builtin: ComptimeBuiltin) -> Option<String> {
    let mut roles = HashMap::new();
    roles.insert(Role::Bool, "Comptime_bool");
    roles.insert(Role::Str, "Comptime_str");
    let scheme =
        crate::pass::typecheck_core::comptime_scheme::<Surface>(builtin, Span::new(0, 0), &roles)
            .ok()??;
    Some(crate::pretty::pretty_type(&scheme.ty))
}

fn comptime_group(builtin: ComptimeBuiltin) -> &'static str {
    use ComptimeBuiltin::*;
    match builtin {
        ComptimeProof | Type | CheckedTerm | TypeVar | TypeName | TypeArity | DiagnosticText
        | ComptimeBool | ComptimeStr | FillCtx => "Types",
        ReflectType | TypeView | TypeIsHost | TypeHostConverters | TypeEqual | TypeNameEqual
        | TypeVarEqual | TypeIsProductRoot | TypeIsSumRoot => "Type Inspection",
        TypeUnit | TypeBottom | TypeProduct | TypeSum | TypeArrow | TypeForall | TypeVarFn
        | TypeNameType | TypeApply | TypeInstantiate | TypeArityFn | TypeVarArity
        | TypeArityZero | TypeAritySucc | TypeArityEqual => "Type Construction",
        TypeProductSpineFold
        | TypeSumSpineFold
        | TypeArgsFold
        | TypeFunctionParamsFold
        | TypeArityFold
        | TypeNameParamAritiesFold => "Type Folds",
        TypeDisplay | TypeShortName | TypeNameDisplay | TypeVarDisplay | TypeArityDisplay
        | DiagnosticConcat => "Diagnostics",
        ElabError | TypeError => "Error Sentinels",
        StructuralRecur => "Compile-Time Recursion",
        TermType | TermLet | TermFn | TermCall | TermTypeFn | TermTypeApp | TermUnit
        | TermHostConvert | TermSpecialize => "Checked Terms",
        Fill => "Fill Relations",
        IntrinsicPair | IntrinsicFst | IntrinsicSnd | IntrinsicLeft | IntrinsicRight
        | IntrinsicEither | IntrinsicAbsurd | IntrinsicIfThenElse => {
            "Reflected Intrinsic Constructors"
        }
    }
}

fn comptime_summary(builtin: ComptimeBuiltin) -> &'static str {
    use ComptimeBuiltin::*;
    match builtin {
        ComptimeProof => {
            "Opaque proof that a helper is callable only during compile-time evaluation."
        }
        Type => "Opaque reflected handle for a Kio type.",
        CheckedTerm => {
            "Opaque checked term template. The carried type can be read with `__term_type__`."
        }
        TypeVar => "Opaque handle for a reflected type variable.",
        TypeName => "Opaque handle for the named head of a reflected named type.",
        TypeArity => "Opaque arity value used by reflected type variables, names, and binders.",
        DiagnosticText => "Compile-time text used in elaborator diagnostics.",
        ComptimeBool => "Compile-time boolean type with `role(bool)`.",
        ComptimeStr => "Compile-time string type with `role(str)`.",
        FillCtx => "Opaque immutable transcript of explicit fill relations for one marked call.",
        ReflectType => {
            "Reflects a type argument into an opaque `__Type__` handle. Elaborators use it when they need to inspect a type parameter, compare types, or pass a type into checked-term construction."
        }
        TypeView => "Decomposes a reflected type into the compiler's structural type-view shape.",
        TypeIsHost => {
            "Tests whether a reflected type is a host type — one declared with `host type`."
        }
        TypeHostConverters => {
            "Builds the converter product type needed to turn host leaves inside a source type into compile-time strings."
        }
        TypeEqual => "Tests whether two reflected types are equivalent.",
        TypeNameEqual => "Tests whether two reflected type-name handles name the same type head.",
        TypeVarEqual => "Tests whether two reflected type-variable handles name the same variable.",
        TypeIsProductRoot => "Tests whether a reflected type's outer shape is a product spine.",
        TypeIsSumRoot => "Tests whether a reflected type's outer shape is a sum spine.",
        TypeUnit => "Constructs the reflected unit type.",
        TypeBottom => "Constructs the reflected bottom type.",
        TypeProduct => "Constructs a reflected product type from two reflected member types.",
        TypeSum => "Constructs a reflected sum type from two reflected branch types.",
        TypeArrow => {
            "Constructs a reflected function type from a parameter packet, parameter arity, and result type."
        }
        TypeForall => "Constructs a reflected polymorphic type by binding fresh type variables.",
        TypeVarFn => "Turns a reflected type-variable handle into a reflected type.",
        TypeNameType => "Turns a reflected named-type handle into a reflected type head.",
        TypeApply => "Applies a reflected type head to one reflected type argument.",
        TypeInstantiate => {
            "Instantiates a reflected polymorphic type with one reflected type argument, returning a diagnostic on failure."
        }
        TypeArityFn => "Reads the arity of a reflected type.",
        TypeVarArity => "Reads the arity carried by a reflected type variable.",
        TypeArityZero => "Constructs the zero arity value.",
        TypeAritySucc => "Constructs the successor of an arity value.",
        TypeArityEqual => "Tests whether two reflected arity values are equal.",
        TypeProductSpineFold => "Folds over the member types of a reflected product spine.",
        TypeSumSpineFold => "Folds over the branch types of a reflected sum spine.",
        TypeArgsFold => "Folds over the type arguments applied to a reflected type head.",
        TypeFunctionParamsFold => "Folds over the parameter packet of a reflected function type.",
        TypeArityFold => "Runs a step function once for each slot in a reflected arity.",
        TypeNameParamAritiesFold => {
            "Folds over the parameter arities declared by a reflected type name."
        }
        TypeDisplay => "Renders a reflected type as diagnostic text.",
        TypeShortName => "Renders a compact diagnostic name for a reflected type.",
        TypeNameDisplay => "Renders a reflected type-name handle as diagnostic text.",
        TypeVarDisplay => "Renders a reflected type-variable handle as diagnostic text.",
        TypeArityDisplay => "Renders a reflected arity value as diagnostic text.",
        DiagnosticConcat => "Concatenates two pieces of diagnostic text.",
        ElabError => {
            "Builds a checked-term sentinel that reports an elaborator diagnostic at the call site."
        }
        TypeError => "Builds a checked-term sentinel that reports a type error at the call site.",
        StructuralRecur => {
            "Runs fuel-checked compile-time structural recursion through an ordinary helper call."
        }
        TermType => "Reads the reflected type carried by a checked term.",
        TermLet => "Constructs a checked let term.",
        TermFn => "Constructs a checked function term.",
        TermCall => "Constructs a checked function call term.",
        TermTypeFn => "Constructs a checked type-function term.",
        TermTypeApp => "Constructs a checked type-application term.",
        TermUnit => "Constructs the checked unit term.",
        TermHostConvert => {
            "Constructs a checked host-conversion term using the converter product from `__type_host_converters__`."
        }
        Fill => "Appends one ordered destination/candidate relation to a fill transcript.",
        TermSpecialize => {
            "Specializes an explicitly polymorphic checked term against a reflected type relation."
        }
        IntrinsicPair => {
            "Constructs a checked term that calls the reflected product constructor intrinsic."
        }
        IntrinsicFst => {
            "Constructs a checked term that calls the reflected first-projection intrinsic."
        }
        IntrinsicSnd => {
            "Constructs a checked term that calls the reflected second-projection intrinsic."
        }
        IntrinsicLeft => {
            "Constructs a checked term that calls the reflected left-sum constructor intrinsic."
        }
        IntrinsicRight => {
            "Constructs a checked term that calls the reflected right-sum constructor intrinsic."
        }
        IntrinsicEither => {
            "Constructs a checked term that calls the reflected sum eliminator intrinsic."
        }
        IntrinsicAbsurd => {
            "Constructs a checked term that calls the reflected bottom eliminator intrinsic."
        }
        IntrinsicIfThenElse => {
            "Constructs a checked term that calls the reflected role-boolean conditional intrinsic."
        }
    }
}

fn comptime_details(builtin: ComptimeBuiltin) -> &'static str {
    use ComptimeBuiltin::*;
    match builtin {
        ComptimeProof => {
            "The compiler supplies this value as the first implementation argument and every `__comptime__` value helper requires it first. Source cannot construct one. Letting it reach runtime is invalid; its runtime representation is bottom."
        }
        Type => {
            "A handle preserves the identity, binders, kind arity, and structural shape of the reflected type. It is inspected and rebuilt only through this module's helpers. It is not a runtime type value, and using it outside compile-time evaluation is invalid."
        }
        CheckedTerm => {
            "The template is already typed Kio' syntax, not source text or an unchecked AST. Constructors verify their component types while building it, and the elaborator result is checked against the declared call result before substitution. It cannot be manufactured or inspected at runtime."
        }
        TypeVar => {
            "A handle identifies one fresh reflected binder and records that binder's kind arity. Compare handles with `__type_var_equal__` and turn one into a type with `__type_var__`. It has no user-constructible or runtime representation."
        }
        TypeName => {
            "A handle identifies the named head of a reflected path together with its parameter arities. Use `__type_name_type__` before applying arguments. It is not interchangeable with `__Type__` and has no runtime representation."
        }
        TypeArity => {
            "Arity is a compile-time natural number used for kinds and function parameter packets. Build it from zero and successor or read it from a type/variable. It is opaque to ordinary arithmetic and has no runtime representation."
        }
        DiagnosticText => {
            "Diagnostic text is the only message payload accepted by `__elab_error__` and `__type_error__`. Produce it from `Comptime_str`, display helpers, and concatenation. It is compiler-owned text, not a runtime string."
        }
        ComptimeBool => {
            "The literals `.t` and `.f` form the compile-time predicate result used by reflection helpers and `if`/`else`. It is distinct from every package host boolean. It is erased after compile-time evaluation and is not a runtime API type."
        }
        ComptimeStr => {
            "String literals in an elaborator implementation can inhabit this compile-time role string and flow into diagnostic text. It is distinct from package host strings, is erased after evaluation, and cannot cross into generated runtime code."
        }
        FillCtx => {
            "The compiler supplies one fresh context as the second argument of an `impl(fills)` implementation. `__fill__` returns a descendant context instead of mutating its input, so Kio code threads the returned value explicitly. A context cannot be constructed by source, converted to or from `__Comptime__`, inspected, or allowed to reach runtime."
        }
        ReflectType => {
            "The type argument must be solved before the helper runs; the result is its exact reflected handle, including nominal identity and kind. An unsolved elaborator type slot cannot call this helper and is reported as a type-inference error before evaluation."
        }
        TypeView => {
            "After transparent-alias unfolding, returns an eight-arm structural view in this order: unit; bottom; product pair; sum pair; function `(parameters & arity & result)`; forall `(variable & arity & body)`; type variable; named head. Consumers must cover the complete sum. An unresolved type cannot be viewed and leaves elaboration stuck."
        }
        TypeIsHost => {
            "Returns `.t` exactly when the reflected identity resolves to a package `host type`, following transparent aliases, and `.f` for structural or package-defined nominal types. The argument must be a resolved reflected type; the helper emits no diagnostic sentinel."
        }
        TypeHostConverters => {
            "Builds the right-associated product of converter function types needed for every host-type leaf reachable in `source_type`; each converter maps that host leaf (polymorphically when needed) to `string_type`. No host leaves produce unit. A converter value is checked separately by `__term_host_convert__`; mismatch there makes term construction fail."
        }
        TypeEqual => {
            "Returns a compile-time boolean using Kio's definitional type equivalence, including transparent-alias unfolding and binder renaming while preserving nominal and host identities. It never coerces structural near-matches; unresolved handles cannot reach the helper."
        }
        TypeNameEqual => {
            "Returns `.t` only when both reflected named heads have the same qualified identity and parameter-arity vector. Applied arguments are not compared here. Supplying ordinary type handles instead of name handles is a type error."
        }
        TypeVarEqual => {
            "Returns `.t` only when both handles identify the same reflected binder with the same arity. Equal display names from different binders do not suffice. Supplying ordinary type handles instead of variable handles is a type error."
        }
        TypeIsProductRoot => {
            "Unfolds transparent aliases and tests only the outer constructor. Nested products below another constructor do not count. The result is always a compile-time boolean for a resolved handle."
        }
        TypeIsSumRoot => {
            "Unfolds transparent aliases and tests only the outer constructor. Nested sums below another constructor do not count. The result is always a compile-time boolean for a resolved handle."
        }
        TypeUnit => {
            "Returns the reflected unit type `.`. It takes only the compile-time proof and cannot fail once called in a compile-time context."
        }
        TypeBottom => {
            "Returns the reflected bottom type `!`. It constructs a type handle, not a bottom value, and cannot fail once called in a compile-time context."
        }
        TypeProduct => {
            "Constructs the ordered binary type `left & right`; it does not flatten either input. Both inputs must already be reflected types, so malformed components are rejected by the helper's static scheme."
        }
        TypeSum => {
            "Constructs the ordered binary type `left | right`; it does not flatten either input or remove duplicates. Both inputs must already be reflected types, so malformed components are rejected by the static scheme."
        }
        TypeArrow => {
            "Constructs a function type from a parameter packet, the packet's ABI arity, and a result type. The arity must describe the parameter packet the generated call convention will use; inconsistent shapes are rejected when a checked function or call is constructed."
        }
        TypeForall => {
            "Creates a fresh type variable of the requested kind arity, passes its handle to `body`, and binds the returned reflected type. The binder is hygienic. A body that does not return `__Type__` is rejected by the static scheme."
        }
        TypeVarFn => {
            "Turns a reflected variable handle into the type that refers to that binder. The handle must come from reflection or a `__type_forall__` callback; arbitrary names cannot be forged."
        }
        TypeNameType => {
            "Turns a reflected name handle into its unapplied named type head. Required arguments are added with `__type_apply__`; leaving a higher-arity head unsaturated is rejected when a kind-`*` type is required."
        }
        TypeApply => {
            "Appends one reflected argument to a named/path type head. Apply once per argument in source order and respect the head's declared kinds. Applying a non-head or over-applying produces an unusable type that later kind/type validation rejects; this helper has no diagnostic-result arm. During marked evaluation, if the outer head is still unresolved after alias unfolding, evaluation remains residual: the helper does not return the unchanged head or discard the argument."
        }
        TypeInstantiate => {
            "Instantiates the outermost reflected `forall` binder with one argument. Success is the left `__Type__` arm; a non-forall input or a structural argument for a higher-kinded binder returns diagnostic text in the right arm. Call again for additional binders."
        }
        TypeArityFn => {
            "Returns the kind arity of the reflected type's outer binder/head: zero for an ordinary kind-`*` type and the recorded arrow count for a higher-kinded binder. The argument must already be resolved."
        }
        TypeVarArity => {
            "Reads the kind arity recorded on a reflected variable handle. It cannot accept a general type handle; that mismatch is a type error."
        }
        TypeArityZero => {
            "Constructs arity zero, representing kind `*` or an empty ABI parameter packet. It cannot fail in a compile-time context."
        }
        TypeAritySucc => {
            "Constructs the successor of an existing arity. Repeated calls encode higher-kinded arrow counts or function ABI slots; ordinary numeric values are not accepted."
        }
        TypeArityEqual => {
            "Returns a compile-time boolean comparing two opaque arity values numerically. It does not compare the types or binders those arities came from."
        }
        TypeProductSpineFold => {
            "Unfolds aliases, walks a right-associated product from left to right, and calls `step(acc, member)` for each member. A non-product is treated as a one-member spine. `R` fixes one accumulator/result type; a step with another shape is a type error."
        }
        TypeSumSpineFold => {
            "Unfolds aliases, walks a right-associated sum from left to right, and calls `step(acc, branch)` for each branch. A non-sum is treated as a one-branch spine. `R` fixes one accumulator/result type; a step with another shape is a type error."
        }
        TypeArgsFold => {
            "Walks the applied arguments of a reflected named type from left to right and calls `step(acc, argument)` for each. A structural or unapplied type has no arguments and returns `init`. During marked evaluation, if the outer type is still unresolved after alias unfolding, evaluation remains residual: the helper does not treat it as a known zero-argument type or return `init`. The accumulator type `R` cannot change between steps."
        }
        TypeFunctionParamsFold => {
            "Unfolds aliases, splits the outer function's parameter packet according to its ABI arity, and folds parameters left to right. A non-function yields no parameters and therefore `init`; malformed arity metadata prevents checked-term construction later."
        }
        TypeArityFold => {
            "Calls `step(acc)` exactly once per slot in `arity`, starting from `init`. It is the total iterator for opaque arities; the accumulator must retain type `R`, and there is no early-exit channel."
        }
        TypeNameParamAritiesFold => {
            "Walks a reflected name's declared parameter arities in source order and calls `step(acc, arity)`. It describes the head's parameter kinds, not currently-applied arguments. A general `__Type__` must first be viewed to obtain the name handle."
        }
        TypeDisplay => {
            "Pretty-prints the complete reflected type for a call-site diagnostic, preserving structure and qualified identities where required. Canonical qualified identities separate module-path components with `/` and the final named head with `.`, as in `foo/bar.Item`. The text is diagnostic-only and must not be parsed back into a type."
        }
        TypeShortName => {
            "Pretty-prints the complete reflected type using the evaluation module's lexical imports. An unshadowed type declared in that module or selectively imported there uses its bare name. Otherwise, the shortest unshadowed qualified import alias for the type's exact declaring module is used, with import order breaking equal-length ties; when no such spelling is available, the canonical path remains, with `/` between its module components. Type applications, functions, products, sums, forall binders, and alias heads stay intact. The text is diagnostic-only and must not be parsed or compared as a type identity; use `__type_equal__` for decisions."
        }
        TypeNameDisplay => {
            "Renders a reflected named head as diagnostic text, including its qualifying path. Canonical module-path components use `/` and the final named head uses `.`, as in `foo/bar.Item`; a lexical import alias remains source-shaped, as in `m.Item`. It does not render applied arguments and must not be used for name equality."
        }
        TypeVarDisplay => {
            "Renders the compiler-chosen name of a reflected type variable for diagnostics. Binder identity must still be compared with `__type_var_equal__`, because display text is not an identity token."
        }
        TypeArityDisplay => {
            "Renders the opaque arity as decimal diagnostic text. This is presentation only; arity logic uses the constructor, fold, and equality helpers."
        }
        DiagnosticConcat => {
            "Concatenates two diagnostic fragments without inserting whitespace or punctuation. Both inputs must be compile-time diagnostic text; host strings are rejected by the type checker."
        }
        ElabError => {
            "Returns a checked-term error sentinel carrying the message. When selected as the elaborator result, the call fails in the elaborator-error category (exit 15) at the bang-call site. It does not construct a runtime term."
        }
        TypeError => {
            "Returns a checked-term error sentinel carrying the message. When selected as the elaborator result, the call fails in the ordinary type-error category (exit 14) at the bang-call site. It does not construct a runtime term."
        }
        StructuralRecur => {
            "Runs `step(recur, fuel, input)` after measuring the initial `fuel`. Unit, literals, structurally determined reflected values, arities, diagnostic text, checked terms, and finite constructor trees with measurable children are measurable; closures, opaque atoms, stuck terms, case splits, and recursive callbacks are not. An initial projected type uses its authenticated alias-unfolded structural carrier as a conservative lower bound; its exact scoped snapshot remains available for whole-handle reflection and fills. An unresolved outer carrier is unmeasurable, and a direct projected value used on a recursive edge must be complete. When an ordinary finite constructor carries a projected field, a known outer structure gives that field the same conservative size in initial and recursive measurements; an unresolved outer field makes the constructor unmeasurable. A structurally determined child can remain measurable even when its parent is open. `recur(next_fuel, next_input)` must give the callback a recursive fuel measure strictly below both the current measure and its stored root measure. Evaluator-created callbacks maintain `current ≤ root`; both bounds are checked independently for a callback constructed directly through the evaluator API. Each resolved helper source occurrence supplies an origin that survives proof/type application stages and higher-order use. The helper's three operands and the callback's two operands may be written as flattened arguments or as one right-folded product packet. After all operands are evaluated, the first invocation at an origin uses the initial measure; a re-entry at that origin uses the recursive measure and must be strictly below the nearest active invocation. Invocations from distinct origins do not compare measures. Equal-measure fuel is rejected regardless of value identity, representation, or whether it contains a projected handle. The callback has type `(fuel & input) -> result`, and every path returns the one `result` type. Unmeasurable initial or recursive fuel, a callback that does not strictly descend from its current measure or stay below its stored root, or an active same helper origin re-entry that does not strictly descend leaves the evaluator stuck and is reported as a totality error (exit 16) with root/current/next measures when available. If both callback bounds fail, the diagnostic reports the current edge; the root-specific diagnostic applies only when the current edge descends but the root bound fails. Once an evaluated edge fails, enclosing compile-time computation cannot discard it; that evaluation reports its first totality failure."
        }
        TermType => {
            "Reads the exact reflected type stored on a checked term, qualifying host identities consistently with reflected call types. Error sentinels and non-term compile-time values cannot be inspected as terms."
        }
        TermLet => {
            "Creates a hygienic let binder of `value_type`, checks `value` against it, passes a checked reference to `body`, and returns the body's checked term. A value/type mismatch or non-term callback result makes construction fail and the elaborator cannot return a valid term."
        }
        TermFn => {
            "Creates a hygienic value lambda with the displayed `fn_type`; the callback receives one checked parameter packet and must return a term of the function's result type. The type must be an outer function with consistent ABI arity, otherwise construction stays stuck and the elaborator fails."
        }
        TermCall => {
            "Creates a checked application after splitting `fn_type` into its parameter packet and result. `fn_value` must have that exact function type and `arg_packet` must have the right-associated parameter type; any mismatch makes construction stay stuck rather than emitting ill-typed Kio'."
        }
        TermTypeFn => {
            "Creates a hygienic type abstraction with a fresh binder of `var_arity`; the callback receives its variable handle and returns the checked body. The result is polymorphic checked Kio'. A non-term callback result cannot be constructed."
        }
        TermTypeApp => {
            "Applies one reflected type argument to a checked term whose outer type is `forall`, substituting the binder in the result type. A monomorphic term or invalid kind application makes construction stay stuck."
        }
        TermUnit => {
            "Constructs the checked unit term `()` with reflected type `.`. It is always valid when called with the compile-time proof."
        }
        TermHostConvert => {
            "Selects the converter matching the host-typed leaf carried by `value` and constructs its checked call to `string_type`. `converters` must have exactly the product type returned by `__type_host_converters__`; an absent host match, wrong converter product, or wrong value type makes construction fail."
        }
        Fill => {
            "Requires the marked call's `__Comptime__` proof first and matching `__Fill_ctx__` second. It records the relation without solving, normalizing, or reading other transcript entries; only the returned context contains the append. Relation order is therefore ordinary source-level value flow."
        }
        TermSpecialize => {
            "Uses only its explicit checked term, pattern type, and target type. Success returns the left checked-term arm, an ordinary route mismatch returns unit, and malformed or underdetermined operands return diagnostic text. It receives no fill context and cannot inspect the call's transcript or ambient inference state."
        }
        IntrinsicPair => {
            "Constructs a checked call to runtime `__pair__` and derives the product type from the two checked operands. This is a reflected-term constructor; it does not call the runtime intrinsic during elaboration. Non-term operands are type errors at the helper call."
        }
        IntrinsicFst => {
            "Constructs a checked runtime first projection. `product_type` must unfold to `A & B` and `value` must have that exact type; otherwise construction fails instead of emitting an invalid projection."
        }
        IntrinsicSnd => {
            "Constructs a checked runtime second projection. `product_type` must unfold to `A & B` and `value` must have that exact type; otherwise construction fails instead of emitting an invalid projection."
        }
        IntrinsicLeft => {
            "Constructs a checked runtime left injection. `sum_type` must unfold to `A | B` and `value` must have type `A`; mismatches make construction fail."
        }
        IntrinsicRight => {
            "Constructs a checked runtime right injection. `sum_type` must unfold to `A | B` and `value` must have type `B`; mismatches make construction fail."
        }
        IntrinsicEither => {
            "Constructs a checked runtime sum elimination. The value must have `sum_type`; each callback receives a checked branch payload and must return `result_type`. A non-sum, wrong scrutinee, or handler-result mismatch makes construction fail."
        }
        IntrinsicAbsurd => {
            "Constructs a checked runtime bottom elimination from `bottom_value` to `result_type`. The value must have type `!`; any inhabited source type makes construction fail."
        }
        IntrinsicIfThenElse => {
            "Constructs a checked runtime `__if_then_else__` call. The condition must use the caller's single `role(bool)` identity and both zero-argument callbacks must return `result_type`. Missing or ambiguous Boolean-role information, or a branch mismatch, makes construction fail."
        }
    }
}

fn comptime_example(builtin: ComptimeBuiltin) -> Option<&'static str> {
    use ComptimeBuiltin::*;
    match builtin {
        TypeProductSpineFold => Some(
            r#"module builtin_type_fold_example;

import __comptime__;

fn product_arity(ct: __Comptime__, typ: __Type__) -> __Type_arity__ {
  __type_product_spine_fold__(
    , ct
    , __Type_arity__
    , typ
    , __type_arity_zero__(ct)
    , .(count: __Type_arity__, _member: __Type__) -> __Type_arity__ {
        __type_arity_succ__(ct, count)
      }
    )
}"#,
        ),
        TermFn => Some(
            r#"module builtin_checked_fn_example;

import __comptime__;

fn identity_term(ct: __Comptime__, typ: __Type__) -> __Checked_term__ {
  let fn_type = __type_arrow__(ct, typ, __type_arity_succ__(ct, __type_arity_zero__(ct)), typ);
  __term_fn__(ct, fn_type, .(parameter: __Checked_term__) -> __Checked_term__ { parameter })
}"#,
        ),
        StructuralRecur => Some(
            r#"module builtin_structural_recur_example;

import __intrinsics__;

import __comptime__;

rec newtype Fuel : . | Fuel { pub constructor mk_fuel; pub projector un_fuel }

fn stop() -> Fuel { Fuel.mk_fuel(__left__(., Fuel, ())) }

fn tick(rest: Fuel) -> Fuel { Fuel.mk_fuel(__right__(., Fuel, rest)) }

fn count_down(ct: __Comptime__, fuel: Fuel) -> __Type_arity__ {
  __structural_recur__(
    , ct
    , Fuel
    , .
    , __Type_arity__
    , fuel
    , ()
    , .(recur: (Fuel & .) -> __Type_arity__, current: Fuel, _input: .) -> __Type_arity__ {
        __either__(
          , .
          , Fuel
          , __Type_arity__
          , Fuel.un_fuel(current)
          , .(_done: .) -> __Type_arity__ { __type_arity_zero__(ct) }
          , .(rest: Fuel) -> __Type_arity__ { __type_arity_succ__(ct, recur(rest, ())) }
          )
      }
    )
}"#,
        ),
        FillCtx => None,
        Fill => Some(
            r#"module builtin_fill_example;

import __comptime__;

fn append_result(ct: __Comptime__, fills: __Fill_ctx__, destination: __Type__, candidate: __Type__) -> __Fill_ctx__ {
  __fill__(ct, fills, destination, candidate)
}"#,
        ),
        TermSpecialize => Some(
            r#"module builtin_term_specialize_example;

import __comptime__;

fn specialize(ct: __Comptime__, term: __Checked_term__, pattern: __Type__, target: __Type__) ->
  __Checked_term__ | . | __Diagnostic_text__
   { __term_specialize__(ct, term, pattern, target) }"#,
        ),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_has_documentation() {
        for name in PRIME_INTRINSICS {
            let doc = doc_for_label(name).unwrap_or_else(|| panic!("missing doc for {name}"));
            assert!(matches!(doc.signature, BuiltinSignature::Value(_)));
            assert!(!doc.details.trim().is_empty(), "empty details for {name}");
        }
        for name in PUBLIC_COMPTIME_NAMES {
            let doc = doc_for_label(name).unwrap_or_else(|| panic!("missing doc for {name}"));
            assert!(!doc.summary.trim().is_empty(), "empty summary for {name}");
            assert!(!doc.details.trim().is_empty(), "empty details for {name}");
            match doc.signature {
                BuiltinSignature::Type => {
                    let builtin = ComptimeBuiltin::from_public_name(name).expect("public name");
                    assert!(builtin.is_type_name(), "{name} should be a value");
                }
                BuiltinSignature::Value(ref signature) => {
                    assert!(!signature.trim().is_empty(), "empty signature for {name}");
                }
                BuiltinSignature::ModuleImport => panic!("{name} documented as a module"),
            }
        }
    }

    #[test]
    fn guide_contains_all_builtin_names_and_checked_examples() {
        use std::collections::HashSet;

        let guide = render_markdown_guide();
        assert!(guide.contains("```kio {ignore}\n"));
        assert!(!guide.contains("```kio\n"));
        assert_eq!(guide.matches("```kio {}\n").count(), 5);
        assert!(guide.contains(BuiltinModule::Intrinsics.name()));
        assert!(guide.contains(BuiltinModule::Comptime.name()));
        assert!(guide.contains("higher-order use"));
        assert!(guide.contains("right-folded product packet"));
        assert!(guide.contains("nearest active invocation"));
        assert!(guide.contains("cannot discard it"));
        for name in PRIME_INTRINSICS {
            assert!(guide.contains(name), "guide missing {name}");
        }
        for name in PUBLIC_COMPTIME_NAMES {
            assert!(guide.contains(name), "guide missing {name}");
        }

        let mut groups = HashSet::new();
        for module in [BuiltinModule::Intrinsics, BuiltinModule::Comptime] {
            for doc in docs_for_module(module) {
                if groups.insert(doc.group) {
                    assert_eq!(
                        guide.matches(&format!("### {}\n", doc.group)).count(),
                        1,
                        "guide repeats the `{}` category heading",
                        doc.group
                    );
                }
            }
        }
    }

    #[test]
    fn structural_recur_example_marks_its_recursive_newtype() {
        let example = comptime_example(ComptimeBuiltin::StructuralRecur)
            .expect("structural recursion example");
        assert!(example.contains("rec newtype Fuel : . | Fuel"));
    }

    #[test]
    #[cfg(feature = "lsp")]
    fn lsp_markdown_stays_concise() {
        let markdown = builtin_markdown("__structural_recur__").expect("builtin markdown");
        assert!(markdown.contains("ordinary helper call"));
        assert!(!markdown.contains("root bound"));
    }
}
