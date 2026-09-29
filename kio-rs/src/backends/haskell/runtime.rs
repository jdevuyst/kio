//! Haskell backend — runtime-support library.
//!
//! The Haskell backend emits the package's exact host-type marker class plus
//! its package-specific runtime support in the public `<Ns>` facade module.
//! The marker class is exported; the runtime declarations are private helpers
//! the emitter calls instead of inlining. Keeping them in the facade gives each
//! artifact one self-contained Haskell source file while the namespace-derived
//! private names keep distinct package artifacts isolated.
//!
//! The primary body renders Kio at native Haskell types. A namespace-derived
//! private carrier remains the typed universal floor for phase-abstract shapes that cannot
//! be named more precisely; a concrete Kio `host type` never enters it and
//! stays the associated type selected by the host marker. Neither path uses
//! `Data.Dynamic`, `unsafeCoerce`, or a carrier walk.
//! Every emitted function is **monad-polymorphic** (`Monad m => …`); the
//! host record is a value record of `m`-returning functions; the exported
//! surface is `Monad m => …`. Kio is **strict**: host effects sequence
//! through the monad (`>>=` / `do`) for effect order, and bound values are
//! forced (the carrier's scalar fields and strictness helper are strict) for value
//! strictness in a lazy host.
//!
//! What lives here:
//!
//! - the package-specific host marker class and its exact associated types;
//! - the private carrier — the internal universal floor for abstract shapes;
//! - the strictness helper — strict identity (`seq`), forcing a bound value to
//!   weak-head normal form so a `let` / `Seq` binding's value is evaluated
//!   when Kio's strict semantics says it is, not lazily on first use.
//! - projection / sum-tag helpers — total accessors the body uses to read a
//!   product slot / a sum's tag + payload, panicking with a clear message
//!   on a malformed value (an emitter contract violation, never user
//!   input).
//! - the Boolean and scalar projectors used by that floor.
//!
//! The declarations live here as a canonical `&str` template so they ship with
//! the kio-rs binary; [`runtime_declarations`] binds the package's host-type
//! inventory and namespace-derived private names before the facade renderer
//! selects its imports.

/// Render the package-specific host-type class and private runtime support
/// declarations that are appended to the generated facade module.
pub(crate) fn runtime_declarations(
    host_types_class: &str,
    host_types: &super::skin::HaskellHostTypes,
    runtime_names: &super::naming::RuntimeNames,
    standard_names: &super::naming::StandardNames,
) -> String {
    let required_types = &runtime_names.required_host_types;
    let require_host_type = &runtime_names.require_host_type;
    let mut class_members = String::new();
    let mut deprecation_pragmas = String::new();
    let mut private_defaults = String::new();
    let mut missing_instances = String::new();
    let mut required_constraints = Vec::new();
    let declarations: Vec<_> = host_types.declarations().collect();
    if declarations.is_empty() {
        class_members.push_str("  -- This package declares no host types.\n");
    } else {
        for declaration in declarations {
            match declaration.origin {
                crate::backends::boundary_facade::BoundaryHostBindingOrigin::Live => {
                    let missing = runtime_names.missing_host_type(&declaration.assoc_name);
                    class_members.push_str(&format!(
                        "  -- kio-host-type: {}|{}|{}|{}|{}\n",
                        declaration.assoc_name,
                        declaration.module_path,
                        declaration.source_name,
                        declaration
                            .role
                            .map(crate::ast::Role::as_str)
                            .unwrap_or("-"),
                        host_type_param_metadata(&declaration.type_params)
                    ));
                    class_members.push_str(&format!(
                        "  type {} (h :: {}.Type) :: {}\n",
                        declaration.assoc_name,
                        standard_names.data_kind,
                        host_result_kind(&declaration.type_params, standard_names)
                    ));
                    class_members.push_str(&format!(
                        "  type {} h = {missing} h\n",
                        declaration.assoc_name
                    ));
                    private_defaults.push_str(&format!(
                        "data {missing} h{}\n",
                        placeholder_type_params(&declaration.type_params)
                    ));
                    missing_instances.push_str(&format!(
                        "instance {{-# OVERLAPPING #-}}\n  {type_lits}.TypeError ('{type_lits}.Text \"missing Haskell host type binding for `{}/{}`\") => {require_host_type} ({missing} h)\n",
                        declaration.module_path,
                        declaration.source_name,
                        type_lits = standard_names.ghc_type_lits,
                    ));
                    required_constraints.push(format!(
                        "{require_host_type} ({} h)",
                        declaration.assoc_name
                    ));
                }
                crate::backends::boundary_facade::BoundaryHostBindingOrigin::Retained {
                    removed_at_version,
                } => {
                    let placeholder = runtime_names.deprecated_host_type(&declaration.assoc_name);
                    let constructor =
                        runtime_names.deprecated_host_constructor(&declaration.assoc_name);
                    class_members.push_str(&format!(
                        "  -- Removed in signature v{removed_at_version}; retained with a private default.\n"
                    ));
                    class_members.push_str(&format!(
                        "  type {} (h :: {}.Type) :: {}\n",
                        declaration.assoc_name,
                        standard_names.data_kind,
                        host_result_kind(&declaration.type_params, standard_names)
                    ));
                    class_members.push_str(&format!(
                        "  type {} h = {placeholder} h\n",
                        declaration.assoc_name
                    ));
                    private_defaults.push_str(&format!(
                        "data {placeholder} h{} = {constructor}\n",
                        placeholder_type_params(&declaration.type_params)
                    ));
                    deprecation_pragmas.push_str(&format!(
                        "{{-# DEPRECATED type {} \"host type `{}/{}` removed at signature v{removed_at_version}\" #-}}\n",
                        declaration.assoc_name, declaration.module_path, declaration.source_name
                    ));
                }
            }
        }
    }

    let enforcement = if required_constraints.is_empty() {
        private_defaults
    } else {
        let constraints = balanced_constraint_tree(&required_constraints, 2);
        // GHC instance matching accepts a selected type variable immediately,
        // unlike closed-family apartness. The universal instance is incoherent
        // only so a stuck host family can take that same path: an external
        // family cannot reduce to the private sentinel it cannot name, while an
        // omitted associated equation has already reduced to that sentinel and
        // selects its more-specific error instance.
        format!(
            "{private_defaults}\n\
             class {require_host_type} (actual :: k)\n\
             instance {{-# INCOHERENT #-}} {require_host_type} actual\n\
             {missing_instances}\n\
             type {required_types} (h :: {kind}.Type) =\n\
             \x20 {constraints}\n\n",
            kind = standard_names.data_kind,
        )
    };
    let class_decl = if required_constraints.is_empty() {
        format!(
            "class {host_types_class} (h :: {kind}.Type) where\n{class_members}",
            kind = standard_names.data_kind,
        )
    } else {
        format!(
            "class {required_types} h => {host_types_class} (h :: {kind}.Type) where\n{class_members}",
            kind = standard_names.data_kind,
        )
    };
    format!(
        "{enforcement}{class_decl}{deprecation_pragmas}\n{}",
        render_runtime_body(runtime_names, standard_names)
    )
}

/// Renders an unbounded constraint set without GHC's flat-tuple arity cap,
/// while balancing the tree keeps type-synonym expansion depth logarithmic.
fn balanced_constraint_tree(constraints: &[String], indent: usize) -> String {
    debug_assert!(!constraints.is_empty());
    if let [constraint] = constraints {
        return constraint.clone();
    }

    let midpoint = constraints.len() / 2;
    let left = balanced_constraint_tree(&constraints[..midpoint], indent + 2);
    let right = balanced_constraint_tree(&constraints[midpoint..], indent + 2);
    let padding = " ".repeat(indent);
    format!("( {left}\n{padding}, {right}\n{padding})")
}

fn host_result_kind(
    params: &[crate::ast::TypeParam],
    standard_names: &super::naming::StandardNames,
) -> String {
    vec![format!("{}.Type", standard_names.data_kind); host_type_arity(params) + 1].join(" -> ")
}

fn placeholder_type_params(params: &[crate::ast::TypeParam]) -> String {
    (0..host_type_arity(params))
        .map(|index| format!(" p{index}"))
        .collect()
}

fn host_type_param_metadata(params: &[crate::ast::TypeParam]) -> String {
    let arity = host_type_arity(params);
    if arity == 0 {
        return "0".to_owned();
    }

    format!("{arity}:{}", vec!["0"; arity].join(","))
}

fn host_type_arity(params: &[crate::ast::TypeParam]) -> usize {
    assert!(
        params
            .iter()
            .all(|param| param.effective_kind() == crate::ast::Kind::Star),
        "host type parameters must have kind `*` before Haskell emission"
    );
    params.len()
}

/// Substitute compiler-owned private names into the fixed runtime template.
/// Package source and host metadata never enter this replacement path.
fn render_runtime_body(
    names: &super::naming::RuntimeNames,
    standard_names: &super::naming::StandardNames,
) -> String {
    let substitutions = [
        ("@DATA_KIND@", standard_names.data_kind.as_str()),
        ("@DATA_TEXT@", standard_names.data_text.as_str()),
        ("@DATA_IO_REF@", standard_names.data_io_ref.as_str()),
        ("@OPAQUE@", names.opaque.as_str()),
        ("@UNIT@", names.unit.as_str()),
        ("@INT@", names.int.as_str()),
        ("@DOUBLE@", names.double.as_str()),
        ("@TEXT@", names.text.as_str()),
        ("@BOOL@", names.bool_.as_str()),
        ("@PRODUCT@", names.product.as_str()),
        ("@SUM@", names.sum.as_str()),
        ("@FUNCTION@", names.function.as_str()),
        ("@FOREIGN@", names.foreign.as_str()),
        ("@FORCE@", names.force.as_str()),
        ("@CALL_FUNCTION@", names.call_function.as_str()),
        ("@PROJECT@", names.project.as_str()),
        ("@PROJECT_HEAD@", names.project_head.as_str()),
        ("@PROJECT_TAIL@", names.project_tail.as_str()),
        ("@MATCH_TAG@", names.match_tag.as_str()),
        ("@AS_BOOL@", names.as_bool.as_str()),
        ("@AS_INT@", names.as_int.as_str()),
        ("@AS_DOUBLE@", names.as_double.as_str()),
        ("@AS_TEXT@", names.as_text.as_str()),
        ("@FOREIGN_REF@", names.foreign_ref.as_str()),
    ];
    let mut rendered = RUNTIME_SUPPORT_BODY.to_owned();
    for (placeholder, name) in substitutions {
        rendered = rendered.replace(placeholder, name);
    }
    assert!(
        !rendered.contains('@'),
        "Haskell runtime template contains an unsubstituted private name"
    );
    rendered
}

/// The native universal value model used only for representation shapes
/// whose exact type is abstract at this phase. Concrete host types stay on
/// the native path and never enter this value model.
const RUNTIME_SUPPORT_BODY: &str = r#"-- The private carrier is the internal floor for shapes whose exact Haskell
-- type is unavailable at this phase. Concrete Kio host types remain the associated
-- types selected by the package's host marker and never enter this ADT.
-- Unlike the erased
-- carrier of Go's `interface{}` / Swift's `Any`, this is a closed Haskell
-- ADT the body pattern-matches natively: products are a native slot
-- vector, sums a native tag + payload, scalars native Haskell primitives
-- inside typed constructors (no Data.Dynamic, no unsafeCoerce). A closure
-- is a native function from the private carrier to a monadic private carrier.
--
-- The scalar fields are strict (`!`) so a bound scalar is forced to its
-- value, matching Kio's strict evaluation in Haskell's lazy host.
--
-- The foreign-reference constructor carries a host-owned opaque reference —
-- the universal floor's "host-ref" value. The body never inspects it; it
-- shuttles the handle between host-fn calls. The
-- payload is an `IORef` over the erased element list: well-kinded for any
-- `m` (an `IORef` value can be held purely; only the host, running in its
-- own monad, reads / writes it), so the body stays monad-polymorphic.
data @OPAQUE@ (h :: @DATA_KIND@.Type) m
  = @UNIT@
  | @INT@      !Integer
  | @DOUBLE@   !Double
  | @TEXT@     !@DATA_TEXT@.Text
  | @BOOL@     !Bool
  | @PRODUCT@  ![@OPAQUE@ h m]
  | @SUM@      !Int (@OPAQUE@ h m)
  | @FUNCTION@ (@OPAQUE@ h m -> m (@OPAQUE@ h m))
  | @FOREIGN@  (@DATA_IO_REF@.IORef [@OPAQUE@ h m])

-- The strictness helper imposes Kio's value strictness on a bound value in
-- Haskell's lazy host: it forces its argument to weak-head normal form before
-- returning it. A strict `let` / `Seq` binding forces its value once by
-- name rather than re-spelling `seq` at every site.
@FORCE@ :: @OPAQUE@ h m -> @OPAQUE@ h m
@FORCE@ !x = x

-- The application helper applies a closure value to an argument. A
-- non-function callee is an emitter contract violation (a call site always holds a function
-- value here), so a miss is a bug, not user input.
@CALL_FUNCTION@ :: @OPAQUE@ h m -> @OPAQUE@ h m -> m (@OPAQUE@ h m)
@CALL_FUNCTION@ v x = case v of
  @FUNCTION@ f -> f x
  _ -> error "kio: application of a non-function value"

-- The projection helper reads slot `i` of an arity-`n` product. A product is
-- carried nested-binary (the same representation the JS / Go targets use, and the
-- shape the recovery passes' `__pair__` / `__fst__` / `__snd__` chains
-- assume): an arity-`n` product is represented by `[head, tail]` cons cells,
-- the tail itself the arity-`(n-1)` remainder, the last slot held bare. So
-- slot `i` is reached by peeling the tail (`[1]`) `i` times, then taking
-- the head (`[0]`) unless `i` is the final slot. This navigates a value
-- whose own nesting (e.g. `(a, (b, c))` built from a `__pair__` over a
-- bound sub-product) is shallower than the flat spine arity, the case a
-- flat slot-vector projection could not express. A malformed value is an
-- emitter contract violation (a bug, not user input) — panic with a clear
-- message.
@PROJECT@ :: Int -> Int -> @OPAQUE@ h m -> @OPAQUE@ h m
@PROJECT@ i n v
  | i + 1 < n = @PROJECT_HEAD@ (peelTail i v)
  | otherwise = peelTail i v
  where
    peelTail 0 x = x
    peelTail k x = peelTail (k - 1) (@PROJECT_TAIL@ x)

@PROJECT_HEAD@ :: @OPAQUE@ h m -> @OPAQUE@ h m
@PROJECT_HEAD@ v = case v of
  @PRODUCT@ (a : _) -> a
  _ -> error "kio: product projection of a non-product value"

@PROJECT_TAIL@ :: @OPAQUE@ h m -> @OPAQUE@ h m
@PROJECT_TAIL@ v = case v of
  @PRODUCT@ [_, t] -> t
  _ -> error "kio: product projection of a non-product value"

-- The sum-tag helper reads a sum's (tag, payload). Like projection, a non-sum
-- value is an emitter contract violation.
@MATCH_TAG@ :: @OPAQUE@ h m -> (Int, @OPAQUE@ h m)
@MATCH_TAG@ v = case v of
  @SUM@ t p -> (t, p)
  _ -> error "kio: match on a non-sum value"

-- The boolean projector narrows the private carrier for an if/else decision.
@AS_BOOL@ :: @OPAQUE@ h m -> Bool
@AS_BOOL@ v = case v of
  @BOOL@ b -> b
  _ -> error "kio: expected a boolean value"

-- The scalar projectors convert a private scalar back to its
-- native Haskell primitive — used by the FFI skin when a value crosses to
-- the typed host boundary.
@AS_INT@ :: @OPAQUE@ h m -> Integer
@AS_INT@ v = case v of
  @INT@ n -> n
  _ -> error "kio: expected an integer value"

@AS_DOUBLE@ :: @OPAQUE@ h m -> Double
@AS_DOUBLE@ v = case v of
  @DOUBLE@ d -> d
  _ -> error "kio: expected a floating-point value"

@AS_TEXT@ :: @OPAQUE@ h m -> @DATA_TEXT@.Text
@AS_TEXT@ v = case v of
  @TEXT@ s -> s
  _ -> error "kio: expected a string value"

-- The foreign-reference helper projects the host-owned IORef back out of a
-- foreign handle. Any other value is a contract violation (the canonical
-- array host fns only ever receive a handle they built), so a miss is a
-- bug, not user input.
@FOREIGN_REF@ :: @OPAQUE@ h m -> @DATA_IO_REF@.IORef [@OPAQUE@ h m]
@FOREIGN_REF@ v = case v of
  @FOREIGN@ r -> r
  _ -> error "kio: expected a foreign (array) handle"
"#;

#[cfg(test)]
mod tests {
    use crate::ast::{Kind, TypeParam};
    use crate::backends::haskell::naming::{RuntimeNames, StandardNames};
    use crate::span::Span;

    use super::{host_type_param_metadata, render_runtime_body};

    fn param(name: &str) -> TypeParam {
        TypeParam {
            name: name.to_owned(),
            span: Span::new(0, 0),
            kind: None,
        }
    }

    #[test]
    fn host_type_metadata_preserves_ordinary_parameter_arity() {
        assert_eq!(host_type_param_metadata(&[]), "0");
        assert_eq!(host_type_param_metadata(&[param("A")]), "1:0");
        assert_eq!(host_type_param_metadata(&[param("A"), param("B")]), "2:0,0");
    }

    #[test]
    fn runtime_template_renders_every_namespace_derived_private_name() {
        let names = RuntimeNames::new("Foo.Runtime");
        let standard_names = StandardNames::new("Foo.Runtime");
        let rendered = render_runtime_body(&names, &standard_names);

        for name in [
            &names.opaque,
            &names.unit,
            &names.int,
            &names.double,
            &names.text,
            &names.bool_,
            &names.product,
            &names.sum,
            &names.function,
            &names.foreign,
            &names.force,
            &names.call_function,
            &names.project,
            &names.project_head,
            &names.project_tail,
            &names.match_tag,
            &names.as_bool,
            &names.as_int,
            &names.as_double,
            &names.as_text,
            &names.foreign_ref,
            &standard_names.data_kind,
            &standard_names.data_text,
            &standard_names.data_io_ref,
        ] {
            assert!(
                rendered.contains(name),
                "missing private runtime name {name}"
            );
        }
        assert!(!rendered.contains('@'));
        assert!(!rendered.contains("data KioOpaque"));
    }

    #[test]
    #[should_panic(expected = "host type parameters must have kind `*` before Haskell emission")]
    fn host_type_metadata_rejects_an_unchecked_higher_kind_parameter() {
        let mut higher = param("F");
        higher.kind = Some(Kind::arrow_chain(1));
        let _ = host_type_param_metadata(&[higher]);
    }
}
