//! Runner protocol selection.
//!
//! A protocol is the runner's one named semantic authority. It fixes the
//! package's exact host types and functions, the native fixtures and bodies
//! that implement them, the exports the runner drives, and whether the
//! artifact is only compiled, instantiated, or invoked. A test selects one
//! protocol; it never supplements that protocol with a second inventory.
//!
//! The runner reads no package source and no signatures, members, or shapes
//! from `kio build` output. A drifted FFI therefore stops the runner's
//! protocol-owned host from compiling instead of teaching the runner the
//! producer's answer. Protocol definitions may share internal fragments, but
//! every accepted protocol name resolves to one complete [`ProtocolContract`].
//!
//! ## Main protocols
//!
//! The default-main catch-all and capability-superset tiers are retired.
//! Goldens and POC cases that invoke exported `main` select an exact named
//! contract. Majority contracts keep concise family names such as
//! `testapi-print` or `testapi-compute`; narrower signatures use sharpened
//! names rather than filtering a larger environment per case.
//!
//! ## Roundtrip protocols
//!
//! The `export-*-roundtrip` / `host-*-roundtrip` protocols pin a bespoke
//! environment and export driver for one FFI-surface regression.
//!
//! ## The coexist protocol
//!
//! [`RunnerProtocol::Coexist`] is the one **two-artifact** protocol: it
//! takes exactly two positional output directories (every other
//! protocol takes exactly one) and drives one host program that hosts
//! **both** packages simultaneously — the executable witness for
//! `specs/backends/README.md` § The package facade § Coexistence. Its
//! fixed contract: each package declares env `{print}` in module
//! `greeter` and exports `greeter/main.main` (the namespace entry
//! shape) plus the same-shaped `greeter/main.pair() -> (I32 & String)`;
//! the driver instantiates both packages — the first behind a host
//! whose `print` prefixes `first: `, the second behind one prefixing
//! `second: ` (positional, so the pinned output is backend-independent)
//! — calls the first package's `main`, then the second's, then the
//! first's again, and then reads `pair()`'s positional product from
//! both in the same first → second → first order, printing
//! `<label> pair: <n> <s>` lines. The interleaved greetings prove
//! instance isolation on top of the link/compile-level symbol
//! distinctness that hosting two branded artifacts in one program
//! already proves; the `pair` reads add the structural-facade half —
//! both packages expose the same positional product shape, so
//! destructuring both in one program proves their boundary
//! representations coexist under the per-package namespace rule.
//!
//! ## The `testapi` namespace
//!
//! A subset of the protocols are **`testapi`-conformed**: their golden
//! re-roots its entire test-facing surface under one fixed `testapi`
//! namespace — the env it provides *and* the exports the runner calls.
//! The runner bakes the namespace into its canonical knowledge: no
//! `--host-module` run.arg, no source / build read (the decoupling red lines).
//! Each host binding carries its exact declaring module, host types
//! conventionally sit at the `testapi` root, and the program entry is the
//! `main` fn in `testapi/main`, reached through each backend's published
//! facade selectors.
//! [`RunnerProtocol::is_testapi`] marks the conformed protocols. A
//! protocol's testapi-ness is part of its fixed contract — `testapi`
//! signals a test-corpus convention, not a language concept.

#[cfg(feature = "rust")]
use crate::host_api::TraitMethod;

/// What the runner does after locating the selected backend artifact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProtocolExecution {
    /// Compile or load the emitted artifact without constructing its host.
    CompileOnly,
    /// Construct the protocol-owned host and package, but call no export.
    ConstructOnly,
    /// Construct the package and run one fixed export driver.
    Invoke(ExportDriver),
}

/// The fixed export-driving program selected by a protocol.
///
/// Variants are semantic identities, not backend source snippets. Each
/// backend renders the same identity using its own facade syntax.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExportDriver {
    /// Invoke the `main` leaf in this exact declaring module.
    Main {
        module: &'static str,
    },
    NamespaceRoundtrip,
    CallbackRoundtrip,
    ModuleRoundtrip,
    MultilabelRoundtrip,
    PolyRoundtrip,
    PolyCallbackRoundtrip,
    StructuralRoundtrip,
    ScalarRoundtrip,
    HostOwnedRoundtrip,
    CallableSlotsRoundtrip,
    RustCallbackAliases,
    FunctorDictRoundtrip,
    HostExistentialRoundtrip,
    PositionalProductRoundtrip,
    TypeRoundtrip,
    CurriedFacade,
    WideCallable,
    NewtypeSumRoundtrip,
    NewtypeScalarRoundtrip,
    NewtypeVisibilityFacade,
    NestedProductRoundtrip,
    CompoundInputOnce,
    NewtypeIgnoredArgumentRoundtrip,
    RecursiveNewtypeBoundary,
    NestedCurriedRoundtrip,
    HostSubstitutedUnitCallback,
    ReturnedForallCallByValue,
    FacadeSelectorCollisions,
    PublicWordNames,
    ModuleAliasScopeCollision,
    Coexist,
}

/// The runner-owned native representation of one exact host type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostTypeFixture {
    Role(RoleFixture),
    /// A role-bearing type whose native fixture should be distinct from the
    /// canonical primitive on targets whose public host contract lets the host
    /// select an exact associated type. Targets whose ABI erases that choice
    /// retain their normal role representation.
    SelectedRole(RoleFixture),
    Array,
    Box,
    Token,
    Scalar,
}

/// A role-bearing host type's exact literal fixture.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RoleFixture {
    Bool,
    I8,
    I16,
    I32,
    I64,
    I128,
    U8,
    U16,
    U32,
    U64,
    U128,
    F32,
    F64,
    String,
}

impl RoleFixture {
    // This shared source is included independently by dynamic runner binaries,
    // which carry role identity but do not need a native spelling.
    #[allow(dead_code)]
    pub const fn role(self) -> &'static str {
        match self {
            Self::Bool => "bool",
            Self::I8 => "i8",
            Self::I16 => "i16",
            Self::I32 => "i32",
            Self::I64 => "i64",
            Self::I128 => "i128",
            Self::U8 => "u8",
            Self::U16 => "u16",
            Self::U32 => "u32",
            Self::U64 => "u64",
            Self::U128 => "u128",
            Self::F32 => "f32",
            Self::F64 => "f64",
            Self::String => "str",
        }
    }
}

/// One exact host-type declaration in a protocol.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostTypeBinding {
    pub module: &'static str,
    pub leaf: &'static str,
    pub type_arity: u8,
    pub fixture: HostTypeFixture,
}

impl HostTypeBinding {
    pub const fn role(module: &'static str, leaf: &'static str, fixture: RoleFixture) -> Self {
        Self {
            module,
            leaf,
            type_arity: 0,
            fixture: HostTypeFixture::Role(fixture),
        }
    }

    pub const fn selected_role(
        module: &'static str,
        leaf: &'static str,
        fixture: RoleFixture,
    ) -> Self {
        Self {
            module,
            leaf,
            type_arity: 0,
            fixture: HostTypeFixture::SelectedRole(fixture),
        }
    }

    pub const fn opaque(
        module: &'static str,
        leaf: &'static str,
        type_arity: u8,
        fixture: HostTypeFixture,
    ) -> Self {
        Self {
            module,
            leaf,
            type_arity,
            fixture,
        }
    }
}

/// The exact qualified identity of a host type referenced by a host-function
/// signature. Native fixture equality is deliberately insufficient: distinct
/// Kio host types may share one host-language representation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostTypeIdentity {
    pub module: &'static str,
    pub leaf: &'static str,
}

impl HostTypeIdentity {
    pub const fn new(module: &'static str, leaf: &'static str) -> Self {
        Self { module, leaf }
    }
}

/// One exact role-bearing host type referenced by a host-function signature.
///
/// The identity selects the declaration; the fixture records the native value
/// shape that the runner supplies for that declaration. Two declarations may
/// deliberately share a fixture without becoming interchangeable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostRoleRef {
    pub identity: HostTypeIdentity,
    pub fixture: RoleFixture,
}

impl HostRoleRef {
    pub const fn new(module: &'static str, leaf: &'static str, fixture: RoleFixture) -> Self {
        Self {
            identity: HostTypeIdentity::new(module, leaf),
            fixture,
        }
    }

    /// Resolve this reference against one complete protocol contract.
    ///
    /// Protocol construction is static, so a missing, duplicate, or
    /// fixture-mismatched declaration is an internal registry bug.
    #[allow(dead_code)] // Static runners resolve native signatures; dyn uses the identity directly.
    pub fn resolve(self, host_types: &[HostTypeBinding]) -> &HostTypeBinding {
        let mut matches = host_types.iter().filter(|candidate| {
            candidate.module == self.identity.module && candidate.leaf == self.identity.leaf
        });
        let binding = matches.next().unwrap_or_else(|| {
            unreachable!(
                "protocol host function references undeclared role type `{}/{}`",
                self.identity.module, self.identity.leaf
            )
        });
        assert!(
            matches.next().is_none(),
            "protocol host function role type `{}/{}` resolves more than once",
            self.identity.module,
            self.identity.leaf
        );
        let actual = match binding.fixture {
            HostTypeFixture::Role(fixture) | HostTypeFixture::SelectedRole(fixture) => fixture,
            other => unreachable!(
                "protocol host function role type `{}/{}` resolves to non-role fixture {other:?}",
                self.identity.module, self.identity.leaf
            ),
        };
        assert_eq!(
            actual, self.fixture,
            "protocol host function role type `{}/{}` has mismatched fixture",
            self.identity.module, self.identity.leaf
        );
        binding
    }
}

/// The backend-independent implementation promised for one host function.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostFnBodyKind {
    Print {
        string: HostRoleRef,
    },
    Eprint {
        string: HostRoleRef,
    },
    Exit {
        status_i32: HostRoleRef,
    },
    ReadAsciiLine {
        string: HostRoleRef,
    },
    StringConcat {
        string: HostRoleRef,
    },
    StringEq {
        string: HostRoleRef,
        bool_: HostRoleRef,
    },
    StringLen {
        string: HostRoleRef,
        index: HostRoleRef,
    },
    StringSlice {
        string: HostRoleRef,
        index: HostRoleRef,
    },
    StringCodeAt {
        string: HostRoleRef,
        index: HostRoleRef,
    },
    Loop,
    NumericToString {
        value: HostRoleRef,
        string: HostRoleRef,
    },
    BoolToString {
        bool_: HostRoleRef,
        string: HostRoleRef,
    },
    PrintI32 {
        value: HostRoleRef,
    },
    StringToInt {
        string: HostRoleRef,
        int: HostRoleRef,
    },
    Arithmetic {
        operation: &'static str,
        number: HostRoleRef,
    },
    FloatArithmetic {
        operation: &'static str,
        number: HostRoleRef,
    },
    Compare {
        operation: &'static str,
        number: HostRoleRef,
        bool_: HostRoleRef,
    },
    Array {
        operation: &'static str,
        array: HostTypeIdentity,
        index: Option<HostRoleRef>,
    },
    MakeScalar {
        string: HostRoleRef,
        scalar: HostTypeIdentity,
    },
    ScalarOf {
        value: HostRoleRef,
        scalar: HostTypeIdentity,
    },
    ScalarAs {
        value: HostRoleRef,
        scalar: HostTypeIdentity,
    },
    ScalarIsTrue {
        scalar: HostTypeIdentity,
        bool_: HostRoleRef,
    },
    MakeToken {
        value_i32: HostRoleRef,
        token: HostTypeIdentity,
    },
    TokenValue {
        token: HostTypeIdentity,
        value_i32: HostRoleRef,
    },
    BoxGet {
        box_type: HostTypeIdentity,
    },
    BoxMake {
        box_type: HostTypeIdentity,
    },
    CallStep {
        i32: HostRoleRef,
        string: HostRoleRef,
        bool_: HostRoleRef,
    },
    MakePairCallback {
        i32: HostRoleRef,
        string: HostRoleRef,
    },
    MakeStep {
        i32: HostRoleRef,
    },
    ApplyPoly {
        string: HostRoleRef,
    },
    MakePairStructural {
        i32: HostRoleRef,
        string: HostRoleRef,
    },
    /// Produce a fixed pair and print one event for each invocation.
    ProducePair {
        i32: HostRoleRef,
        string: HostRoleRef,
    },
    SumToString {
        i32: HostRoleRef,
        string: HostRoleRef,
    },
    RoundFunctor,
    ObservePacked {
        i32: HostRoleRef,
    },
    RoundPicker,
    RoundPolyThunk,
    RoundPolyUnitSlot,
    StagedSecond {
        string: HostRoleRef,
    },
    /// Round-trip a genuinely nested `String -> String -> String` value.
    /// Each function layer remains a separate host call.
    NestedCurriedRoundtrip {
        string: HostRoleRef,
    },
    /// Invoke `Callback(Unit)` where `Callback[A] = A -> Text`.
    /// Substitution preserves the callback's already-planned value slot, so
    /// every host calls this callback with one explicit Unit value.
    InvokeSubstitutedUnitCallback {
        text: HostRoleRef,
    },
    /// Return one rank-N value whose only protocol-owned observation is at
    /// Unit on the first call, then fail on the second call. The first host
    /// call is performed once before the returned type application stages are
    /// used twice; the second failure must propagate back through the package.
    ReturnedForallUnit,
    /// Emit one fixed trace line from a zero-slot Unit -> Unit host function.
    TraceUnit {
        text: &'static str,
    },
    /// `[A](A)[B](Unit) -> Unit`: the generic first argument retains one
    /// public value slot when instantiated at Unit, while the literal Unit
    /// argument remains a zero-slot group.
    StagedUnitCall,
    UnreachableI32Print {
        i32: HostRoleRef,
    },
}

impl HostFnBodyKind {
    const fn print(string: HostRoleRef) -> Self {
        Self::Print { string }
    }

    const fn eprint(string: HostRoleRef) -> Self {
        Self::Eprint { string }
    }

    const fn exit(status_i32: HostRoleRef) -> Self {
        Self::Exit { status_i32 }
    }

    const fn read_ascii_line(string: HostRoleRef) -> Self {
        Self::ReadAsciiLine { string }
    }

    const fn string_concat(string: HostRoleRef) -> Self {
        Self::StringConcat { string }
    }

    const fn string_eq(string: HostRoleRef, bool_: HostRoleRef) -> Self {
        Self::StringEq { string, bool_ }
    }

    const fn string_len(string: HostRoleRef, index: HostRoleRef) -> Self {
        Self::StringLen { string, index }
    }

    const fn string_slice(string: HostRoleRef, index: HostRoleRef) -> Self {
        Self::StringSlice { string, index }
    }

    const fn string_code_at(string: HostRoleRef, index: HostRoleRef) -> Self {
        Self::StringCodeAt { string, index }
    }

    const fn numeric_to_string(value: HostRoleRef, string: HostRoleRef) -> Self {
        Self::NumericToString { value, string }
    }

    const fn bool_to_string(bool_: HostRoleRef, string: HostRoleRef) -> Self {
        Self::BoolToString { bool_, string }
    }

    const fn print_i32(value: HostRoleRef) -> Self {
        Self::PrintI32 { value }
    }

    const fn string_to_int(string: HostRoleRef, int: HostRoleRef) -> Self {
        Self::StringToInt { string, int }
    }

    const fn arithmetic(operation: &'static str, number: HostRoleRef) -> Self {
        Self::Arithmetic { operation, number }
    }

    const fn float_arithmetic(operation: &'static str, number: HostRoleRef) -> Self {
        Self::FloatArithmetic { operation, number }
    }

    const fn compare(operation: &'static str, number: HostRoleRef, bool_: HostRoleRef) -> Self {
        Self::Compare {
            operation,
            number,
            bool_,
        }
    }

    const fn array(
        operation: &'static str,
        array: HostTypeIdentity,
        index: Option<HostRoleRef>,
    ) -> Self {
        Self::Array {
            operation,
            array,
            index,
        }
    }

    const fn make_scalar(string: HostRoleRef, scalar: HostTypeIdentity) -> Self {
        Self::MakeScalar { string, scalar }
    }

    const fn scalar_of(value: HostRoleRef, scalar: HostTypeIdentity) -> Self {
        Self::ScalarOf { value, scalar }
    }

    const fn scalar_as(value: HostRoleRef, scalar: HostTypeIdentity) -> Self {
        Self::ScalarAs { value, scalar }
    }

    const fn scalar_is_true(scalar: HostTypeIdentity, bool_: HostRoleRef) -> Self {
        Self::ScalarIsTrue { scalar, bool_ }
    }

    const fn make_token(value_i32: HostRoleRef, token: HostTypeIdentity) -> Self {
        Self::MakeToken { value_i32, token }
    }

    const fn token_value(token: HostTypeIdentity, value_i32: HostRoleRef) -> Self {
        Self::TokenValue { token, value_i32 }
    }

    const fn call_step(i32: HostRoleRef, string: HostRoleRef, bool_: HostRoleRef) -> Self {
        Self::CallStep { i32, string, bool_ }
    }

    const fn make_pair_callback(i32: HostRoleRef, string: HostRoleRef) -> Self {
        Self::MakePairCallback { i32, string }
    }

    const fn make_step(i32: HostRoleRef) -> Self {
        Self::MakeStep { i32 }
    }

    const fn apply_poly(string: HostRoleRef) -> Self {
        Self::ApplyPoly { string }
    }

    const fn make_pair_structural(i32: HostRoleRef, string: HostRoleRef) -> Self {
        Self::MakePairStructural { i32, string }
    }

    const fn sum_to_string(i32: HostRoleRef, string: HostRoleRef) -> Self {
        Self::SumToString { i32, string }
    }

    const fn unreachable_i32_print(i32: HostRoleRef) -> Self {
        Self::UnreachableI32Print { i32 }
    }

    /// Every distinct role-bearing declaration used by this signature.
    ///
    /// Repeated argument occurrences use the same reference and appear once.
    /// Three is the largest distinct-role set in the current body algebra.
    #[allow(dead_code)]
    pub const fn role_refs(self) -> [Option<HostRoleRef>; 3] {
        match self {
            Self::Print { string }
            | Self::Eprint { string }
            | Self::ReadAsciiLine { string }
            | Self::StringConcat { string }
            | Self::ApplyPoly { string }
            | Self::StagedSecond { string }
            | Self::NestedCurriedRoundtrip { string } => [Some(string), None, None],
            Self::InvokeSubstitutedUnitCallback { text } => [Some(text), None, None],
            Self::Exit { status_i32 } => [Some(status_i32), None, None],
            Self::StringEq { string, bool_ } => [Some(string), Some(bool_), None],
            Self::StringLen { string, index }
            | Self::StringSlice { string, index }
            | Self::StringCodeAt { string, index } => [Some(string), Some(index), None],
            Self::NumericToString { value, string } => [Some(value), Some(string), None],
            Self::BoolToString { bool_, string } => [Some(bool_), Some(string), None],
            Self::PrintI32 { value } => [Some(value), None, None],
            Self::StringToInt { string, int } => [Some(string), Some(int), None],
            Self::Arithmetic { number, .. } | Self::FloatArithmetic { number, .. } => {
                [Some(number), None, None]
            }
            Self::Compare { number, bool_, .. } => [Some(number), Some(bool_), None],
            Self::Array { index, .. } => [index, None, None],
            Self::MakeScalar { string, .. } => [Some(string), None, None],
            Self::ScalarOf { value, .. } | Self::ScalarAs { value, .. } => {
                [Some(value), None, None]
            }
            Self::ScalarIsTrue { bool_, .. } => [Some(bool_), None, None],
            Self::MakeToken { value_i32, .. } | Self::TokenValue { value_i32, .. } => {
                [Some(value_i32), None, None]
            }
            Self::CallStep { i32, string, bool_ } => [Some(i32), Some(string), Some(bool_)],
            Self::MakePairCallback { i32, string }
            | Self::MakePairStructural { i32, string }
            | Self::ProducePair { i32, string }
            | Self::SumToString { i32, string } => [Some(i32), Some(string), None],
            Self::MakeStep { i32 }
            | Self::UnreachableI32Print { i32 }
            | Self::ObservePacked { i32 } => [Some(i32), None, None],
            Self::Loop
            | Self::BoxGet { .. }
            | Self::BoxMake { .. }
            | Self::RoundFunctor
            | Self::RoundPicker
            | Self::RoundPolyThunk
            | Self::RoundPolyUnitSlot
            | Self::ReturnedForallUnit
            | Self::TraceUnit { .. }
            | Self::StagedUnitCall => [None, None, None],
        }
    }

    /// The exact opaque host type referenced by this body/signature shape,
    /// when it has one. Role-bearing and structural slots need no nominal
    /// host-type identity at the native boundary.
    #[cfg(test)]
    pub const fn referenced_host_type(self) -> Option<HostTypeIdentity> {
        match self {
            Self::Array { array, .. } => Some(array),
            Self::MakeScalar { scalar, .. }
            | Self::ScalarOf { scalar, .. }
            | Self::ScalarAs { scalar, .. }
            | Self::ScalarIsTrue { scalar, .. } => Some(scalar),
            Self::MakeToken { token, .. } | Self::TokenValue { token, .. } => Some(token),
            Self::BoxGet { box_type } | Self::BoxMake { box_type } => Some(box_type),
            _ => None,
        }
    }
}

impl HostFnBodyKind {
    /// Render the exact canonical Kio' descriptor signature derived by the
    /// runtime loader for this body shape.
    ///
    /// Host-type identity comes exclusively from the structured references in
    /// this value. Native fixture equality is intentionally irrelevant here:
    /// two role-bearing declarations may share one host representation without
    /// becoming interchangeable Kio host types.
    #[allow(dead_code)] // Consumed only by the dyn-load-prime runner binary.
    pub fn canonical_prime_signature(self) -> String {
        fn host(identity: HostTypeIdentity) -> String {
            format!("h{{{}.{}}}", identity.module, identity.leaf)
        }

        fn role(reference: HostRoleRef) -> String {
            host(reference.identity)
        }

        match self {
            Self::Print { string } | Self::Eprint { string } => {
                format!("({}) -> .", role(string))
            }
            Self::Exit { status_i32 } => format!("({}) -> !", role(status_i32)),
            Self::ReadAsciiLine { string } => format!("() -> ({} | .)", role(string)),
            Self::StringConcat { string } => {
                let string = role(string);
                format!("({string} & {string}) -> {string}")
            }
            Self::StringEq { string, bool_ } => {
                let string = role(string);
                format!("({string} & {string}) -> {}", role(bool_))
            }
            Self::StringLen { string, index } => {
                format!("({}) -> {}", role(string), role(index))
            }
            Self::StringSlice { string, index } => {
                let index = role(index);
                format!(
                    "({} & ({index} & {index})) -> {}",
                    role(string),
                    role(string)
                )
            }
            Self::StringCodeAt { string, index } => {
                let index = role(index);
                format!("({} & {index}) -> ({index} | .)", role(string))
            }
            Self::Loop => "[#0] [#1] ((#0 -> (#0 | #1)) & #0) -> #1".to_owned(),
            Self::NumericToString { value, string } => {
                format!("({}) -> {}", role(value), role(string))
            }
            Self::BoolToString { bool_, string } => {
                format!("({}) -> {}", role(bool_), role(string))
            }
            Self::PrintI32 { value } | Self::UnreachableI32Print { i32: value } => {
                format!("({}) -> .", role(value))
            }
            Self::StringToInt { string, int } => {
                format!("({}) -> ({} | .)", role(string), role(int))
            }
            Self::Arithmetic { number, .. } | Self::FloatArithmetic { number, .. } => {
                let number = role(number);
                format!("({number} & {number}) -> {number}")
            }
            Self::Compare { number, bool_, .. } => {
                let number = role(number);
                format!("({number} & {number}) -> {}", role(bool_))
            }
            Self::Array {
                operation,
                array,
                index,
            } => {
                let array = host(array);
                match operation {
                    "make-empty" => format!("[#0] () -> {array}(#0)"),
                    "make-filled" => {
                        format!("[#0] ({} & #0) -> {array}(#0)", role(index.unwrap()))
                    }
                    "len" => format!("[#0] ({array}(#0)) -> {}", role(index.unwrap())),
                    "get" => {
                        format!("[#0] ({array}(#0) & {}) -> #0", role(index.unwrap()))
                    }
                    "set" => format!("[#0] ({array}(#0) & ({} & #0)) -> .", role(index.unwrap())),
                    "push" => format!("[#0] ({array}(#0) & #0) -> ."),
                    "pop-back" => format!("[#0] ({array}(#0)) -> (#0 | .)"),
                    "swap" => {
                        let index = role(index.unwrap());
                        format!("[#0] ({array}(#0) & ({index} & {index})) -> .")
                    }
                    "clear" => format!("[#0] ({array}(#0)) -> ."),
                    "clone" => format!("[#0] ({array}(#0)) -> {array}(#0)"),
                    other => unreachable!("unsupported canonical array operation `{other}`"),
                }
            }
            Self::MakeScalar { string, scalar } => {
                let string = role(string);
                format!("({string} & {string}) -> {}", host(scalar))
            }
            Self::ScalarOf { value, scalar } => {
                format!("({}) -> {}", role(value), host(scalar))
            }
            Self::ScalarAs { value, scalar } => {
                format!("({}) -> (. | {})", host(scalar), role(value))
            }
            Self::ScalarIsTrue { scalar, bool_ } => {
                format!("({}) -> {}", host(scalar), role(bool_))
            }
            Self::MakeToken { value_i32, token } => {
                format!("({}) -> {}", role(value_i32), host(token))
            }
            Self::TokenValue { token, value_i32 } => {
                format!("({}) -> {}", host(token), role(value_i32))
            }
            Self::BoxGet { box_type } => {
                format!("[#0] ({}(#0)) -> #0", host(box_type))
            }
            Self::BoxMake { box_type } => {
                format!("[#0] (#0) -> {}(#0)", host(box_type))
            }
            Self::CallStep { i32, string, bool_ } => {
                let i32 = role(i32);
                format!(
                    "((({i32} & ({} & {})) -> {i32}) & {i32}) -> {i32}",
                    role(string),
                    role(bool_)
                )
            }
            Self::MakePairCallback { i32, string } => {
                let i32 = role(i32);
                format!("(({i32} -> ({i32} & {})) & {i32}) -> {i32}", role(string))
            }
            Self::MakeStep { i32 } => {
                let i32 = role(i32);
                format!("({i32}) -> ({i32} -> {i32})")
            }
            Self::ApplyPoly { string } => {
                format!("([#0] (#0 -> #0)) -> {}", role(string))
            }
            Self::MakePairStructural { i32, string } => {
                let i32 = role(i32);
                let string = role(string);
                format!("({i32} & {string}) -> ({i32} & {string})")
            }
            Self::ProducePair { i32, string } => {
                format!("() -> ({} & {})", role(i32), role(string))
            }
            Self::SumToString { i32, string } => {
                let string = role(string);
                format!("({} | {string}) -> {string}", role(i32))
            }
            Self::RoundFunctor => "(n{testapi/types.Functor}(n{testapi/types.Box})) -> \
                 n{testapi/types.Functor}(n{testapi/types.Box})"
                .to_owned(),
            Self::ObservePacked { i32 } => format!("(n{{testapi/types.Packed}}) -> {}", role(i32)),
            Self::RoundPicker => {
                "(n{testapi/types.Pick_first}) -> n{testapi/types.Pick_first}".to_owned()
            }
            Self::RoundPolyThunk => {
                "(n{testapi/types.Poly_thunk}) -> n{testapi/types.Poly_thunk}".to_owned()
            }
            Self::RoundPolyUnitSlot => {
                "(n{testapi/types.Unit_slot}(.)) -> n{testapi/types.Unit_slot}(.)".to_owned()
            }
            Self::StagedSecond { string } => {
                let string = role(string);
                format!("[#0] ({string}) -> [#1] ({string}) -> {string}")
            }
            Self::NestedCurriedRoundtrip { string } => {
                let string = role(string);
                format!(
                    "(({string} -> ({string} -> {string}))) -> ({string} -> ({string} -> {string}))"
                )
            }
            Self::InvokeSubstitutedUnitCallback { text } => {
                format!("((. -> {})) -> {}", role(text), role(text))
            }
            Self::ReturnedForallUnit => "(.) -> [#0] #0".to_owned(),
            Self::TraceUnit { .. } => "(.) -> .".to_owned(),
            Self::StagedUnitCall => "[#0] (#0) -> [#1] (.) -> .".to_owned(),
        }
    }

    /// Value-slot count for each source value group in the same host
    /// declaration. Type-binder groups are represented in the canonical
    /// signature and do not create runtime application stages.
    #[allow(dead_code)] // Consumed only by the dyn-load-prime runner binary.
    pub fn canonical_prime_group_slots(self) -> &'static [u8] {
        match self {
            Self::ReadAsciiLine { .. } | Self::ProducePair { .. } => &[0],
            Self::StringConcat { .. }
            | Self::StringEq { .. }
            | Self::StringCodeAt { .. }
            | Self::Arithmetic { .. }
            | Self::FloatArithmetic { .. }
            | Self::Compare { .. }
            | Self::MakeScalar { .. }
            | Self::Loop
            | Self::CallStep { .. }
            | Self::MakePairCallback { .. }
            | Self::MakePairStructural { .. } => &[2],
            Self::StringSlice { .. } => &[3],
            Self::Array {
                operation: "make-empty",
                ..
            } => &[0],
            Self::Array {
                operation: "make-filled" | "get" | "push",
                ..
            } => &[2],
            Self::Array {
                operation: "set" | "swap",
                ..
            } => &[3],
            Self::Array {
                operation: "len" | "pop-back" | "clear" | "clone",
                ..
            } => &[1],
            Self::Array {
                operation: other, ..
            } => unreachable!("unsupported canonical array operation `{other}`"),
            Self::Print { .. }
            | Self::Eprint { .. }
            | Self::Exit { .. }
            | Self::StringLen { .. }
            | Self::NumericToString { .. }
            | Self::BoolToString { .. }
            | Self::PrintI32 { .. }
            | Self::StringToInt { .. }
            | Self::ScalarOf { .. }
            | Self::ScalarAs { .. }
            | Self::ScalarIsTrue { .. }
            | Self::MakeToken { .. }
            | Self::TokenValue { .. }
            | Self::BoxGet { .. }
            | Self::BoxMake { .. }
            | Self::MakeStep { .. }
            | Self::ApplyPoly { .. }
            | Self::SumToString { .. }
            | Self::RoundFunctor
            | Self::ObservePacked { .. }
            | Self::RoundPicker
            | Self::RoundPolyThunk
            | Self::RoundPolyUnitSlot
            | Self::NestedCurriedRoundtrip { .. }
            | Self::InvokeSubstitutedUnitCallback { .. }
            | Self::UnreachableI32Print { .. } => &[1],
            Self::StagedSecond { .. } => &[1, 1],
            Self::ReturnedForallUnit | Self::TraceUnit { .. } => &[0],
            Self::StagedUnitCall => &[1, 0],
        }
    }
}

/// One exact host-function declaration and its runner implementation.
///
/// [`HostFnBodyKind`] is the structured operational signature as well as the
/// body recipe: it carries numeric kinds and every nominal host-type identity
/// that affects a target signature. Backends render that structure directly;
/// there is no parallel free-form signature that can drift from it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostFnBinding {
    pub module: &'static str,
    pub leaf: &'static str,
    pub body: HostFnBodyKind,
}

const fn host_fn(module: &'static str, leaf: &'static str, body: HostFnBodyKind) -> HostFnBinding {
    HostFnBinding { module, leaf, body }
}

/// One complete named runner contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProtocolContract {
    pub execution: ProtocolExecution,
    pub host_types: &'static [HostTypeBinding],
    pub host_fns: &'static [HostFnBinding],
    /// Whether the package's complete public test surface is rooted at
    /// `testapi`. This remains distinct from package/artifact identity.
    pub testapi_conformed: bool,
}

const fn main_contract(
    host_types: &'static [HostTypeBinding],
    host_fns: &'static [HostFnBinding],
) -> ProtocolContract {
    ProtocolContract {
        execution: ProtocolExecution::Invoke(ExportDriver::Main {
            module: TESTAPI_MAIN_MODULE,
        }),
        host_types,
        host_fns,
        testapi_conformed: true,
    }
}

const fn invoke_contract(
    driver: ExportDriver,
    host_types: &'static [HostTypeBinding],
    host_fns: &'static [HostFnBinding],
    testapi_conformed: bool,
) -> ProtocolContract {
    ProtocolContract {
        execution: ProtocolExecution::Invoke(driver),
        host_types,
        host_fns,
        testapi_conformed,
    }
}

const fn construct_contract(
    host_types: &'static [HostTypeBinding],
    host_fns: &'static [HostFnBinding],
) -> ProtocolContract {
    ProtocolContract {
        execution: ProtocolExecution::ConstructOnly,
        host_types,
        host_fns,
        testapi_conformed: false,
    }
}

pub const EMPTY_PROTOCOL_NAME: &str = "empty-main";
pub const COMPILE_ONLY_PROTOCOL_NAME: &str = "compile-only";
pub const CONSTRUCT_ONLY_PROTOCOL_NAME: &str = "construct-only";
pub const SAME_LEAF_HOST_LITERAL_PROTOCOL_NAME: &str = "same-leaf-host-literal-roles";
pub const MAIN_BOX_FIXTURE_PROTOCOL_NAME: &str = "main-box-fixture";
pub const EMPTY_API_MAIN_PROTOCOL_NAME: &str = "empty-api-main";
pub const GENERATED_CORE_MAIN_PROTOCOL_NAME: &str = "generated-core-main";
pub const GENERATED_SURFACE_MAIN_PROTOCOL_NAME: &str = "generated-surface-main";
pub const EXPORT_NAMESPACE_ROUNDTRIP_PROTOCOL_NAME: &str = "export-namespace-roundtrip";
pub const EXPORT_CALLBACK_ROUNDTRIP_PROTOCOL_NAME: &str = "export-callback-roundtrip";
pub const EXPORT_MODULE_ROUNDTRIP_PROTOCOL_NAME: &str = "export-module-roundtrip";
pub const EXPORT_MULTILABEL_ROUNDTRIP_PROTOCOL_NAME: &str = "export-multi-label-roundtrip";
pub const EXPORT_POLY_ROUNDTRIP_PROTOCOL_NAME: &str = "export-poly-roundtrip";
pub const EXPORT_POLY_CALLBACK_ROUNDTRIP_PROTOCOL_NAME: &str = "export-poly-callback-roundtrip";
pub const EXPORT_STRUCTURAL_ROUNDTRIP_PROTOCOL_NAME: &str = "export-structural-roundtrip";
pub const EXPORT_SCALAR_ROUNDTRIP_PROTOCOL_NAME: &str = "export-scalar-roundtrip";
pub const EXPORT_HOST_OWNED_ROUNDTRIP_PROTOCOL_NAME: &str = "export-host-owned-roundtrip";
pub const EXPORT_CALLABLE_SLOTS_ROUNDTRIP_PROTOCOL_NAME: &str = "export-callable-slots-roundtrip";
pub const RUST_CALLBACK_ALIASES_PROTOCOL_NAME: &str = "rust-callback-aliases";
pub const EXPORT_FUNCTOR_DICT_ROUNDTRIP_PROTOCOL_NAME: &str = "export-functor-dict-roundtrip";
pub const HOST_EXISTENTIAL_ROUNDTRIP_PROTOCOL_NAME: &str = "host-existential-roundtrip";
pub const EXPORT_POSITIONAL_PRODUCT_ROUNDTRIP_PROTOCOL_NAME: &str =
    "export-positional-product-roundtrip";
pub const EXPORT_TYPE_ROUNDTRIP_PROTOCOL_NAME: &str = "export-type-roundtrip";
pub const EXPORT_CURRIED_FACADE_PROTOCOL_NAME: &str = "export-curried-facade";
pub const EXPORT_WIDE_CALLABLE_PROTOCOL_NAME: &str = "export-wide-callable";
pub const EXPORT_NEWTYPE_SUM_ROUNDTRIP_PROTOCOL_NAME: &str = "export-newtype-sum-roundtrip";
pub const EXPORT_NEWTYPE_SCALAR_ROUNDTRIP_PROTOCOL_NAME: &str = "export-newtype-scalar-roundtrip";
pub const EXPORT_NEWTYPE_IGNORED_ARGUMENT_ROUNDTRIP_PROTOCOL_NAME: &str =
    "export-newtype-ignored-argument-roundtrip";
pub const RECURSIVE_NEWTYPE_BOUNDARY_PROTOCOL_NAME: &str = "recursive-newtype-boundary";
pub const EXPORT_NESTED_PRODUCT_ROUNDTRIP_PROTOCOL_NAME: &str = "export-nested-product-roundtrip";
pub const EXPORT_COMPOUND_INPUT_ONCE_PROTOCOL_NAME: &str = "export-compound-input-once";
pub const NEWTYPE_VISIBILITY_FACADE_PROTOCOL_NAME: &str = "newtype-visibility-facade";
pub const HOST_CALLBACK_RETURN_ROUNDTRIP_PROTOCOL_NAME: &str = "host-callback-return-roundtrip";
pub const HOST_CALLBACK_ROUNDTRIP_PROTOCOL_NAME: &str = "host-callback-roundtrip";
pub const HOST_GENERIC_RETURN_ONLY_ROUNDTRIP_PROTOCOL_NAME: &str =
    "host-generic-return-only-roundtrip";
pub const HOST_GENERIC_TYPE_ROUNDTRIP_PROTOCOL_NAME: &str = "host-generic-type-roundtrip";
pub const HOST_RANKN_ROUNDTRIP_PROTOCOL_NAME: &str = "host-rankn-roundtrip";
pub const HOST_STRUCTURAL_ROUNDTRIP_PROTOCOL_NAME: &str = "host-structural-roundtrip";
pub const HOST_STRUCTURAL_ELAB_ROUNDTRIP_PROTOCOL_NAME: &str = "host-structural-elab-roundtrip";
pub const HOST_TYPE_ROUNDTRIP_PROTOCOL_NAME: &str = "host-type-roundtrip";
pub const HOST_FUNCTOR_DICT_ROUNDTRIP_PROTOCOL_NAME: &str = "host-functor-dict-roundtrip";
pub const HOST_POLY_FUNCTION_NEWTYPE_ROUNDTRIP_PROTOCOL_NAME: &str =
    "host-poly-function-newtype-roundtrip";
pub const HOST_POLY_UNIT_PAYLOAD_ROUNDTRIP_PROTOCOL_NAME: &str = "host-poly-unit-payload-roundtrip";
pub const HOST_INTERLEAVED_STAGE_ROUNDTRIP_PROTOCOL_NAME: &str = "host-interleaved-stage-roundtrip";
pub const NESTED_CURRIED_ROUNDTRIP_PROTOCOL_NAME: &str = "nested-curried-roundtrip";
pub const HOST_SUBSTITUTED_UNIT_CALLBACK_PROTOCOL_NAME: &str = "host-substituted-unit-callback";
pub const RETURNED_FORALL_CALL_BY_VALUE_PROTOCOL_NAME: &str = "returned-forall-call-by-value";
pub const HOST_STAGED_UNIT_CALL_PROTOCOL_NAME: &str = "host-staged-unit-call";
pub const FACADE_SELECTOR_COLLISIONS_PROTOCOL_NAME: &str = "facade-selector-collisions";
pub const PUBLIC_WORD_NAMES_PROTOCOL_NAME: &str = "public-word-names";
pub const MODULE_ALIAS_SCOPE_COLLISION_PROTOCOL_NAME: &str = "module-alias-scope-collision";
pub const ELAB_PROTOCOL_NAME: &str = "elab-main";
pub const ROOT_SCOPED_HOST_ENV_PROTOCOL_NAME: &str = "root-scoped-host-env";
pub const TESTAPI_PRINT_PROTOCOL_NAME: &str = "testapi-print";
pub const TESTAPI_PRINT_MARKED_STRING_PROTOCOL_NAME: &str = "testapi-print-marked-string";
pub const TESTAPI_BARE_COLLECTION_PROTOCOL_NAME: &str = "testapi-bare-collection";
pub const TESTAPI_ARRAY_PROTOCOL_NAME: &str = "testapi-array";
pub const TESTAPI_ARRAY_CLEAR_PROTOCOL_NAME: &str = "testapi-array-clear";
pub const TESTAPI_BIGINT_PROTOCOL_NAME: &str = "testapi-bigint";
pub const TESTAPI_TEXT_PROTOCOL_NAME: &str = "testapi-text";
pub const TESTAPI_COMPUTE_PROTOCOL_NAME: &str = "testapi-compute";
pub const TESTAPI_ARITH_COLLECTION_PROTOCOL_NAME: &str = "testapi-arith-collection";
pub const TESTAPI_FMT_PROTOCOL_NAME: &str = "testapi-fmt";
pub const TESTAPI_ARITH_PROTOCOL_NAME: &str = "testapi-arith";
pub const TESTAPI_BARE_ARITH_PROTOCOL_NAME: &str = "testapi-bare-arith";
pub const TESTAPI_BARE_COMPUTE_PROTOCOL_NAME: &str = "testapi-bare-compute";
pub const TESTAPI_IO_PROTOCOL_NAME: &str = "testapi-io";
pub const TESTAPI_FLOAT_PROTOCOL_NAME: &str = "testapi-float";
pub const TESTAPI_DYN_LOAD_PROTOCOL_NAME: &str = "testapi-dyn-load";
pub const COEXIST_PROTOCOL_NAME: &str = "coexist";

pub const TESTAPI_ARITH_ADD_I32_PROTOCOL_NAME: &str = "testapi-arith-add-i32";
pub const TESTAPI_ARITH_ADD_INT_PROTOCOL_NAME: &str = "testapi-arith-add-int";

pub const TESTAPI_ARITH_COLLECTION_ELAB_PROTOCOL_NAME: &str = "testapi-arith-collection-elab";
pub const TESTAPI_ARITH_COLLECTION_ELAB_NO_BOOL_PROTOCOL_NAME: &str =
    "testapi-arith-collection-elab-no-bool-format";
pub const TESTAPI_ARITH_COLLECTION_COMPOSITE_ELAB_PROTOCOL_NAME: &str =
    "testapi-arith-collection-composite-elab";
pub const TESTAPI_ARITH_COLLECTION_DICT_ELAB_PROTOCOL_NAME: &str =
    "testapi-arith-collection-dict-elab";
pub const TESTAPI_ARITH_COLLECTION_LIST_ELAB_PROTOCOL_NAME: &str =
    "testapi-arith-collection-list-elab";
pub const TESTAPI_ARITH_COLLECTION_OPTICS_ELAB_PROTOCOL_NAME: &str =
    "testapi-arith-collection-optics-elab";
pub const TESTAPI_ARITH_COLLECTION_QUEUE_ELAB_PROTOCOL_NAME: &str =
    "testapi-arith-collection-queue-elab";

pub const TESTAPI_ARRAY_ROOT_PROTOCOL_NAME: &str = "testapi-array-root";
pub const TESTAPI_ARRAY_ELAB_NO_CLONE_SWAP_PROTOCOL_NAME: &str = "testapi-array-elab-no-clone-swap";
pub const TESTAPI_ARRAY_ELAB_PUSH_ONLY_PROTOCOL_NAME: &str = "testapi-array-elab-push-only";
pub const TESTAPI_ARRAY_ELAB_FIXED_PROTOCOL_NAME: &str = "testapi-array-elab-fixed";
pub const TESTAPI_ARRAY_ELAB_STACK_PROTOCOL_NAME: &str = "testapi-array-elab-stack";
pub const TESTAPI_ARRAY_GET_FILLED_PROTOCOL_NAME: &str = "testapi-array-get-filled";

pub const TESTAPI_BARE_ARITH_ELAB_I32_PROTOCOL_NAME: &str = "testapi-bare-arith-elab-i32";
pub const TESTAPI_BARE_ARITH_BOOL_I32_PROTOCOL_NAME: &str = "testapi-bare-arith-bool-i32";
pub const TESTAPI_BARE_COLLECTION_ELAB_REDUCED_PROTOCOL_NAME: &str =
    "testapi-bare-collection-elab-reduced";
pub const TESTAPI_BARE_COMPUTE_ELAB_I32_REDUCED_PROTOCOL_NAME: &str =
    "testapi-bare-compute-elab-i32-reduced";
pub const TESTAPI_BIGINT_U128_PROTOCOL_NAME: &str = "testapi-bigint-u128";

pub const TESTAPI_COMPUTE_ROOT_PROTOCOL_NAME: &str = "testapi-compute-root";
pub const TESTAPI_COMPUTE_LOOP_PROTOCOL_NAME: &str = "testapi-compute-loop";
pub const TESTAPI_COMPUTE_ELAB_NO_I32_FORMAT_PROTOCOL_NAME: &str =
    "testapi-compute-elab-no-i32-format";
pub const TESTAPI_COMPUTE_ELAB_NO_INPUT_PARSE_PROTOCOL_NAME: &str =
    "testapi-compute-elab-no-input-parse";
pub const TESTAPI_COMPUTE_LIST_ELAB_PROTOCOL_NAME: &str = "testapi-compute-list-elab";
pub const TESTAPI_COMPUTE_DIFF_PROTOCOL_NAME: &str = "testapi-compute-diff";
pub const TESTAPI_COMPUTE_ELAB_INT_PROTOCOL_NAME: &str = "testapi-compute-elab-int";
pub const TESTAPI_COMPUTE_REC_BINDER_PROTOCOL_NAME: &str = "testapi-compute-rec-binder";
pub const TESTAPI_COMPUTE_REC_ORDER_PROTOCOL_NAME: &str = "testapi-compute-rec-order";
pub const TESTAPI_COMPUTE_REC_PARTIAL_LET_PROTOCOL_NAME: &str = "testapi-compute-rec-partial-let";
pub const TESTAPI_COMPUTE_INT_STR_PROTOCOL_NAME: &str = "testapi-compute-int-str";
pub const TESTAPI_COMPUTE_SUB_PRINT_PROTOCOL_NAME: &str = "testapi-compute-sub-print";

pub const TESTAPI_FLOAT_F32_F64_PROTOCOL_NAME: &str = "testapi-float-f32-f64";

pub const TESTAPI_FMT_ROOT_I32_PROTOCOL_NAME: &str = "testapi-fmt-root-i32";
pub const TESTAPI_FMT_ELAB_INT_PROTOCOL_NAME: &str = "testapi-fmt-elab-int";
pub const TESTAPI_FMT_ROOT_INT_PROTOCOL_NAME: &str = "testapi-fmt-root-int";
pub const TESTAPI_FMT_INT_ONLY_PROTOCOL_NAME: &str = "testapi-fmt-int-only";

pub const TESTAPI_PRINT_ELAB_STRING_PROTOCOL_NAME: &str = "testapi-print-elab-string";
pub const TESTAPI_PRINT_ELAB_CORE_PROTOCOL_NAME: &str = "testapi-print-elab-core";
pub const TESTAPI_PRINT_ELAB_I32_PROTOCOL_NAME: &str = "testapi-print-elab-i32";
pub const TESTAPI_PRINT_BOOL_STRING_PROTOCOL_NAME: &str = "testapi-print-bool-string";
pub const TESTAPI_PRINT_ELAB_I32_INT_PROTOCOL_NAME: &str = "testapi-print-elab-i32-int";
pub const TESTAPI_PRINT_ELAB_BOOL_STRING_PROTOCOL_NAME: &str = "testapi-print-elab-bool-string";
pub const TESTAPI_PRINT_LOGIC_BOOL_PROTOCOL_NAME: &str = "testapi-print-logic-bool";
pub const TESTAPI_PRINT_STR_PROTOCOL_NAME: &str = "testapi-print-str";

pub const TESTAPI_TEXT_ELAB_I32_PROTOCOL_NAME: &str = "testapi-text-elab-i32";
pub const TESTAPI_TEXT_ELAB_INT_PROTOCOL_NAME: &str = "testapi-text-elab-int";
pub const TESTAPI_TEXT_ROOT_INT_PROTOCOL_NAME: &str = "testapi-text-root-int";
pub const TESTAPI_TEXT_BOOL_INT_PROTOCOL_NAME: &str = "testapi-text-bool-int";
pub const TESTAPI_TEXT_CONCAT_PROTOCOL_NAME: &str = "testapi-text-concat";

/// Protocols whose complete contract cannot yet be executed by the
/// one-image dyn-load-prime differential.
///
/// This is the canonical protocol-level classification consumed by both the
/// runner and its coverage lint. Keep it in protocol space: source scanning
/// must not infer runtime support from which declarations a particular case
/// happens to call.
#[allow(dead_code)] // Consumed only by the dyn-load-prime runner and shell lint.
pub const DYN_LOAD_PRIME_UNSUPPORTED_PROTOCOL_NAMES: &[&str] = &[
    EXPORT_POLY_CALLBACK_ROUNDTRIP_PROTOCOL_NAME,
    EXPORT_STRUCTURAL_ROUNDTRIP_PROTOCOL_NAME,
    EXPORT_SCALAR_ROUNDTRIP_PROTOCOL_NAME,
    EXPORT_HOST_OWNED_ROUNDTRIP_PROTOCOL_NAME,
    EXPORT_CALLABLE_SLOTS_ROUNDTRIP_PROTOCOL_NAME,
    RUST_CALLBACK_ALIASES_PROTOCOL_NAME,
    EXPORT_FUNCTOR_DICT_ROUNDTRIP_PROTOCOL_NAME,
    HOST_EXISTENTIAL_ROUNDTRIP_PROTOCOL_NAME,
    EXPORT_POSITIONAL_PRODUCT_ROUNDTRIP_PROTOCOL_NAME,
    EXPORT_WIDE_CALLABLE_PROTOCOL_NAME,
    NEWTYPE_VISIBILITY_FACADE_PROTOCOL_NAME,
    EXPORT_NEWTYPE_IGNORED_ARGUMENT_ROUNDTRIP_PROTOCOL_NAME,
    RECURSIVE_NEWTYPE_BOUNDARY_PROTOCOL_NAME,
    HOST_GENERIC_RETURN_ONLY_ROUNDTRIP_PROTOCOL_NAME,
    NESTED_CURRIED_ROUNDTRIP_PROTOCOL_NAME,
    HOST_SUBSTITUTED_UNIT_CALLBACK_PROTOCOL_NAME,
    RETURNED_FORALL_CALL_BY_VALUE_PROTOCOL_NAME,
    HOST_STAGED_UNIT_CALL_PROTOCOL_NAME,
    FACADE_SELECTOR_COLLISIONS_PROTOCOL_NAME,
    PUBLIC_WORD_NAMES_PROTOCOL_NAME,
    MODULE_ALIAS_SCOPE_COLLISION_PROTOCOL_NAME,
    TESTAPI_ARRAY_PROTOCOL_NAME,
    TESTAPI_ARRAY_CLEAR_PROTOCOL_NAME,
    TESTAPI_ARRAY_ROOT_PROTOCOL_NAME,
    TESTAPI_ARRAY_ELAB_NO_CLONE_SWAP_PROTOCOL_NAME,
    TESTAPI_ARRAY_ELAB_PUSH_ONLY_PROTOCOL_NAME,
    TESTAPI_ARRAY_ELAB_FIXED_PROTOCOL_NAME,
    TESTAPI_ARRAY_ELAB_STACK_PROTOCOL_NAME,
    TESTAPI_ARRAY_GET_FILLED_PROTOCOL_NAME,
    TESTAPI_BIGINT_PROTOCOL_NAME,
    TESTAPI_BIGINT_U128_PROTOCOL_NAME,
    TESTAPI_FLOAT_F32_F64_PROTOCOL_NAME,
    COEXIST_PROTOCOL_NAME,
];

/// The fixed root namespace every testapi-conformed golden re-roots its
/// test-facing surface under. Baked into the runner's canonical
/// knowledge — never read from a golden's `run.args`, `.kio` source, or
/// `kio build` output.
pub const TESTAPI_ROOT: &str = "testapi";
pub const TESTAPI_MAIN_MODULE: &str = "testapi/main";

/// The exact public value-slot count exercised by `export-wide-callable`.
#[allow(dead_code)] // This shared module is also compiled by the inapplicable dyn-load runner.
pub const WIDE_CALLABLE_SLOT_COUNT: usize = 255;

/// The default protocol name when `run.args` carries no `--protocol`.
/// `empty-main` — exact empty host plus the flat `main` driver — is the
/// untagged default; it pairs with [`RunnerProtocol::default`].
#[cfg(test)]
pub const DEFAULT_PROTOCOL_NAME: &str = EMPTY_PROTOCOL_NAME;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RunnerProtocol {
    #[default]
    Empty,
    CompileOnly,
    ConstructOnly,
    SameLeafHostLiteralRoles,
    MainBoxFixture,
    EmptyApiMain,
    GeneratedCoreMain,
    GeneratedSurfaceMain,
    ExportNamespaceRoundtrip,
    ExportCallbackRoundtrip,
    ExportModuleRoundtrip,
    ExportMultilabelRoundtrip,
    ExportPolyRoundtrip,
    /// `00_success/ffi_export_poly_callback` — a testapi-conformed export
    /// roundtrip whose subject is a **host closure crossing into a
    /// polymorphic export typed by the export's own type parameters**:
    /// `apply_via[K][R](f: K -> R, x: K) -> R` in `testapi/main`, threading
    /// the closure through an internal generic call (the same export shape
    /// `00_success/exec_host_closure_type_param_leg` declares, exercised
    /// here from the host side). The driver calls it at two concrete
    /// instantiations — a string transform (`f("apply")` → `via: apply`)
    /// and an integer step (`f(7)` → `15`) — and prints both results, so a
    /// boundary wrapper that fails to convert the erased argument to `K`
    /// before invoking the host closure (or to re-erase the `R` result)
    /// fails at runtime on every backend. Empty env (no host items at
    /// all — the signature reaches only type parameters). Cross-backend:
    /// js / ts / python / java / rust / go / swift / haskell.
    ExportPolyCallbackRoundtrip,
    ExportStructuralRoundtrip,
    ExportScalarRoundtrip,
    ExportHostOwnedRoundtrip,
    ExportCallableSlotsRoundtrip,
    /// Rust's public callback aliases, including binders of nested forall
    /// carriers. Other backend naming contracts do not define these aliases.
    RustCallbackAliases,
    ExportFunctorDictRoundtrip,
    HostExistentialRoundtrip,
    /// `00_success/build_label_and_structural_products` — an export
    /// roundtrip over one label-derived product and one anonymous structural
    /// product. The package declares the label-derived product first, then
    /// exports `make_pair(I32, String) -> I32 & String`. The driver calls
    /// `make_pair(7, "hello")` and projects both structural components
    /// through the backend's documented public product interface. A facade
    /// that accidentally reuses the labels from the first product fails the
    /// native host build or the runtime projections. Empty env.
    /// Cross-backend: js / ts / python / java / rust / go / swift / haskell.
    ExportPositionalProductRoundtrip,
    ExportTypeRoundtrip,
    /// `00_success/ffi_export_curried_facade` — a testapi-conformed
    /// export roundtrip whose subject is the **flat facade over curried
    /// multi-value-group exports**: the package exports
    /// `pick(a: Str)(b: Str) -> Str` (returns `a`) and
    /// `last(a: I32)(b: I32, c: I32) -> I32` (returns `c`). The driver
    /// calls both flat — `pick("ku", "rz")`, `last(1, 2, 3)` — and
    /// prints the results, pinning that every backend's host surface
    /// flattens value groups into one call while routing each group to
    /// its internal layer (`specs/backends/README.md` § Function-type
    /// FFI canonicalization). The env has no host functions; the exact role
    /// host types still appear as associated types on statically typed host
    /// contracts. Cross-backend: js / ts / python / java / rust / go / swift /
    /// haskell.
    ExportCurriedFacade,
    /// `00_success/ffi_export_wide_callable` — a testapi-conformed export
    /// roundtrip whose `select` declaration carries 255 public I32 value
    /// slots across three source groups. `make_select` additionally returns
    /// one callable with a single 255-slot stage. The host invokes both and
    /// prints each returned first, middle, and last triple.
    ExportWideCallable,
    /// `00_success/ffi_export_newtype_sum_roundtrip` — a testapi-conformed
    /// export roundtrip whose subject is a **sum arm that is a newtype over
    /// a product**: `Tagged = Pr | .` where the label-minted `Pr` wraps
    /// `(I32 & String)`. The package exports `pack(I32, String) -> Tagged`
    /// (Out) and `first_or(I32, Tagged) -> I32` (In). The driver calls
    /// `first_or(0, pack(7, "hi"))` and prints the recovered first field.
    /// The In conversion of `Tagged` reads the `Pr` arm's product fields,
    /// so the boundary representation must retain the typed product payload
    /// rather than erase it to the universal value. Empty
    /// env; the driver only chains the two exports. Cross-backend: js / ts /
    /// python / java / rust / go.
    ExportNewtypeSumRoundtrip,
    /// `00_success/ffi_export_newtype_scalar_roundtrip` — a
    /// testapi-conformed export roundtrip whose subject is a **bare
    /// scalar-payload newtype** (`newtype Wrap : I32`) crossing a `pub fn`
    /// in **both** directions: the package exports `bump(Wrap) -> Wrap`,
    /// taking the newtype as a param (In) and returning it (Out). The
    /// driver builds a `Wrap` through its public ctor, calls `bump`, and
    /// reads the result back through its public projector. The erased
    /// body boxes the newtype's *payload*, so each wrapper must bridge the
    /// nominal struct ↔ erased payload via `convert_newtype_value`; a bare
    /// `as_any(struct)` / `from_any::<struct>` would panic at the FFI
    /// boundary. Empty env; the driver only round-trips the one export.
    /// Cross-backend: js / ts / python / java / rust / go / swift / haskell.
    ExportNewtypeScalarRoundtrip,
    /// `00_success/ffi_export_newtype_ignored_argument_roundtrip` — a
    /// recursive-looking instantiation `Wrap : Const(Wrap)` where
    /// `Const[A]` ignores `A` and carries exactly one I32. The driver chains
    /// `from_i32` into `to_i32`, proving the public boundary remains the
    /// finite scalar-backed newtype rather than a recursive carrier.
    ExportNewtypeIgnoredArgumentRoundtrip,
    /// `00_success/build_alias_hidden_recursive_carrier` and
    /// `00_success/build_generic_newtype_recursive_sibling_boundary` — two
    /// recursive public-newtype payloads whose recursive edge is hidden
    /// behind, respectively, a type alias and a generic sibling carrier. The
    /// driver obtains a finite payload through an export, invokes the public
    /// constructor directly, round-trips the nominal value through an export,
    /// invokes the public projector directly, and prints an exported I32 tag
    /// that distinguishes the base payload from the recursive arm. This pins
    /// both public-member and wrapper conversions in both directions without
    /// reconstructing either recursive host shape.
    RecursiveNewtypeBoundary,
    /// `00_success/ffi_newtype_visibility_facade` — a testapi-conformed
    /// host-facade protocol for public newtypes. The driver round-trips two
    /// opaque nominal types through public functions,
    /// constructs a constructor-only type, and projects a projector-only
    /// type, exercises a type with both public members, and uses two same-leaf
    /// newtypes in distinct module namespaces. It covers natural positive host
    /// use without inspecting or reconstructing the emitted interface.
    NewtypeVisibilityFacade,
    /// `00_success/ffi_export_nested_product_roundtrip` — a
    /// testapi-conformed export roundtrip whose subject is a **named
    /// newtype product nested inside another named product**: the
    /// label-minted `Inner` wraps `(I32 & I32)` and `Outer` wraps
    /// `(String & Inner)`. The package exports
    /// `make(String, I32, I32) -> Outer`; the driver calls
    /// `make("nest", 7, 9)` and prints the outer string slot and both
    /// inner integer slots. The Out conversion opens one binding scope
    /// per product nesting level, so the skin must mint a fresh binder
    /// per level — the focused regression for the JS skin's
    /// temporal-dead-zone `__slots` shadowing (the inner conversion's
    /// binder captured the enclosing binder it reads in its own
    /// initializer). Empty env; the driver only reads one export's
    /// fields. Cross-backend: js / ts / python / java / rust / go /
    /// swift / haskell.
    ExportNestedProductRoundtrip,
    /// Count direct host and callback pair producers, then round-trip nested
    /// products, every sum arm, and an atomic value through public exports.
    /// Getter-capable hosts additionally assert selected-field access counts.
    ExportCompoundInputOnce,
    HostCallbackReturnRoundtrip,
    HostCallbackRoundtrip,
    /// `00_success/ffi_nested_curried_roundtrip` — one
    /// `String -> String -> String` callback crosses a host-function
    /// parameter and return, then the same shape crosses exported-function
    /// parameters and returns. The driver calls both layers separately on
    /// both legs; no facade may flatten the nested function value.
    NestedCurriedRoundtrip,
    /// `00_success/ffi_host_substituted_unit_callback` — the public alias
    /// `Callback[A] = A -> Text` is instantiated at Unit. The callback keeps
    /// its one pre-substitution value slot, and the host invokes it with an
    /// explicit Unit value before returning the resulting Text.
    HostSubstitutedUnitCallback,
    /// `00_success/ffi_returned_forall_call_by_value` — a host call returning
    /// `[A] A` is captured before a later host effect, then instantiated at
    /// Unit twice. The fixed trace proves the original host call occurs once
    /// and each returned type application is call-by-value.
    ReturnedForallCallByValue,
    /// `00_success/ffi_host_staged_unit_call` — the first value group of
    /// `[A](A)[B](Unit) -> Unit` retains its one public slot when `A = Unit`;
    /// the declaration's literal Unit group remains a zero-slot group.
    HostStagedUnitCall,
    /// `00_success/ffi_facade_selector_collisions` — one module namespace
    /// contains a same-spelling exported function, nested module, and public
    /// newtype. The fixed drivers reach all three through each backend's
    /// public facade without inspecting the emitted interface.
    FacadeSelectorCollisions,
    /// `00_success/ffi_public_word_names` — public word casing preserves the
    /// leading/suffix affix quartet, nested module ownership, and an affixed
    /// generic nominal constructor/projector surface. Independent hosts also
    /// observe transparent wrapper keys and the qualified second field of a
    /// product containing two same-leaf nominals from distinct nested modules.
    PublicWordNames,
    /// `00_success/ffi_module_alias_scope_collision` — two modules export the
    /// same alias and function leaves with different underlying shapes. Each
    /// module's `make` result is consumed by its own `consume`, pinning alias
    /// ownership to the declaring module.
    ModuleAliasScopeCollision,
    /// `00_success/exec_host_fn_value_ref_return_only` — a narrow,
    /// testapi-conformed contract where `array_make_empty` is referenced as a
    /// value before use. Its host types are `Array[T]`, `I32`, and `String`;
    /// its host-function set is exactly `array_get`, `array_make_empty`,
    /// `array_push`, `int_to_string`, and `print`; it invokes
    /// `testapi/main.main`.
    HostGenericReturnOnlyRoundtrip,
    HostGenericTypeRoundtrip,
    HostRanknRoundtrip,
    HostStructuralRoundtrip,
    HostStructuralElabRoundtrip,
    HostTypeRoundtrip,
    /// `00_success/ffi_host_functor_dict_roundtrip` — a functor/monad
    /// **dictionary** crossing the host boundary. The host fn
    /// `round_functor(d: Functor(Box)) -> Functor(Box)` receives a fully
    /// built `Functor[*F]` dictionary value and hands one back, so the
    /// dictionary crosses in **both** directions (env-fn parameter and
    /// return). The dictionary is an ordinary value Kio passes as a `fn`
    /// parameter — a newtype over a polymorphic-function payload — never a
    /// host typeclass instance, per `specs/backends/README.md` § Higher-kinded
    /// types. Each runner's body is the identity (`arg0`): an erased-static
    /// backend receives the erased dictionary closure, a native-HKT backend the
    /// rank-N field, and threading it straight back is the meaningful
    /// crossing — the package then applies
    /// the returned dictionary's `fmap`, proving it survives the round-trip
    /// intact. Testapi-conformed: `round_functor` in `testapi/arith`, the
    /// `Box` / `Functor` newtypes in `testapi/types`, exported `main` in
    /// `testapi/main`.
    HostFunctorDictRoundtrip,
    /// `00_success/ffi_host_poly_function_newtype_roundtrip` — a minimal
    /// host identity round-trip for `Pick_first : [A] (A & A) -> A`.
    /// The package constructs the newtype from a natural two-binder lambda,
    /// sends it through `round_picker`, projects the returned function, and
    /// invokes it. The one nominal value therefore crosses both boundary
    /// directions before its two-slot public callable is used.
    HostPolyFunctionNewtypeRoundtrip,
    /// `00_success/ffi_host_poly_unit_payload_roundtrip` pairs host identity
    /// round-trips to distinguish the zero value slots
    /// of `[A] . -> .` from the one Unit-valued slot retained when
    /// `[A] A -> A` is instantiated at Unit.
    HostPolyUnitPayloadRoundtrip,
    /// `00_success/ffi_host_interleaved_stage_roundtrip` — an exact host
    /// declaration with alternating type and value groups:
    /// `staged[A](first: String)[B](second: String) -> String`. The host
    /// receives the canonical flat two-value call and returns `second`.
    HostInterleavedStageRoundtrip,
    /// `test-data/poc/elab` — a bespoke fixed-env `main`-calling
    /// protocol. The elab POC's env (`print`, `string_concat`,
    /// `i32_to_string`, `bool_to_string`) pairs the formatting helpers
    /// with `string_concat`. Like the roundtrip protocols it pins its own
    /// exact environment in its [`ProtocolContract`]; unlike them its
    /// driver just calls exported `main`.
    Elab,
    /// `00_success/exec_host_env_root_scoped` — a testapi multi-root
    /// protocol. Two `testapi/<root>` submodules declare the **same
    /// leaf** with **distinct** signatures: `testapi/alpha` declares
    /// `print(String)` and `testapi/beta` declares `print(I32)`. Their
    /// qualified identities remain distinct as
    /// `testapi_alpha__print` / `testapi_beta__print`. The exported
    /// `main` (in `testapi/main`) calls only `alpha.print`;
    /// `beta.print` is a declared-but-uncalled trait member, so the
    /// runner's body for it is unreachable. Export root is `testapi`.
    RootScopedHostEnv,
    /// `00_success/exec_print`, testapi-conformed. Its exact `{print}`
    /// environment declares `print` in
    /// `testapi/io`, the `String` host type at the `testapi` root, and
    /// exported `main` in `testapi/main`.
    TestApiPrint,
    /// `00_success/exec_underscore_type_names`, testapi-conformed. It has the
    /// same executable shape as `testapi-print`, but the exact string-role
    /// host type is the marked type name `_String`. Keeping this as a distinct
    /// protocol makes every runner reconstruct that public host identity
    /// independently of emitted source.
    TestApiPrintMarkedString,
    /// `test-data/poc/option`, testapi-conformed. Each host function is
    /// fixed in its exact declaring submodule (`print` in
    /// `testapi/io`, the formatters in `testapi/fmt`, the string ops in
    /// `testapi/text`, the bare arithmetic + i32 comparisons in
    /// `testapi/arith`, `loop` in `testapi/iter`), the role types at the
    /// `testapi` root, and exported `main` in `testapi/main`.
    TestApiBareCollection,
    /// `00_success/exec_insertion_sort`, testapi-conformed. Each host
    /// function is fixed in its
    /// exact declaring submodule (`print` in `testapi/io`, the
    /// formatters in `testapi/fmt`, the `_i32` arithmetic + comparisons
    /// in `testapi/arith`, the string ops in `testapi/text`, `loop` in
    /// `testapi/iter` and the `array_*` family in `testapi/array`), the
    /// role types and the non-role `Array[T]` host type at the `testapi`
    /// root, and exported `main` in `testapi/main`.
    TestApiArray,
    /// `00_success/exec_array_clear`, testapi-conformed. Its exact host
    /// types are `Array[T]`, `I32`, and `String`; its host-function set is
    /// exactly `array_clear`, `array_len`, `array_make_filled`,
    /// `int_to_string`, and `print`. The package fills an array with three
    /// strings, clears it, and prints the resulting length.
    TestApiArrayClear,
    /// `00_success/exec_bigint_arithmetic`, testapi-conformed. A
    /// wide-integer contract exercising the JS `BigInt` value shape (per
    /// `specs/backends/js.md` § Atomic types): role host types
    /// `I64` / `U64` / `I128` / `U128` (and `String`) at the `testapi`
    /// root, the per-width formatters (`i64_to_string` / `u64_to_string`
    /// / `i128_to_string` / `u128_to_string`) in `testapi/fmt`, the
    /// per-width arithmetic (`add_i64` / `mul_u64` / `add_i128` /
    /// `add_u128`) in `testapi/arith`, `print`
    /// in `testapi/io`, and exported `main` in `testapi/main`.
    TestApiBigint,
    /// `test-data/poc/hkt`, testapi-conformed. It fixes `print` in `testapi/io`, the
    /// formatters in `testapi/fmt`, the string ops in `testapi/text`,
    /// the role types at the `testapi` root, and exported `main` in
    /// `testapi/main`.
    TestApiText,
    /// `test-data/poc/{queue,result}`, testapi-conformed. It fixes `print` in
    /// `testapi/io`, the formatters in `testapi/fmt`, the string ops in
    /// `testapi/text`, the `_i32` arithmetic + comparisons in
    /// `testapi/arith`, `loop` in `testapi/iter`, the role types at the
    /// `testapi` root, and exported `main` in `testapi/main`.
    TestApiCompute,
    /// `test-data/poc/list`, testapi-conformed. It fixes `print` in
    /// `testapi/io`, the formatters in `testapi/fmt`, `string_concat` in
    /// `testapi/text`, the `_i32` arithmetic + comparisons in
    /// `testapi/arith`, `loop` in `testapi/iter`, the role types at the
    /// `testapi` root, and exported `main` in `testapi/main`.
    TestApiArithCollection,
    /// The exact formatting environment (`print` + integer/bool formatting),
    /// rooted
    /// under `testapi`: `print` in `testapi/io`, the formatters in
    /// `testapi/fmt`, the role types at the `testapi` root, exported
    /// `main` in `testapi/main`.
    TestApiFmt,
    /// The exact arithmetic environment (`print` + integer formatting + the
    /// `_i32`-suffixed arithmetic), re-rooted under `testapi`: `print` in
    /// `testapi/io`, the formatters in `testapi/fmt`, the `_i32`
    /// arithmetic in `testapi/arith`, the role types at the `testapi`
    /// root, exported `main` in `testapi/main`.
    TestApiArith,
    /// The exact bare-arithmetic environment (`print` + integer formatting + the
    /// unsuffixed `add` / `sub` / `mul` / `div` / `mod` arithmetic),
    /// re-rooted under `testapi`: `print` in `testapi/io`, the formatters
    /// in `testapi/fmt`, the bare arithmetic in `testapi/arith`, the role
    /// types at the `testapi` root, exported `main` in `testapi/main`.
    TestApiBareArith,
    /// The exact bare-compute environment (bare arithmetic + string ops + stdin +
    /// `loop`), re-rooted under `testapi`: `print` in `testapi/io`, the
    /// formatters in `testapi/fmt`, the string ops in `testapi/text`, the
    /// bare arithmetic in `testapi/arith`, `loop` in `testapi/iter`, the
    /// role types at the `testapi` root, exported `main` in
    /// `testapi/main`.
    TestApiBareCompute,
    /// The exact I/O environment (`print` + stdin/stdout/exit), rooted under
    /// `testapi`: the I/O fns in `testapi/io`, the role types at the
    /// `testapi` root, exported `main` in `testapi/main`.
    TestApiIo,
    /// `00_success/exec_prime_eval_float` and
    /// `exec_float_arithmetic_testapi`, testapi-conformed. A float contract
    /// exercising the `f64` and `f32` role value shapes: `print` in
    /// `testapi/io`, `f64_to_string` / `f32_to_string` in `testapi/fmt`,
    /// the IEEE-754 arithmetic (`add_f64` / `sub_f64` / `mul_f64` /
    /// `add_f32`) in `testapi/arith`, the role types (`String` / `F64` /
    /// `F32`) at the `testapi` root, and exported `main` in `testapi/main`.
    /// The float arithmetic and stringify are rendered from the
    /// `FloatArithmetic` / `NumericToString` body kinds.
    TestApiFloat,
    /// `00_success/exec_dyn_load_integration`, testapi-conformed. The host
    /// env the `dyn_load_prime` package requires when a host vendors it and
    /// loads a guest image at runtime: the `compute` env plus `eq_i32` and
    /// the string-inspection ops (`string_len` / `string_slice` /
    /// `string_code_at`) the lexer / parser fold. The environment is exactly
    /// the dyn_load_prime package's
    /// `testapi` surface: `print` + `read_ascii_line` in `testapi/io`, the
    /// formatters in `testapi/fmt`, the string ops in `testapi/text`, the
    /// arithmetic + comparisons in `testapi/arith`, `loop` in
    /// `testapi/iter`, the role types at the `testapi` root, exported
    /// `main` in `testapi/main`.
    TestApiDynLoad,
    TestApiArithAddI32,
    TestApiArithAddInt,
    TestApiArithCollectionElab,
    TestApiArithCollectionElabNoBool,
    TestApiArithCollectionCompositeElab,
    TestApiArithCollectionDictElab,
    TestApiArithCollectionListElab,
    TestApiArithCollectionOpticsElab,
    TestApiArithCollectionQueueElab,
    TestApiArrayRoot,
    TestApiArrayElabNoCloneSwap,
    TestApiArrayElabPushOnly,
    TestApiArrayElabFixed,
    TestApiArrayElabStack,
    TestApiArrayGetFilled,
    TestApiBareArithElabI32,
    TestApiBareArithBoolI32,
    TestApiBareCollectionElabReduced,
    TestApiBareComputeElabI32Reduced,
    TestApiBigintU128,
    TestApiComputeRoot,
    TestApiComputeLoop,
    TestApiComputeElabNoI32Format,
    TestApiComputeElabNoInputParse,
    TestApiComputeListElab,
    TestApiComputeDiff,
    TestApiComputeElabInt,
    TestApiComputeRecBinder,
    TestApiComputeRecOrder,
    TestApiComputeRecPartialLet,
    TestApiComputeIntStr,
    TestApiComputeSubPrint,
    TestApiFloatF32F64,
    TestApiFmtRootI32,
    TestApiFmtElabInt,
    TestApiFmtRootInt,
    TestApiFmtIntOnly,
    TestApiPrintElabString,
    TestApiPrintElabCore,
    TestApiPrintElabI32,
    TestApiPrintBoolString,
    TestApiPrintElabI32Int,
    TestApiPrintElabBoolString,
    TestApiPrintLogicBool,
    TestApiPrintStr,
    TestApiTextElabI32,
    TestApiTextElabInt,
    TestApiTextRootInt,
    TestApiTextBoolInt,
    TestApiTextConcat,
    /// `00_success/ffi_two_packages_coexist` — the two-artifact protocol
    /// (see the module doc's § The coexist protocol). Env `{print}` in
    /// module `greeter`, exports `greeter/main.main` and the same-shaped
    /// `greeter/main.pair() -> (I32 & String)`, positional host prefixes
    /// `first: ` / `second: `, call order first → second → first for the
    /// greetings and again for the `pair` product reads.
    Coexist,
}

const NO_HOST_TYPES: &[HostTypeBinding] = &[];
const NO_HOST_FNS: &[HostFnBinding] = &[];
const TESTAPI_ARRAY_TYPE: HostTypeIdentity = HostTypeIdentity::new("testapi", "Array");
const TESTAPI_BOX_TYPE: HostTypeIdentity = HostTypeIdentity::new("testapi", "Box");
const TESTAPI_TOKEN_TYPE: HostTypeIdentity = HostTypeIdentity::new("testapi", "Token");
const TESTAPI_SCALAR_TYPE: HostTypeIdentity = HostTypeIdentity::new("testapi", "Scalar");
const TESTAPI_BOOL_ROLE: HostRoleRef = HostRoleRef::new("testapi", "Bool", RoleFixture::Bool);
const TESTAPI_COUNT_ROLE: HostRoleRef = HostRoleRef::new("testapi", "Count", RoleFixture::I32);
const TESTAPI_I32_ROLE: HostRoleRef = HostRoleRef::new("testapi", "I32", RoleFixture::I32);
const TESTAPI_INT_ROLE: HostRoleRef = HostRoleRef::new("testapi", "Int", RoleFixture::I32);
const TESTAPI_I64_ROLE: HostRoleRef = HostRoleRef::new("testapi", "I64", RoleFixture::I64);
const TESTAPI_I128_ROLE: HostRoleRef = HostRoleRef::new("testapi", "I128", RoleFixture::I128);
const TESTAPI_U64_ROLE: HostRoleRef = HostRoleRef::new("testapi", "U64", RoleFixture::U64);
const TESTAPI_U128_ROLE: HostRoleRef = HostRoleRef::new("testapi", "U128", RoleFixture::U128);
const TESTAPI_F32_ROLE: HostRoleRef = HostRoleRef::new("testapi", "F32", RoleFixture::F32);
const TESTAPI_F64_ROLE: HostRoleRef = HostRoleRef::new("testapi", "F64", RoleFixture::F64);
const TESTAPI_STRING_ROLE: HostRoleRef = HostRoleRef::new("testapi", "String", RoleFixture::String);
const TESTAPI_MARKED_STRING_ROLE: HostRoleRef =
    HostRoleRef::new("testapi", "_String", RoleFixture::String);
const TESTAPI_STR_ROLE: HostRoleRef = HostRoleRef::new("testapi", "Str", RoleFixture::String);
const TESTAPI_TEXT_ROLE: HostRoleRef = HostRoleRef::new("testapi", "Text", RoleFixture::String);
const SELECTOR_FOO_BAR_I32_ROLE: HostRoleRef =
    HostRoleRef::new("testapi/foo/bar", "I32", RoleFixture::I32);
const SELECTOR_FOO_UBAR_I32_ROLE: HostRoleRef =
    HostRoleRef::new("testapi/foo_bar", "I32", RoleFixture::I32);
const SELECTOR_I_I32_ROLE: HostRoleRef = HostRoleRef::new("testapi/i", "I32", RoleFixture::I32);
const GREETER_STRING_ROLE: HostRoleRef = HostRoleRef::new("greeter", "String", RoleFixture::String);

const TYPES_SAME_LEAF_ROLES: &[HostTypeBinding] = &[
    HostTypeBinding::role("left", "Shared", RoleFixture::I64),
    HostTypeBinding::role("right", "Shared", RoleFixture::I32),
];
const TYPES_MAIN_BOX: &[HostTypeBinding] = &[HostTypeBinding::opaque(
    "main",
    "Box",
    1,
    HostTypeFixture::Box,
)];
const TYPES_GENERATED_CORE: &[HostTypeBinding] = &[
    HostTypeBinding::role("prog", "I8", RoleFixture::I8),
    HostTypeBinding::role("prog", "I16", RoleFixture::I16),
    HostTypeBinding::role("prog", "I32", RoleFixture::I32),
    HostTypeBinding::role("prog", "I64", RoleFixture::I64),
    HostTypeBinding::role("prog", "I128", RoleFixture::I128),
    HostTypeBinding::role("prog", "U8", RoleFixture::U8),
    HostTypeBinding::role("prog", "U16", RoleFixture::U16),
    HostTypeBinding::role("prog", "U32", RoleFixture::U32),
    HostTypeBinding::role("prog", "U64", RoleFixture::U64),
    HostTypeBinding::role("prog", "U128", RoleFixture::U128),
    HostTypeBinding::role("prog", "F32", RoleFixture::F32),
    HostTypeBinding::role("prog", "F64", RoleFixture::F64),
    HostTypeBinding::role("prog", "String", RoleFixture::String),
    HostTypeBinding::role("prog", "Bool", RoleFixture::Bool),
];
const TYPES_GENERATED_SURFACE: &[HostTypeBinding] = &[
    HostTypeBinding::role("prog", "I8", RoleFixture::I8),
    HostTypeBinding::role("prog", "I16", RoleFixture::I16),
    HostTypeBinding::role("prog", "I32", RoleFixture::I32),
    HostTypeBinding::role("prog", "I64", RoleFixture::I64),
    HostTypeBinding::role("prog", "I128", RoleFixture::I128),
    HostTypeBinding::role("prog", "U8", RoleFixture::U8),
    HostTypeBinding::role("prog", "U16", RoleFixture::U16),
    HostTypeBinding::role("prog", "U32", RoleFixture::U32),
    HostTypeBinding::role("prog", "U64", RoleFixture::U64),
    HostTypeBinding::role("prog", "U128", RoleFixture::U128),
    HostTypeBinding::role("prog", "F32", RoleFixture::F32),
    HostTypeBinding::role("prog", "F64", RoleFixture::F64),
    HostTypeBinding::role("prog", "String", RoleFixture::String),
    HostTypeBinding::role("prog", "Bool", RoleFixture::Bool),
    HostTypeBinding::role("testapi", "String", RoleFixture::String),
    HostTypeBinding::role("testapi", "I32", RoleFixture::I32),
    HostTypeBinding::role("testapi", "Int", RoleFixture::I32),
    HostTypeBinding::role("testapi", "Bool", RoleFixture::Bool),
];

const TYPES_TESTAPI_STRING: &[HostTypeBinding] = &[HostTypeBinding::role(
    "testapi",
    "String",
    RoleFixture::String,
)];
const TYPES_TESTAPI_MARKED_STRING: &[HostTypeBinding] = &[HostTypeBinding::role(
    "testapi",
    "_String",
    RoleFixture::String,
)];
const TYPES_TESTAPI_STR: &[HostTypeBinding] =
    &[HostTypeBinding::role("testapi", "Str", RoleFixture::String)];
const TYPES_TESTAPI_TEXT: &[HostTypeBinding] = &[HostTypeBinding::role(
    "testapi",
    "Text",
    RoleFixture::String,
)];
const TYPES_TESTAPI_I32: &[HostTypeBinding] =
    &[HostTypeBinding::role("testapi", "I32", RoleFixture::I32)];
const TYPES_MAIN_I32: &[HostTypeBinding] =
    &[HostTypeBinding::role("main", "I32", RoleFixture::I32)];
const TYPES_PUBLIC_WORD_NAMES: &[HostTypeBinding] = &[HostTypeBinding::role(
    "word_api",
    "Count_value",
    RoleFixture::I32,
)];
const TYPES_FACADE_SELECTOR_COLLISIONS: &[HostTypeBinding] = &[
    HostTypeBinding::role("testapi", "I32", RoleFixture::I32),
    HostTypeBinding::role("testapi/foo/bar", "I32", RoleFixture::I32),
    HostTypeBinding::role("testapi/foo_bar", "I32", RoleFixture::I32),
    HostTypeBinding::role("testapi/i", "I32", RoleFixture::I32),
];
const TYPES_TESTAPI_SELECTED_I32: &[HostTypeBinding] = &[HostTypeBinding::selected_role(
    "testapi",
    "I32",
    RoleFixture::I32,
)];
const TYPES_TESTAPI_I32_STRING: &[HostTypeBinding] = &[
    HostTypeBinding::role("testapi", "I32", RoleFixture::I32),
    HostTypeBinding::role("testapi", "String", RoleFixture::String),
];
const TYPES_MAIN_I32_STRING: &[HostTypeBinding] = &[
    HostTypeBinding::role("main", "I32", RoleFixture::I32),
    HostTypeBinding::role("main", "String", RoleFixture::String),
];
const TYPES_TESTAPI_I32_STR: &[HostTypeBinding] = &[
    HostTypeBinding::role("testapi", "I32", RoleFixture::I32),
    HostTypeBinding::role("testapi", "Str", RoleFixture::String),
];
const TYPES_TESTAPI_INT_STRING: &[HostTypeBinding] = &[
    HostTypeBinding::role("testapi", "Int", RoleFixture::I32),
    HostTypeBinding::role("testapi", "String", RoleFixture::String),
];
const TYPES_TESTAPI_BOOL_STRING: &[HostTypeBinding] = &[
    HostTypeBinding::role("testapi", "Bool", RoleFixture::Bool),
    HostTypeBinding::role("testapi", "String", RoleFixture::String),
];
const TYPES_TESTAPI_CORE: &[HostTypeBinding] = &[
    HostTypeBinding::role("testapi", "Bool", RoleFixture::Bool),
    HostTypeBinding::role("testapi", "I32", RoleFixture::I32),
    HostTypeBinding::role("testapi", "String", RoleFixture::String),
];
const TYPES_TESTAPI_BOOL_INT_STRING: &[HostTypeBinding] = &[
    HostTypeBinding::role("testapi", "Bool", RoleFixture::Bool),
    HostTypeBinding::role("testapi", "Int", RoleFixture::I32),
    HostTypeBinding::role("testapi", "String", RoleFixture::String),
];
const TYPES_TESTAPI_BOOL_INT_STR: &[HostTypeBinding] = &[
    HostTypeBinding::role("testapi", "Bool", RoleFixture::Bool),
    HostTypeBinding::role("testapi", "Int", RoleFixture::I32),
    HostTypeBinding::role("testapi", "Str", RoleFixture::String),
];
const TYPES_TESTAPI_BOOL_I32_INT_STRING: &[HostTypeBinding] = &[
    HostTypeBinding::role("testapi", "Bool", RoleFixture::Bool),
    HostTypeBinding::role("testapi", "I32", RoleFixture::I32),
    HostTypeBinding::role("testapi", "Int", RoleFixture::I32),
    HostTypeBinding::role("testapi", "String", RoleFixture::String),
];

const TYPES_ELAB_STRING: &[HostTypeBinding] = &[
    HostTypeBinding::role("elab/testapi", "Bool", RoleFixture::Bool),
    HostTypeBinding::role("elab/testapi", "I32", RoleFixture::I32),
    HostTypeBinding::role("elab/testapi", "Int", RoleFixture::I32),
    HostTypeBinding::role("elab/testapi", "String", RoleFixture::String),
    HostTypeBinding::role("testapi", "String", RoleFixture::String),
];
const TYPES_ELAB_BOOL_STRING: &[HostTypeBinding] = &[
    HostTypeBinding::role("elab/testapi", "Bool", RoleFixture::Bool),
    HostTypeBinding::role("elab/testapi", "I32", RoleFixture::I32),
    HostTypeBinding::role("elab/testapi", "Int", RoleFixture::I32),
    HostTypeBinding::role("elab/testapi", "String", RoleFixture::String),
    HostTypeBinding::role("testapi", "Bool", RoleFixture::Bool),
    HostTypeBinding::role("testapi", "String", RoleFixture::String),
];
const TYPES_ELAB_I32_STRING: &[HostTypeBinding] = &[
    HostTypeBinding::role("elab/testapi", "Bool", RoleFixture::Bool),
    HostTypeBinding::role("elab/testapi", "I32", RoleFixture::I32),
    HostTypeBinding::role("elab/testapi", "Int", RoleFixture::I32),
    HostTypeBinding::role("elab/testapi", "String", RoleFixture::String),
    HostTypeBinding::role("testapi", "I32", RoleFixture::I32),
    HostTypeBinding::role("testapi", "String", RoleFixture::String),
];
const TYPES_ELAB_I32_INT_STRING: &[HostTypeBinding] = &[
    HostTypeBinding::role("elab/testapi", "Bool", RoleFixture::Bool),
    HostTypeBinding::role("elab/testapi", "I32", RoleFixture::I32),
    HostTypeBinding::role("elab/testapi", "Int", RoleFixture::I32),
    HostTypeBinding::role("elab/testapi", "String", RoleFixture::String),
    HostTypeBinding::role("testapi", "I32", RoleFixture::I32),
    HostTypeBinding::role("testapi", "Int", RoleFixture::I32),
    HostTypeBinding::role("testapi", "String", RoleFixture::String),
];
const TYPES_ELAB_CORE: &[HostTypeBinding] = &[
    HostTypeBinding::role("elab/testapi", "Bool", RoleFixture::Bool),
    HostTypeBinding::role("elab/testapi", "I32", RoleFixture::I32),
    HostTypeBinding::role("elab/testapi", "Int", RoleFixture::I32),
    HostTypeBinding::role("elab/testapi", "String", RoleFixture::String),
    HostTypeBinding::role("testapi", "Bool", RoleFixture::Bool),
    HostTypeBinding::role("testapi", "I32", RoleFixture::I32),
    HostTypeBinding::role("testapi", "String", RoleFixture::String),
];
const TYPES_ELAB_BOOL_INT_STRING: &[HostTypeBinding] = &[
    HostTypeBinding::role("elab/testapi", "Bool", RoleFixture::Bool),
    HostTypeBinding::role("elab/testapi", "I32", RoleFixture::I32),
    HostTypeBinding::role("elab/testapi", "Int", RoleFixture::I32),
    HostTypeBinding::role("elab/testapi", "String", RoleFixture::String),
    HostTypeBinding::role("testapi", "Bool", RoleFixture::Bool),
    HostTypeBinding::role("testapi", "Int", RoleFixture::I32),
    HostTypeBinding::role("testapi", "String", RoleFixture::String),
];
const TYPES_ELAB_BOOL_I32_INT_STRING: &[HostTypeBinding] = &[
    HostTypeBinding::role("elab/testapi", "Bool", RoleFixture::Bool),
    HostTypeBinding::role("elab/testapi", "I32", RoleFixture::I32),
    HostTypeBinding::role("elab/testapi", "Int", RoleFixture::I32),
    HostTypeBinding::role("elab/testapi", "String", RoleFixture::String),
    HostTypeBinding::role("testapi", "Bool", RoleFixture::Bool),
    HostTypeBinding::role("testapi", "I32", RoleFixture::I32),
    HostTypeBinding::role("testapi", "Int", RoleFixture::I32),
    HostTypeBinding::role("testapi", "String", RoleFixture::String),
];

const TYPES_ARRAY_ROOT: &[HostTypeBinding] = &[
    HostTypeBinding::opaque("testapi", "Array", 1, HostTypeFixture::Array),
    HostTypeBinding::role("testapi", "Bool", RoleFixture::Bool),
    HostTypeBinding::role("testapi", "I32", RoleFixture::I32),
    HostTypeBinding::role("testapi", "String", RoleFixture::String),
];
const TYPES_HOST_ARRAY_RETURN_ONLY: &[HostTypeBinding] = &[
    HostTypeBinding::opaque("testapi", "Array", 1, HostTypeFixture::Array),
    HostTypeBinding::role("testapi", "I32", RoleFixture::I32),
    HostTypeBinding::role("testapi", "String", RoleFixture::String),
];
const TYPES_ARRAY_ELAB: &[HostTypeBinding] = &[
    HostTypeBinding::role("elab/testapi", "Bool", RoleFixture::Bool),
    HostTypeBinding::role("elab/testapi", "I32", RoleFixture::I32),
    HostTypeBinding::role("elab/testapi", "Int", RoleFixture::I32),
    HostTypeBinding::role("elab/testapi", "String", RoleFixture::String),
    HostTypeBinding::opaque("testapi", "Array", 1, HostTypeFixture::Array),
    HostTypeBinding::role("testapi", "Bool", RoleFixture::Bool),
    HostTypeBinding::role("testapi", "I32", RoleFixture::I32),
    HostTypeBinding::role("testapi", "String", RoleFixture::String),
];

const TYPES_BIGINT: &[HostTypeBinding] = &[
    HostTypeBinding::role("testapi", "I128", RoleFixture::I128),
    HostTypeBinding::role("testapi", "I64", RoleFixture::I64),
    HostTypeBinding::role("testapi", "String", RoleFixture::String),
    HostTypeBinding::role("testapi", "U64", RoleFixture::U64),
];
const TYPES_BIGINT_U128: &[HostTypeBinding] = &[
    HostTypeBinding::role("testapi", "I128", RoleFixture::I128),
    HostTypeBinding::role("testapi", "I64", RoleFixture::I64),
    HostTypeBinding::role("testapi", "String", RoleFixture::String),
    HostTypeBinding::role("testapi", "U128", RoleFixture::U128),
    HostTypeBinding::role("testapi", "U64", RoleFixture::U64),
];
const TYPES_FLOAT_F64: &[HostTypeBinding] = &[
    HostTypeBinding::role("testapi", "F64", RoleFixture::F64),
    HostTypeBinding::role("testapi", "String", RoleFixture::String),
];
const TYPES_FLOAT_F32_F64: &[HostTypeBinding] = &[
    HostTypeBinding::role("testapi", "F32", RoleFixture::F32),
    HostTypeBinding::role("testapi", "F64", RoleFixture::F64),
    HostTypeBinding::role("testapi", "String", RoleFixture::String),
];
const TYPES_LOGIC_BOOL: &[HostTypeBinding] = &[
    HostTypeBinding::role("logic", "Later_bool", RoleFixture::Bool),
    HostTypeBinding::role("testapi", "Bool", RoleFixture::Bool),
    HostTypeBinding::role("testapi", "String", RoleFixture::String),
];

const TYPES_ARITH_COLLECTION_COMPOSITE: &[HostTypeBinding] = &[
    HostTypeBinding::role("dict/elab/testapi", "Bool", RoleFixture::Bool),
    HostTypeBinding::role("dict/elab/testapi", "I32", RoleFixture::I32),
    HostTypeBinding::role("dict/elab/testapi", "Int", RoleFixture::I32),
    HostTypeBinding::role("dict/elab/testapi", "String", RoleFixture::String),
    HostTypeBinding::role("elab/testapi", "Bool", RoleFixture::Bool),
    HostTypeBinding::role("elab/testapi", "I32", RoleFixture::I32),
    HostTypeBinding::role("elab/testapi", "Int", RoleFixture::I32),
    HostTypeBinding::role("elab/testapi", "String", RoleFixture::String),
    HostTypeBinding::role("list/elab/testapi", "Bool", RoleFixture::Bool),
    HostTypeBinding::role("list/elab/testapi", "I32", RoleFixture::I32),
    HostTypeBinding::role("list/elab/testapi", "Int", RoleFixture::I32),
    HostTypeBinding::role("list/elab/testapi", "String", RoleFixture::String),
    HostTypeBinding::role("result/elab/testapi", "Bool", RoleFixture::Bool),
    HostTypeBinding::role("result/elab/testapi", "I32", RoleFixture::I32),
    HostTypeBinding::role("result/elab/testapi", "Int", RoleFixture::I32),
    HostTypeBinding::role("result/elab/testapi", "String", RoleFixture::String),
    HostTypeBinding::role("testapi", "Bool", RoleFixture::Bool),
    HostTypeBinding::role("testapi", "I32", RoleFixture::I32),
    HostTypeBinding::role("testapi", "String", RoleFixture::String),
];

const fn dependency_elab_type(
    module: &'static str,
    leaf: &'static str,
    fixture: RoleFixture,
) -> HostTypeBinding {
    HostTypeBinding::role(module, leaf, fixture)
}

const TYPES_ARITH_COLLECTION_DICT: &[HostTypeBinding] = &[
    dependency_elab_type("dict/elab/testapi", "Bool", RoleFixture::Bool),
    dependency_elab_type("dict/elab/testapi", "I32", RoleFixture::I32),
    dependency_elab_type("dict/elab/testapi", "Int", RoleFixture::I32),
    dependency_elab_type("dict/elab/testapi", "String", RoleFixture::String),
    HostTypeBinding::role("elab/testapi", "Bool", RoleFixture::Bool),
    HostTypeBinding::role("elab/testapi", "I32", RoleFixture::I32),
    HostTypeBinding::role("elab/testapi", "Int", RoleFixture::I32),
    HostTypeBinding::role("elab/testapi", "String", RoleFixture::String),
    dependency_elab_type("list/elab/testapi", "Bool", RoleFixture::Bool),
    dependency_elab_type("list/elab/testapi", "I32", RoleFixture::I32),
    dependency_elab_type("list/elab/testapi", "Int", RoleFixture::I32),
    dependency_elab_type("list/elab/testapi", "String", RoleFixture::String),
    HostTypeBinding::role("testapi", "Bool", RoleFixture::Bool),
    HostTypeBinding::role("testapi", "I32", RoleFixture::I32),
    HostTypeBinding::role("testapi", "String", RoleFixture::String),
];
const TYPES_ARITH_COLLECTION_LIST: &[HostTypeBinding] = &[
    HostTypeBinding::role("elab/testapi", "Bool", RoleFixture::Bool),
    HostTypeBinding::role("elab/testapi", "I32", RoleFixture::I32),
    HostTypeBinding::role("elab/testapi", "Int", RoleFixture::I32),
    HostTypeBinding::role("elab/testapi", "String", RoleFixture::String),
    dependency_elab_type("list/elab/testapi", "Bool", RoleFixture::Bool),
    dependency_elab_type("list/elab/testapi", "I32", RoleFixture::I32),
    dependency_elab_type("list/elab/testapi", "Int", RoleFixture::I32),
    dependency_elab_type("list/elab/testapi", "String", RoleFixture::String),
    HostTypeBinding::role("testapi", "Bool", RoleFixture::Bool),
    HostTypeBinding::role("testapi", "I32", RoleFixture::I32),
    HostTypeBinding::role("testapi", "String", RoleFixture::String),
];
const TYPES_ARITH_COLLECTION_OPTICS: &[HostTypeBinding] = &[
    HostTypeBinding::role("elab/testapi", "Bool", RoleFixture::Bool),
    HostTypeBinding::role("elab/testapi", "I32", RoleFixture::I32),
    HostTypeBinding::role("elab/testapi", "Int", RoleFixture::I32),
    HostTypeBinding::role("elab/testapi", "String", RoleFixture::String),
    dependency_elab_type("optics/elab/testapi", "Bool", RoleFixture::Bool),
    dependency_elab_type("optics/elab/testapi", "I32", RoleFixture::I32),
    dependency_elab_type("optics/elab/testapi", "Int", RoleFixture::I32),
    dependency_elab_type("optics/elab/testapi", "String", RoleFixture::String),
    HostTypeBinding::role("testapi", "Bool", RoleFixture::Bool),
    HostTypeBinding::role("testapi", "I32", RoleFixture::I32),
    HostTypeBinding::role("testapi", "String", RoleFixture::String),
];
const TYPES_ARITH_COLLECTION_QUEUE: &[HostTypeBinding] = &[
    HostTypeBinding::role("elab/testapi", "Bool", RoleFixture::Bool),
    HostTypeBinding::role("elab/testapi", "I32", RoleFixture::I32),
    HostTypeBinding::role("elab/testapi", "Int", RoleFixture::I32),
    HostTypeBinding::role("elab/testapi", "String", RoleFixture::String),
    dependency_elab_type("queue/elab/testapi", "Bool", RoleFixture::Bool),
    dependency_elab_type("queue/elab/testapi", "I32", RoleFixture::I32),
    dependency_elab_type("queue/elab/testapi", "Int", RoleFixture::I32),
    dependency_elab_type("queue/elab/testapi", "String", RoleFixture::String),
    dependency_elab_type("queue/list/elab/testapi", "Bool", RoleFixture::Bool),
    dependency_elab_type("queue/list/elab/testapi", "I32", RoleFixture::I32),
    dependency_elab_type("queue/list/elab/testapi", "Int", RoleFixture::I32),
    dependency_elab_type("queue/list/elab/testapi", "String", RoleFixture::String),
    HostTypeBinding::role("testapi", "Bool", RoleFixture::Bool),
    HostTypeBinding::role("testapi", "I32", RoleFixture::I32),
    HostTypeBinding::role("testapi", "String", RoleFixture::String),
];

const TYPES_COMPUTE_LIST_ELAB: &[HostTypeBinding] = &[
    HostTypeBinding::role("elab/testapi", "Bool", RoleFixture::Bool),
    HostTypeBinding::role("elab/testapi", "I32", RoleFixture::I32),
    HostTypeBinding::role("elab/testapi", "Int", RoleFixture::I32),
    HostTypeBinding::role("elab/testapi", "String", RoleFixture::String),
    dependency_elab_type("list/elab/testapi", "Bool", RoleFixture::Bool),
    dependency_elab_type("list/elab/testapi", "I32", RoleFixture::I32),
    dependency_elab_type("list/elab/testapi", "Int", RoleFixture::I32),
    dependency_elab_type("list/elab/testapi", "String", RoleFixture::String),
    HostTypeBinding::role("testapi", "Bool", RoleFixture::Bool),
    HostTypeBinding::role("testapi", "I32", RoleFixture::I32),
    HostTypeBinding::role("testapi", "String", RoleFixture::String),
];

const TYPES_HOST_TOKEN: &[HostTypeBinding] = &[
    HostTypeBinding::selected_role("testapi", "Count", RoleFixture::I32),
    HostTypeBinding::selected_role("testapi", "I32", RoleFixture::I32),
    HostTypeBinding::selected_role("testapi", "String", RoleFixture::String),
    HostTypeBinding::opaque("testapi", "Token", 0, HostTypeFixture::Token),
];
const TYPES_HOST_BOX: &[HostTypeBinding] = &[
    HostTypeBinding::opaque("testapi", "Box", 1, HostTypeFixture::Box),
    HostTypeBinding::role("testapi", "I32", RoleFixture::I32),
    HostTypeBinding::role("testapi", "String", RoleFixture::String),
];
const TYPES_DYN_LOAD: &[HostTypeBinding] = &[
    HostTypeBinding::role("testapi", "Bool", RoleFixture::Bool),
    HostTypeBinding::role("testapi", "F64", RoleFixture::F64),
    HostTypeBinding::role("testapi", "I32", RoleFixture::I32),
    HostTypeBinding::opaque("testapi", "Scalar", 0, HostTypeFixture::Scalar),
    HostTypeBinding::role("testapi", "String", RoleFixture::String),
];
const TYPES_EXPORT_STRUCTURAL: &[HostTypeBinding] = &[
    HostTypeBinding::role("elab/testapi", "Bool", RoleFixture::Bool),
    HostTypeBinding::role("elab/testapi", "I32", RoleFixture::I32),
    HostTypeBinding::role("elab/testapi", "Int", RoleFixture::I32),
    HostTypeBinding::role("elab/testapi", "String", RoleFixture::String),
    HostTypeBinding::role("testapi", "Bool", RoleFixture::Bool),
    HostTypeBinding::role("testapi", "I16", RoleFixture::I16),
    HostTypeBinding::role("testapi", "I32", RoleFixture::I32),
    HostTypeBinding::role("testapi", "I64", RoleFixture::I64),
    HostTypeBinding::role("testapi", "I8", RoleFixture::I8),
    HostTypeBinding::role("testapi", "String", RoleFixture::String),
    HostTypeBinding::role("testapi", "U16", RoleFixture::U16),
    HostTypeBinding::role("testapi", "U32", RoleFixture::U32),
    HostTypeBinding::role("testapi", "U64", RoleFixture::U64),
    HostTypeBinding::role("testapi", "U8", RoleFixture::U8),
];
const TYPES_EXPORT_SCALAR: &[HostTypeBinding] = &[
    HostTypeBinding::role("testapi", "F32", RoleFixture::F32),
    HostTypeBinding::role("testapi", "F64", RoleFixture::F64),
    HostTypeBinding::role("testapi", "I128", RoleFixture::I128),
    HostTypeBinding::role("testapi", "U128", RoleFixture::U128),
];
const TYPES_EXPORT_HOST_OWNED: &[HostTypeBinding] = &[
    HostTypeBinding::opaque("testapi", "Box", 1, HostTypeFixture::Box),
    HostTypeBinding::role("testapi", "I32", RoleFixture::I32),
    HostTypeBinding::role("testapi", "String", RoleFixture::String),
    HostTypeBinding::opaque("testapi", "Token", 0, HostTypeFixture::Token),
];
const TYPES_EXPORT_CALLABLE_SLOTS: &[HostTypeBinding] = &[
    HostTypeBinding::role("elab/testapi", "Bool", RoleFixture::Bool),
    HostTypeBinding::role("elab/testapi", "I32", RoleFixture::I32),
    HostTypeBinding::role("elab/testapi", "Int", RoleFixture::I32),
    HostTypeBinding::role("elab/testapi", "String", RoleFixture::String),
    HostTypeBinding::role("testapi", "I32", RoleFixture::I32),
];
const TYPES_COEXIST: &[HostTypeBinding] = &[
    HostTypeBinding::role("greeter", "I32", RoleFixture::I32),
    HostTypeBinding::role("greeter", "String", RoleFixture::String),
];

const FNS_PRINT_STRING: &[HostFnBinding] = &[host_fn(
    "testapi/io",
    "print",
    HostFnBodyKind::print(TESTAPI_STRING_ROLE),
)];
const FNS_PRINT_MARKED_STRING: &[HostFnBinding] = &[host_fn(
    "testapi/io",
    "print",
    HostFnBodyKind::print(TESTAPI_MARKED_STRING_ROLE),
)];
const FNS_PRINT_STR: &[HostFnBinding] = &[host_fn(
    "testapi/io",
    "print",
    HostFnBodyKind::print(TESTAPI_STR_ROLE),
)];

const FNS_FMT_I32: &[HostFnBinding] = &[
    host_fn(
        "testapi/fmt",
        "bool_to_string",
        HostFnBodyKind::bool_to_string(TESTAPI_BOOL_ROLE, TESTAPI_STRING_ROLE),
    ),
    host_fn(
        "testapi/fmt",
        "i32_to_string",
        HostFnBodyKind::numeric_to_string(TESTAPI_I32_ROLE, TESTAPI_STRING_ROLE),
    ),
    host_fn(
        "testapi/fmt",
        "int_to_string",
        HostFnBodyKind::numeric_to_string(TESTAPI_I32_ROLE, TESTAPI_STRING_ROLE),
    ),
    host_fn(
        "testapi/fmt",
        "print_i32",
        HostFnBodyKind::print_i32(TESTAPI_I32_ROLE),
    ),
    host_fn(
        "testapi/io",
        "print",
        HostFnBodyKind::print(TESTAPI_STRING_ROLE),
    ),
];
const FNS_FMT_INT: &[HostFnBinding] = &[
    host_fn(
        "testapi/fmt",
        "bool_to_string",
        HostFnBodyKind::bool_to_string(TESTAPI_BOOL_ROLE, TESTAPI_STRING_ROLE),
    ),
    host_fn(
        "testapi/fmt",
        "i32_to_string",
        HostFnBodyKind::numeric_to_string(TESTAPI_INT_ROLE, TESTAPI_STRING_ROLE),
    ),
    host_fn(
        "testapi/fmt",
        "int_to_string",
        HostFnBodyKind::numeric_to_string(TESTAPI_INT_ROLE, TESTAPI_STRING_ROLE),
    ),
    host_fn(
        "testapi/fmt",
        "print_i32",
        HostFnBodyKind::print_i32(TESTAPI_INT_ROLE),
    ),
    host_fn(
        "testapi/io",
        "print",
        HostFnBodyKind::print(TESTAPI_STRING_ROLE),
    ),
];
const FNS_FMT_INT_ONLY: &[HostFnBinding] = &[
    host_fn(
        "testapi/fmt",
        "int_to_string",
        HostFnBodyKind::numeric_to_string(TESTAPI_INT_ROLE, TESTAPI_STRING_ROLE),
    ),
    host_fn(
        "testapi/io",
        "print",
        HostFnBodyKind::print(TESTAPI_STRING_ROLE),
    ),
];

const FNS_TEXT_I32: &[HostFnBinding] = &[
    host_fn(
        "testapi/fmt",
        "bool_to_string",
        HostFnBodyKind::bool_to_string(TESTAPI_BOOL_ROLE, TESTAPI_STRING_ROLE),
    ),
    host_fn(
        "testapi/fmt",
        "i32_to_string",
        HostFnBodyKind::numeric_to_string(TESTAPI_I32_ROLE, TESTAPI_STRING_ROLE),
    ),
    host_fn(
        "testapi/fmt",
        "int_to_string",
        HostFnBodyKind::numeric_to_string(TESTAPI_I32_ROLE, TESTAPI_STRING_ROLE),
    ),
    host_fn(
        "testapi/fmt",
        "print_i32",
        HostFnBodyKind::print_i32(TESTAPI_I32_ROLE),
    ),
    host_fn(
        "testapi/io",
        "print",
        HostFnBodyKind::print(TESTAPI_STRING_ROLE),
    ),
    host_fn(
        "testapi/text",
        "string_code_at",
        HostFnBodyKind::string_code_at(TESTAPI_STRING_ROLE, TESTAPI_I32_ROLE),
    ),
    host_fn(
        "testapi/text",
        "string_concat",
        HostFnBodyKind::string_concat(TESTAPI_STRING_ROLE),
    ),
    host_fn(
        "testapi/text",
        "string_eq",
        HostFnBodyKind::string_eq(TESTAPI_STRING_ROLE, TESTAPI_BOOL_ROLE),
    ),
    host_fn(
        "testapi/text",
        "string_len",
        HostFnBodyKind::string_len(TESTAPI_STRING_ROLE, TESTAPI_I32_ROLE),
    ),
    host_fn(
        "testapi/text",
        "string_slice",
        HostFnBodyKind::string_slice(TESTAPI_STRING_ROLE, TESTAPI_I32_ROLE),
    ),
];
const FNS_TEXT_INT: &[HostFnBinding] = &[
    host_fn(
        "testapi/fmt",
        "bool_to_string",
        HostFnBodyKind::bool_to_string(TESTAPI_BOOL_ROLE, TESTAPI_STRING_ROLE),
    ),
    host_fn(
        "testapi/fmt",
        "i32_to_string",
        HostFnBodyKind::numeric_to_string(TESTAPI_INT_ROLE, TESTAPI_STRING_ROLE),
    ),
    host_fn(
        "testapi/fmt",
        "int_to_string",
        HostFnBodyKind::numeric_to_string(TESTAPI_INT_ROLE, TESTAPI_STRING_ROLE),
    ),
    host_fn(
        "testapi/fmt",
        "print_i32",
        HostFnBodyKind::print_i32(TESTAPI_INT_ROLE),
    ),
    host_fn(
        "testapi/io",
        "print",
        HostFnBodyKind::print(TESTAPI_STRING_ROLE),
    ),
    host_fn(
        "testapi/text",
        "string_code_at",
        HostFnBodyKind::string_code_at(TESTAPI_STRING_ROLE, TESTAPI_INT_ROLE),
    ),
    host_fn(
        "testapi/text",
        "string_concat",
        HostFnBodyKind::string_concat(TESTAPI_STRING_ROLE),
    ),
    host_fn(
        "testapi/text",
        "string_eq",
        HostFnBodyKind::string_eq(TESTAPI_STRING_ROLE, TESTAPI_BOOL_ROLE),
    ),
    host_fn(
        "testapi/text",
        "string_len",
        HostFnBodyKind::string_len(TESTAPI_STRING_ROLE, TESTAPI_INT_ROLE),
    ),
    host_fn(
        "testapi/text",
        "string_slice",
        HostFnBodyKind::string_slice(TESTAPI_STRING_ROLE, TESTAPI_INT_ROLE),
    ),
];
const FNS_TEXT_CONCAT: &[HostFnBinding] = &[
    host_fn(
        "testapi/io",
        "print",
        HostFnBodyKind::print(TESTAPI_STRING_ROLE),
    ),
    host_fn(
        "testapi/text",
        "string_concat",
        HostFnBodyKind::string_concat(TESTAPI_STRING_ROLE),
    ),
];

const FNS_ARITH_I32: &[HostFnBinding] = &[
    host_fn(
        "testapi/arith",
        "add_i32",
        HostFnBodyKind::arithmetic("add", TESTAPI_I32_ROLE),
    ),
    host_fn(
        "testapi/arith",
        "mul_i32",
        HostFnBodyKind::arithmetic("mul", TESTAPI_I32_ROLE),
    ),
    host_fn(
        "testapi/arith",
        "sub_i32",
        HostFnBodyKind::arithmetic("sub", TESTAPI_I32_ROLE),
    ),
    host_fn(
        "testapi/fmt",
        "i32_to_string",
        HostFnBodyKind::numeric_to_string(TESTAPI_I32_ROLE, TESTAPI_STRING_ROLE),
    ),
    host_fn(
        "testapi/fmt",
        "int_to_string",
        HostFnBodyKind::numeric_to_string(TESTAPI_I32_ROLE, TESTAPI_STRING_ROLE),
    ),
    host_fn(
        "testapi/io",
        "print",
        HostFnBodyKind::print(TESTAPI_STRING_ROLE),
    ),
];
const FNS_ARITH_ADD_I32: &[HostFnBinding] = &[
    host_fn(
        "testapi/arith",
        "add_i32",
        HostFnBodyKind::arithmetic("add", TESTAPI_I32_ROLE),
    ),
    host_fn(
        "testapi/fmt",
        "i32_to_string",
        HostFnBodyKind::numeric_to_string(TESTAPI_I32_ROLE, TESTAPI_STRING_ROLE),
    ),
    host_fn(
        "testapi/io",
        "print",
        HostFnBodyKind::print(TESTAPI_STRING_ROLE),
    ),
];
const FNS_ARITH_ADD_INT: &[HostFnBinding] = &[
    host_fn(
        "testapi/arith",
        "add_i32",
        HostFnBodyKind::arithmetic("add", TESTAPI_I32_ROLE),
    ),
    host_fn(
        "testapi/fmt",
        "int_to_string",
        HostFnBodyKind::numeric_to_string(TESTAPI_I32_ROLE, TESTAPI_STRING_ROLE),
    ),
    host_fn(
        "testapi/io",
        "print",
        HostFnBodyKind::print(TESTAPI_STRING_ROLE),
    ),
];

const FNS_ARITH_COLLECTION_I32: &[HostFnBinding] = &[
    host_fn(
        "testapi/arith",
        "add_i32",
        HostFnBodyKind::arithmetic("add", TESTAPI_I32_ROLE),
    ),
    host_fn(
        "testapi/arith",
        "eq_i32",
        HostFnBodyKind::compare("eq", TESTAPI_I32_ROLE, TESTAPI_BOOL_ROLE),
    ),
    host_fn(
        "testapi/arith",
        "leq_i32",
        HostFnBodyKind::compare("leq", TESTAPI_I32_ROLE, TESTAPI_BOOL_ROLE),
    ),
    host_fn(
        "testapi/arith",
        "lt_i32",
        HostFnBodyKind::compare("lt", TESTAPI_I32_ROLE, TESTAPI_BOOL_ROLE),
    ),
    host_fn(
        "testapi/arith",
        "mul_i32",
        HostFnBodyKind::arithmetic("mul", TESTAPI_I32_ROLE),
    ),
    host_fn(
        "testapi/arith",
        "sub_i32",
        HostFnBodyKind::arithmetic("sub", TESTAPI_I32_ROLE),
    ),
    host_fn(
        "testapi/fmt",
        "bool_to_string",
        HostFnBodyKind::bool_to_string(TESTAPI_BOOL_ROLE, TESTAPI_STRING_ROLE),
    ),
    host_fn(
        "testapi/fmt",
        "int_to_string",
        HostFnBodyKind::numeric_to_string(TESTAPI_I32_ROLE, TESTAPI_STRING_ROLE),
    ),
    host_fn(
        "testapi/io",
        "print",
        HostFnBodyKind::print(TESTAPI_STRING_ROLE),
    ),
    host_fn("testapi/iter", "loop", HostFnBodyKind::Loop),
    host_fn(
        "testapi/text",
        "string_concat",
        HostFnBodyKind::string_concat(TESTAPI_STRING_ROLE),
    ),
];
const FNS_ARITH_COLLECTION_NO_BOOL: &[HostFnBinding] = &[
    FNS_ARITH_COLLECTION_I32[0],
    FNS_ARITH_COLLECTION_I32[1],
    FNS_ARITH_COLLECTION_I32[2],
    FNS_ARITH_COLLECTION_I32[3],
    FNS_ARITH_COLLECTION_I32[4],
    FNS_ARITH_COLLECTION_I32[5],
    FNS_ARITH_COLLECTION_I32[7],
    FNS_ARITH_COLLECTION_I32[8],
    FNS_ARITH_COLLECTION_I32[9],
    FNS_ARITH_COLLECTION_I32[10],
];

const FNS_BARE_ARITH_INT: &[HostFnBinding] = &[
    host_fn(
        "testapi/arith",
        "add",
        HostFnBodyKind::arithmetic("add", TESTAPI_INT_ROLE),
    ),
    host_fn(
        "testapi/arith",
        "div",
        HostFnBodyKind::arithmetic("div", TESTAPI_INT_ROLE),
    ),
    host_fn(
        "testapi/arith",
        "mod",
        HostFnBodyKind::arithmetic("mod", TESTAPI_INT_ROLE),
    ),
    host_fn(
        "testapi/arith",
        "mul",
        HostFnBodyKind::arithmetic("mul", TESTAPI_INT_ROLE),
    ),
    host_fn(
        "testapi/arith",
        "sub",
        HostFnBodyKind::arithmetic("sub", TESTAPI_INT_ROLE),
    ),
    host_fn(
        "testapi/fmt",
        "int_to_string",
        HostFnBodyKind::numeric_to_string(TESTAPI_INT_ROLE, TESTAPI_STRING_ROLE),
    ),
    host_fn(
        "testapi/io",
        "print",
        HostFnBodyKind::print(TESTAPI_STRING_ROLE),
    ),
];
const FNS_BARE_ARITH_I32: &[HostFnBinding] = &[
    host_fn(
        "testapi/arith",
        "add",
        HostFnBodyKind::arithmetic("add", TESTAPI_I32_ROLE),
    ),
    host_fn(
        "testapi/arith",
        "div",
        HostFnBodyKind::arithmetic("div", TESTAPI_I32_ROLE),
    ),
    host_fn(
        "testapi/arith",
        "mod",
        HostFnBodyKind::arithmetic("mod", TESTAPI_I32_ROLE),
    ),
    host_fn(
        "testapi/arith",
        "mul",
        HostFnBodyKind::arithmetic("mul", TESTAPI_I32_ROLE),
    ),
    host_fn(
        "testapi/arith",
        "sub",
        HostFnBodyKind::arithmetic("sub", TESTAPI_I32_ROLE),
    ),
    host_fn(
        "testapi/fmt",
        "int_to_string",
        HostFnBodyKind::numeric_to_string(TESTAPI_I32_ROLE, TESTAPI_STRING_ROLE),
    ),
    host_fn(
        "testapi/io",
        "print",
        HostFnBodyKind::print(TESTAPI_STRING_ROLE),
    ),
];

const FNS_BARE_COLLECTION_I32: &[HostFnBinding] = &[
    FNS_BARE_ARITH_I32[0],
    FNS_BARE_ARITH_I32[1],
    host_fn(
        "testapi/arith",
        "eq_i32",
        HostFnBodyKind::compare("eq", TESTAPI_I32_ROLE, TESTAPI_BOOL_ROLE),
    ),
    host_fn(
        "testapi/arith",
        "leq_i32",
        HostFnBodyKind::compare("leq", TESTAPI_I32_ROLE, TESTAPI_BOOL_ROLE),
    ),
    host_fn(
        "testapi/arith",
        "lt_i32",
        HostFnBodyKind::compare("lt", TESTAPI_I32_ROLE, TESTAPI_BOOL_ROLE),
    ),
    FNS_BARE_ARITH_I32[2],
    FNS_BARE_ARITH_I32[3],
    FNS_BARE_ARITH_I32[4],
    host_fn(
        "testapi/fmt",
        "bool_to_string",
        HostFnBodyKind::bool_to_string(TESTAPI_BOOL_ROLE, TESTAPI_STRING_ROLE),
    ),
    FNS_BARE_ARITH_I32[5],
    FNS_BARE_ARITH_I32[6],
    host_fn("testapi/iter", "loop", HostFnBodyKind::Loop),
    host_fn(
        "testapi/text",
        "string_concat",
        HostFnBodyKind::string_concat(TESTAPI_STRING_ROLE),
    ),
    host_fn(
        "testapi/text",
        "string_eq",
        HostFnBodyKind::string_eq(TESTAPI_STRING_ROLE, TESTAPI_BOOL_ROLE),
    ),
];
const FNS_BARE_COLLECTION_REDUCED: &[HostFnBinding] = &[
    FNS_BARE_COLLECTION_I32[0],
    FNS_BARE_COLLECTION_I32[1],
    FNS_BARE_COLLECTION_I32[2],
    FNS_BARE_COLLECTION_I32[4],
    FNS_BARE_COLLECTION_I32[5],
    FNS_BARE_COLLECTION_I32[6],
    FNS_BARE_COLLECTION_I32[7],
    FNS_BARE_COLLECTION_I32[9],
    FNS_BARE_COLLECTION_I32[10],
    FNS_BARE_COLLECTION_I32[11],
    FNS_BARE_COLLECTION_I32[12],
];

const FNS_BARE_COMPUTE_INT: &[HostFnBinding] = &[
    FNS_BARE_ARITH_INT[0],
    FNS_BARE_ARITH_INT[1],
    FNS_BARE_ARITH_INT[2],
    FNS_BARE_ARITH_INT[3],
    FNS_BARE_ARITH_INT[4],
    FNS_BARE_ARITH_INT[5],
    host_fn(
        "testapi/fmt",
        "string_to_int",
        HostFnBodyKind::string_to_int(TESTAPI_STRING_ROLE, TESTAPI_INT_ROLE),
    ),
    FNS_BARE_ARITH_INT[6],
    host_fn(
        "testapi/io",
        "read_ascii_line",
        HostFnBodyKind::read_ascii_line(TESTAPI_STRING_ROLE),
    ),
    host_fn("testapi/iter", "loop", HostFnBodyKind::Loop),
    host_fn(
        "testapi/text",
        "string_concat",
        HostFnBodyKind::string_concat(TESTAPI_STRING_ROLE),
    ),
    host_fn(
        "testapi/text",
        "string_eq",
        HostFnBodyKind::string_eq(TESTAPI_STRING_ROLE, TESTAPI_BOOL_ROLE),
    ),
];
const FNS_BARE_COMPUTE_I32_REDUCED: &[HostFnBinding] = &[
    FNS_BARE_ARITH_I32[0],
    FNS_BARE_ARITH_I32[1],
    FNS_BARE_ARITH_I32[3],
    FNS_BARE_ARITH_I32[4],
    FNS_BARE_ARITH_I32[5],
    FNS_BARE_ARITH_I32[6],
    host_fn(
        "testapi/io",
        "read_ascii_line",
        HostFnBodyKind::read_ascii_line(TESTAPI_STRING_ROLE),
    ),
    host_fn("testapi/iter", "loop", HostFnBodyKind::Loop),
    host_fn(
        "testapi/text",
        "string_concat",
        HostFnBodyKind::string_concat(TESTAPI_STRING_ROLE),
    ),
    host_fn(
        "testapi/text",
        "string_eq",
        HostFnBodyKind::string_eq(TESTAPI_STRING_ROLE, TESTAPI_BOOL_ROLE),
    ),
];

const FNS_ARRAY_FULL: &[HostFnBinding] = &[
    host_fn(
        "testapi/arith",
        "add_i32",
        HostFnBodyKind::arithmetic("add", TESTAPI_I32_ROLE),
    ),
    host_fn(
        "testapi/arith",
        "leq_i32",
        HostFnBodyKind::compare("leq", TESTAPI_I32_ROLE, TESTAPI_BOOL_ROLE),
    ),
    host_fn(
        "testapi/arith",
        "mul_i32",
        HostFnBodyKind::arithmetic("mul", TESTAPI_I32_ROLE),
    ),
    host_fn(
        "testapi/arith",
        "sub_i32",
        HostFnBodyKind::arithmetic("sub", TESTAPI_I32_ROLE),
    ),
    host_fn(
        "testapi/array",
        "array_clear",
        HostFnBodyKind::array("clear", TESTAPI_ARRAY_TYPE, None),
    ),
    host_fn(
        "testapi/array",
        "array_clone",
        HostFnBodyKind::array("clone", TESTAPI_ARRAY_TYPE, None),
    ),
    host_fn(
        "testapi/array",
        "array_get",
        HostFnBodyKind::array("get", TESTAPI_ARRAY_TYPE, Some(TESTAPI_I32_ROLE)),
    ),
    host_fn(
        "testapi/array",
        "array_len",
        HostFnBodyKind::array("len", TESTAPI_ARRAY_TYPE, Some(TESTAPI_I32_ROLE)),
    ),
    host_fn(
        "testapi/array",
        "array_make_empty",
        HostFnBodyKind::array("make-empty", TESTAPI_ARRAY_TYPE, None),
    ),
    host_fn(
        "testapi/array",
        "array_make_filled",
        HostFnBodyKind::array("make-filled", TESTAPI_ARRAY_TYPE, Some(TESTAPI_I32_ROLE)),
    ),
    host_fn(
        "testapi/array",
        "array_pop_back",
        HostFnBodyKind::array("pop-back", TESTAPI_ARRAY_TYPE, None),
    ),
    host_fn(
        "testapi/array",
        "array_push",
        HostFnBodyKind::array("push", TESTAPI_ARRAY_TYPE, None),
    ),
    host_fn(
        "testapi/array",
        "array_set",
        HostFnBodyKind::array("set", TESTAPI_ARRAY_TYPE, Some(TESTAPI_I32_ROLE)),
    ),
    host_fn(
        "testapi/array",
        "array_swap",
        HostFnBodyKind::array("swap", TESTAPI_ARRAY_TYPE, Some(TESTAPI_I32_ROLE)),
    ),
    host_fn(
        "testapi/fmt",
        "bool_to_string",
        HostFnBodyKind::bool_to_string(TESTAPI_BOOL_ROLE, TESTAPI_STRING_ROLE),
    ),
    host_fn(
        "testapi/fmt",
        "int_to_string",
        HostFnBodyKind::numeric_to_string(TESTAPI_I32_ROLE, TESTAPI_STRING_ROLE),
    ),
    host_fn(
        "testapi/io",
        "print",
        HostFnBodyKind::print(TESTAPI_STRING_ROLE),
    ),
    host_fn("testapi/iter", "loop", HostFnBodyKind::Loop),
    host_fn(
        "testapi/text",
        "string_concat",
        HostFnBodyKind::string_concat(TESTAPI_STRING_ROLE),
    ),
    host_fn(
        "testapi/text",
        "string_eq",
        HostFnBodyKind::string_eq(TESTAPI_STRING_ROLE, TESTAPI_BOOL_ROLE),
    ),
];
const FNS_ARRAY_NO_CLEAR: &[HostFnBinding] = &[
    FNS_ARRAY_FULL[0],
    FNS_ARRAY_FULL[1],
    FNS_ARRAY_FULL[2],
    FNS_ARRAY_FULL[3],
    FNS_ARRAY_FULL[5],
    FNS_ARRAY_FULL[6],
    FNS_ARRAY_FULL[7],
    FNS_ARRAY_FULL[8],
    FNS_ARRAY_FULL[9],
    FNS_ARRAY_FULL[10],
    FNS_ARRAY_FULL[11],
    FNS_ARRAY_FULL[12],
    FNS_ARRAY_FULL[13],
    FNS_ARRAY_FULL[14],
    FNS_ARRAY_FULL[15],
    FNS_ARRAY_FULL[16],
    FNS_ARRAY_FULL[17],
    FNS_ARRAY_FULL[18],
    FNS_ARRAY_FULL[19],
];
const FNS_ARRAY_CLEAR: &[HostFnBinding] = &[
    FNS_ARRAY_FULL[4],
    FNS_ARRAY_FULL[7],
    FNS_ARRAY_FULL[9],
    FNS_ARRAY_FULL[15],
    FNS_ARRAY_FULL[16],
];
const FNS_HOST_ARRAY_RETURN_ONLY: &[HostFnBinding] = &[
    FNS_ARRAY_NO_CLEAR[5],
    FNS_ARRAY_NO_CLEAR[7],
    FNS_ARRAY_NO_CLEAR[10],
    FNS_ARRAY_NO_CLEAR[14],
    FNS_ARRAY_NO_CLEAR[15],
];
const FNS_ARRAY_NO_CLONE_SWAP_STRING_EQ: &[HostFnBinding] = &[
    FNS_ARRAY_NO_CLEAR[0],
    FNS_ARRAY_NO_CLEAR[1],
    FNS_ARRAY_NO_CLEAR[2],
    FNS_ARRAY_NO_CLEAR[3],
    FNS_ARRAY_NO_CLEAR[5],
    FNS_ARRAY_NO_CLEAR[6],
    FNS_ARRAY_NO_CLEAR[7],
    FNS_ARRAY_NO_CLEAR[8],
    FNS_ARRAY_NO_CLEAR[9],
    FNS_ARRAY_NO_CLEAR[10],
    FNS_ARRAY_NO_CLEAR[11],
    FNS_ARRAY_NO_CLEAR[13],
    FNS_ARRAY_NO_CLEAR[14],
    FNS_ARRAY_NO_CLEAR[15],
    FNS_ARRAY_NO_CLEAR[16],
    FNS_ARRAY_NO_CLEAR[17],
];
const FNS_ARRAY_PUSH_ONLY: &[HostFnBinding] = &[
    FNS_ARRAY_NO_CLEAR[0],
    FNS_ARRAY_NO_CLEAR[1],
    FNS_ARRAY_NO_CLEAR[2],
    FNS_ARRAY_NO_CLEAR[3],
    FNS_ARRAY_NO_CLEAR[5],
    FNS_ARRAY_NO_CLEAR[6],
    FNS_ARRAY_NO_CLEAR[7],
    FNS_ARRAY_NO_CLEAR[8],
    FNS_ARRAY_NO_CLEAR[10],
    FNS_ARRAY_NO_CLEAR[11],
    FNS_ARRAY_NO_CLEAR[14],
    FNS_ARRAY_NO_CLEAR[15],
    FNS_ARRAY_NO_CLEAR[16],
    FNS_ARRAY_NO_CLEAR[17],
];
const FNS_ARRAY_FIXED: &[HostFnBinding] = &[
    FNS_ARRAY_NO_CLEAR[0],
    FNS_ARRAY_NO_CLEAR[1],
    FNS_ARRAY_NO_CLEAR[2],
    FNS_ARRAY_NO_CLEAR[3],
    FNS_ARRAY_NO_CLEAR[5],
    FNS_ARRAY_NO_CLEAR[6],
    FNS_ARRAY_NO_CLEAR[8],
    FNS_ARRAY_NO_CLEAR[11],
    FNS_ARRAY_NO_CLEAR[13],
    FNS_ARRAY_NO_CLEAR[14],
    FNS_ARRAY_NO_CLEAR[15],
    FNS_ARRAY_NO_CLEAR[16],
    FNS_ARRAY_NO_CLEAR[17],
];
const FNS_ARRAY_STACK: &[HostFnBinding] = &[
    FNS_ARRAY_NO_CLEAR[0],
    FNS_ARRAY_NO_CLEAR[1],
    FNS_ARRAY_NO_CLEAR[3],
    FNS_ARRAY_NO_CLEAR[5],
    FNS_ARRAY_NO_CLEAR[6],
    FNS_ARRAY_NO_CLEAR[7],
    FNS_ARRAY_NO_CLEAR[8],
    FNS_ARRAY_NO_CLEAR[9],
    FNS_ARRAY_NO_CLEAR[10],
    FNS_ARRAY_NO_CLEAR[11],
    FNS_ARRAY_NO_CLEAR[14],
    FNS_ARRAY_NO_CLEAR[15],
    FNS_ARRAY_NO_CLEAR[16],
    FNS_ARRAY_NO_CLEAR[17],
];
const FNS_ARRAY_GET_FILLED: &[HostFnBinding] = &[
    FNS_ARRAY_NO_CLEAR[5],
    FNS_ARRAY_NO_CLEAR[8],
    FNS_ARRAY_NO_CLEAR[15],
];

const FNS_BIGINT: &[HostFnBinding] = &[
    host_fn(
        "testapi/arith",
        "add_i128",
        HostFnBodyKind::arithmetic("add", TESTAPI_I128_ROLE),
    ),
    host_fn(
        "testapi/arith",
        "add_i64",
        HostFnBodyKind::arithmetic("add", TESTAPI_I64_ROLE),
    ),
    host_fn(
        "testapi/arith",
        "mul_u64",
        HostFnBodyKind::arithmetic("mul", TESTAPI_U64_ROLE),
    ),
    host_fn(
        "testapi/fmt",
        "i128_to_string",
        HostFnBodyKind::numeric_to_string(TESTAPI_I128_ROLE, TESTAPI_STRING_ROLE),
    ),
    host_fn(
        "testapi/fmt",
        "i64_to_string",
        HostFnBodyKind::numeric_to_string(TESTAPI_I64_ROLE, TESTAPI_STRING_ROLE),
    ),
    host_fn(
        "testapi/fmt",
        "u64_to_string",
        HostFnBodyKind::numeric_to_string(TESTAPI_U64_ROLE, TESTAPI_STRING_ROLE),
    ),
    host_fn(
        "testapi/io",
        "print",
        HostFnBodyKind::print(TESTAPI_STRING_ROLE),
    ),
];
const FNS_BIGINT_U128: &[HostFnBinding] = &[
    FNS_BIGINT[0],
    FNS_BIGINT[1],
    host_fn(
        "testapi/arith",
        "add_u128",
        HostFnBodyKind::arithmetic("add", TESTAPI_U128_ROLE),
    ),
    FNS_BIGINT[2],
    FNS_BIGINT[3],
    FNS_BIGINT[4],
    host_fn(
        "testapi/fmt",
        "u128_to_string",
        HostFnBodyKind::numeric_to_string(TESTAPI_U128_ROLE, TESTAPI_STRING_ROLE),
    ),
    FNS_BIGINT[5],
    FNS_BIGINT[6],
];

const FNS_FLOAT_F64: &[HostFnBinding] = &[
    host_fn(
        "testapi/arith",
        "add_f64",
        HostFnBodyKind::float_arithmetic("add", TESTAPI_F64_ROLE),
    ),
    host_fn(
        "testapi/arith",
        "mul_f64",
        HostFnBodyKind::float_arithmetic("mul", TESTAPI_F64_ROLE),
    ),
    host_fn(
        "testapi/arith",
        "sub_f64",
        HostFnBodyKind::float_arithmetic("sub", TESTAPI_F64_ROLE),
    ),
    host_fn(
        "testapi/fmt",
        "f64_to_string",
        HostFnBodyKind::numeric_to_string(TESTAPI_F64_ROLE, TESTAPI_STRING_ROLE),
    ),
    host_fn(
        "testapi/io",
        "print",
        HostFnBodyKind::print(TESTAPI_STRING_ROLE),
    ),
];
const FNS_FLOAT_F32_F64: &[HostFnBinding] = &[
    host_fn(
        "testapi/arith",
        "add_f32",
        HostFnBodyKind::float_arithmetic("add", TESTAPI_F32_ROLE),
    ),
    FNS_FLOAT_F64[0],
    FNS_FLOAT_F64[1],
    FNS_FLOAT_F64[2],
    host_fn(
        "testapi/fmt",
        "f32_to_string",
        HostFnBodyKind::numeric_to_string(TESTAPI_F32_ROLE, TESTAPI_STRING_ROLE),
    ),
    FNS_FLOAT_F64[3],
    FNS_FLOAT_F64[4],
];

const FNS_COMPUTE_I32: &[HostFnBinding] = &[
    host_fn(
        "testapi/arith",
        "add_i32",
        HostFnBodyKind::arithmetic("add", TESTAPI_I32_ROLE),
    ),
    host_fn(
        "testapi/arith",
        "div_i32",
        HostFnBodyKind::arithmetic("div", TESTAPI_I32_ROLE),
    ),
    host_fn(
        "testapi/arith",
        "leq_i32",
        HostFnBodyKind::compare("leq", TESTAPI_I32_ROLE, TESTAPI_BOOL_ROLE),
    ),
    host_fn(
        "testapi/arith",
        "lt_i32",
        HostFnBodyKind::compare("lt", TESTAPI_I32_ROLE, TESTAPI_BOOL_ROLE),
    ),
    host_fn(
        "testapi/arith",
        "mod_i32",
        HostFnBodyKind::arithmetic("mod", TESTAPI_I32_ROLE),
    ),
    host_fn(
        "testapi/arith",
        "mul_i32",
        HostFnBodyKind::arithmetic("mul", TESTAPI_I32_ROLE),
    ),
    host_fn(
        "testapi/arith",
        "sub_i32",
        HostFnBodyKind::arithmetic("sub", TESTAPI_I32_ROLE),
    ),
    host_fn(
        "testapi/fmt",
        "i32_to_string",
        HostFnBodyKind::numeric_to_string(TESTAPI_I32_ROLE, TESTAPI_STRING_ROLE),
    ),
    host_fn(
        "testapi/fmt",
        "int_to_string",
        HostFnBodyKind::numeric_to_string(TESTAPI_I32_ROLE, TESTAPI_STRING_ROLE),
    ),
    host_fn(
        "testapi/fmt",
        "string_to_int",
        HostFnBodyKind::string_to_int(TESTAPI_STRING_ROLE, TESTAPI_I32_ROLE),
    ),
    host_fn(
        "testapi/io",
        "print",
        HostFnBodyKind::print(TESTAPI_STRING_ROLE),
    ),
    host_fn(
        "testapi/io",
        "read_ascii_line",
        HostFnBodyKind::read_ascii_line(TESTAPI_STRING_ROLE),
    ),
    host_fn("testapi/iter", "loop", HostFnBodyKind::Loop),
    host_fn(
        "testapi/text",
        "string_concat",
        HostFnBodyKind::string_concat(TESTAPI_STRING_ROLE),
    ),
    host_fn(
        "testapi/text",
        "string_eq",
        HostFnBodyKind::string_eq(TESTAPI_STRING_ROLE, TESTAPI_BOOL_ROLE),
    ),
];
const FNS_COMPUTE_LOOP: &[HostFnBinding] = &[FNS_COMPUTE_I32[12]];
const FNS_COMPUTE_NO_I32_FORMAT: &[HostFnBinding] = &[
    FNS_COMPUTE_I32[0],
    FNS_COMPUTE_I32[1],
    FNS_COMPUTE_I32[2],
    FNS_COMPUTE_I32[3],
    FNS_COMPUTE_I32[4],
    FNS_COMPUTE_I32[5],
    FNS_COMPUTE_I32[6],
    FNS_COMPUTE_I32[8],
    FNS_COMPUTE_I32[9],
    FNS_COMPUTE_I32[10],
    FNS_COMPUTE_I32[11],
    FNS_COMPUTE_I32[12],
    FNS_COMPUTE_I32[13],
    FNS_COMPUTE_I32[14],
];
const FNS_COMPUTE_NO_INPUT_PARSE: &[HostFnBinding] = &[
    FNS_COMPUTE_I32[0],
    FNS_COMPUTE_I32[1],
    FNS_COMPUTE_I32[2],
    FNS_COMPUTE_I32[3],
    FNS_COMPUTE_I32[4],
    FNS_COMPUTE_I32[5],
    FNS_COMPUTE_I32[6],
    FNS_COMPUTE_I32[8],
    FNS_COMPUTE_I32[10],
    FNS_COMPUTE_I32[12],
    FNS_COMPUTE_I32[13],
    FNS_COMPUTE_I32[14],
];
const FNS_COMPUTE_DIFF: &[HostFnBinding] = &[
    FNS_COMPUTE_I32[0],
    FNS_COMPUTE_I32[2],
    FNS_COMPUTE_I32[3],
    FNS_COMPUTE_I32[5],
    FNS_COMPUTE_I32[6],
    FNS_COMPUTE_I32[8],
    FNS_COMPUTE_I32[10],
    FNS_COMPUTE_I32[11],
    FNS_COMPUTE_I32[12],
    FNS_COMPUTE_I32[13],
    FNS_COMPUTE_I32[14],
];
const FNS_COMPUTE_REC_BINDER: &[HostFnBinding] = &[
    FNS_COMPUTE_I32[2],
    FNS_COMPUTE_I32[6],
    FNS_COMPUTE_I32[7],
    FNS_COMPUTE_I32[10],
    FNS_COMPUTE_I32[12],
];
const FNS_COMPUTE_REC_ORDER: &[HostFnBinding] =
    &[FNS_COMPUTE_I32[2], FNS_COMPUTE_I32[10], FNS_COMPUTE_I32[12]];
const FNS_COMPUTE_REC_PARTIAL_LET: &[HostFnBinding] = &[
    FNS_COMPUTE_I32[2],
    FNS_COMPUTE_I32[7],
    FNS_COMPUTE_I32[10],
    FNS_COMPUTE_I32[12],
];
const FNS_COMPUTE_SUB_PRINT: &[HostFnBinding] =
    &[FNS_COMPUTE_I32[6], FNS_COMPUTE_I32[8], FNS_COMPUTE_I32[10]];

const FNS_COMPUTE_INT: &[HostFnBinding] = &[
    host_fn(
        "testapi/arith",
        "add_i32",
        HostFnBodyKind::arithmetic("add", TESTAPI_INT_ROLE),
    ),
    host_fn(
        "testapi/arith",
        "div_i32",
        HostFnBodyKind::arithmetic("div", TESTAPI_INT_ROLE),
    ),
    host_fn(
        "testapi/arith",
        "leq_i32",
        HostFnBodyKind::compare("leq", TESTAPI_INT_ROLE, TESTAPI_BOOL_ROLE),
    ),
    host_fn(
        "testapi/arith",
        "lt_i32",
        HostFnBodyKind::compare("lt", TESTAPI_INT_ROLE, TESTAPI_BOOL_ROLE),
    ),
    host_fn(
        "testapi/arith",
        "mod_i32",
        HostFnBodyKind::arithmetic("mod", TESTAPI_INT_ROLE),
    ),
    host_fn(
        "testapi/arith",
        "mul_i32",
        HostFnBodyKind::arithmetic("mul", TESTAPI_INT_ROLE),
    ),
    host_fn(
        "testapi/arith",
        "sub_i32",
        HostFnBodyKind::arithmetic("sub", TESTAPI_INT_ROLE),
    ),
    host_fn(
        "testapi/fmt",
        "i32_to_string",
        HostFnBodyKind::numeric_to_string(TESTAPI_INT_ROLE, TESTAPI_STRING_ROLE),
    ),
    host_fn(
        "testapi/fmt",
        "int_to_string",
        HostFnBodyKind::numeric_to_string(TESTAPI_INT_ROLE, TESTAPI_STRING_ROLE),
    ),
    host_fn(
        "testapi/fmt",
        "string_to_int",
        HostFnBodyKind::string_to_int(TESTAPI_STRING_ROLE, TESTAPI_INT_ROLE),
    ),
    FNS_COMPUTE_I32[10],
    FNS_COMPUTE_I32[11],
    FNS_COMPUTE_I32[12],
    FNS_COMPUTE_I32[13],
    FNS_COMPUTE_I32[14],
];
const FNS_COMPUTE_INT_STR: &[HostFnBinding] = &[
    FNS_COMPUTE_INT[0],
    FNS_COMPUTE_INT[1],
    FNS_COMPUTE_INT[2],
    FNS_COMPUTE_INT[3],
    FNS_COMPUTE_INT[4],
    FNS_COMPUTE_INT[5],
    FNS_COMPUTE_INT[6],
    host_fn(
        "testapi/fmt",
        "i32_to_string",
        HostFnBodyKind::numeric_to_string(TESTAPI_INT_ROLE, TESTAPI_STR_ROLE),
    ),
    host_fn(
        "testapi/fmt",
        "int_to_string",
        HostFnBodyKind::numeric_to_string(TESTAPI_INT_ROLE, TESTAPI_STR_ROLE),
    ),
    host_fn(
        "testapi/fmt",
        "string_to_int",
        HostFnBodyKind::string_to_int(TESTAPI_STR_ROLE, TESTAPI_INT_ROLE),
    ),
    host_fn(
        "testapi/io",
        "print",
        HostFnBodyKind::print(TESTAPI_STR_ROLE),
    ),
    host_fn(
        "testapi/io",
        "read_ascii_line",
        HostFnBodyKind::read_ascii_line(TESTAPI_STR_ROLE),
    ),
    FNS_COMPUTE_I32[12],
    host_fn(
        "testapi/text",
        "string_concat",
        HostFnBodyKind::string_concat(TESTAPI_STR_ROLE),
    ),
    host_fn(
        "testapi/text",
        "string_eq",
        HostFnBodyKind::string_eq(TESTAPI_STR_ROLE, TESTAPI_BOOL_ROLE),
    ),
];

const FNS_IO: &[HostFnBinding] = &[
    host_fn(
        "testapi/io",
        "eprint",
        HostFnBodyKind::eprint(TESTAPI_STRING_ROLE),
    ),
    host_fn("testapi/io", "exit", HostFnBodyKind::exit(TESTAPI_I32_ROLE)),
    host_fn(
        "testapi/io",
        "print",
        HostFnBodyKind::print(TESTAPI_STRING_ROLE),
    ),
    host_fn(
        "testapi/io",
        "read_ascii_line",
        HostFnBodyKind::read_ascii_line(TESTAPI_STRING_ROLE),
    ),
];

const FNS_DYN_LOAD: &[HostFnBinding] = &[
    FNS_PRINT_STRING[0],
    FNS_COMPUTE_I32[11],
    FNS_COMPUTE_I32[8],
    FNS_COMPUTE_I32[7],
    FNS_FMT_I32[0],
    FNS_FMT_I32[3],
    FNS_FLOAT_F64[3],
    FNS_COMPUTE_I32[9],
    FNS_BARE_ARITH_I32[0],
    FNS_BARE_ARITH_I32[4],
    FNS_BARE_ARITH_I32[3],
    FNS_BARE_ARITH_I32[1],
    FNS_BARE_ARITH_I32[2],
    FNS_COMPUTE_I32[0],
    FNS_COMPUTE_I32[6],
    FNS_COMPUTE_I32[5],
    FNS_COMPUTE_I32[1],
    FNS_COMPUTE_I32[4],
    FNS_ARITH_COLLECTION_I32[1],
    FNS_COMPUTE_I32[2],
    FNS_COMPUTE_I32[3],
    FNS_FLOAT_F64[0],
    FNS_FLOAT_F64[2],
    FNS_FLOAT_F64[1],
    FNS_TEXT_I32[6],
    FNS_TEXT_I32[7],
    FNS_TEXT_I32[8],
    FNS_TEXT_I32[9],
    FNS_TEXT_I32[5],
    FNS_COMPUTE_I32[12],
    host_fn(
        "testapi/scalar",
        "make_scalar",
        HostFnBodyKind::make_scalar(TESTAPI_STRING_ROLE, TESTAPI_SCALAR_TYPE),
    ),
    host_fn(
        "testapi/scalar",
        "scalar_of_i32",
        HostFnBodyKind::scalar_of(TESTAPI_I32_ROLE, TESTAPI_SCALAR_TYPE),
    ),
    host_fn(
        "testapi/scalar",
        "scalar_of_str",
        HostFnBodyKind::scalar_of(TESTAPI_STRING_ROLE, TESTAPI_SCALAR_TYPE),
    ),
    host_fn(
        "testapi/scalar",
        "scalar_of_bool",
        HostFnBodyKind::scalar_of(TESTAPI_BOOL_ROLE, TESTAPI_SCALAR_TYPE),
    ),
    host_fn(
        "testapi/scalar",
        "scalar_of_f64",
        HostFnBodyKind::scalar_of(TESTAPI_F64_ROLE, TESTAPI_SCALAR_TYPE),
    ),
    host_fn(
        "testapi/scalar",
        "scalar_as_i32",
        HostFnBodyKind::scalar_as(TESTAPI_I32_ROLE, TESTAPI_SCALAR_TYPE),
    ),
    host_fn(
        "testapi/scalar",
        "scalar_as_str",
        HostFnBodyKind::scalar_as(TESTAPI_STRING_ROLE, TESTAPI_SCALAR_TYPE),
    ),
    host_fn(
        "testapi/scalar",
        "scalar_as_bool",
        HostFnBodyKind::scalar_as(TESTAPI_BOOL_ROLE, TESTAPI_SCALAR_TYPE),
    ),
    host_fn(
        "testapi/scalar",
        "scalar_as_f64",
        HostFnBodyKind::scalar_as(TESTAPI_F64_ROLE, TESTAPI_SCALAR_TYPE),
    ),
    host_fn(
        "testapi/scalar",
        "scalar_is_true",
        HostFnBodyKind::scalar_is_true(TESTAPI_SCALAR_TYPE, TESTAPI_BOOL_ROLE),
    ),
];

const FNS_ELAB: &[HostFnBinding] = &[
    host_fn(
        "testapi/fmt",
        "bool_to_string",
        HostFnBodyKind::bool_to_string(TESTAPI_BOOL_ROLE, TESTAPI_STRING_ROLE),
    ),
    host_fn(
        "testapi/fmt",
        "i32_to_string",
        HostFnBodyKind::numeric_to_string(TESTAPI_I32_ROLE, TESTAPI_STRING_ROLE),
    ),
    FNS_PRINT_STRING[0],
    host_fn(
        "testapi/text",
        "string_concat",
        HostFnBodyKind::string_concat(TESTAPI_STRING_ROLE),
    ),
];
const FNS_ROOT_SCOPED: &[HostFnBinding] = &[
    host_fn(
        "testapi/alpha",
        "print",
        HostFnBodyKind::print(TESTAPI_STRING_ROLE),
    ),
    host_fn(
        "testapi/beta",
        "print",
        HostFnBodyKind::unreachable_i32_print(TESTAPI_I32_ROLE),
    ),
];
const FNS_HOST_TOKEN: &[HostFnBinding] = &[
    host_fn(
        "testapi/arith",
        "add_count",
        HostFnBodyKind::arithmetic("add", TESTAPI_COUNT_ROLE),
    ),
    host_fn(
        "testapi/fmt",
        "i32_to_string",
        HostFnBodyKind::numeric_to_string(TESTAPI_I32_ROLE, TESTAPI_STRING_ROLE),
    ),
    host_fn(
        "testapi/fmt",
        "count_to_string",
        HostFnBodyKind::numeric_to_string(TESTAPI_COUNT_ROLE, TESTAPI_STRING_ROLE),
    ),
    FNS_PRINT_STRING[0],
    host_fn(
        "testapi/opaque",
        "make_token",
        HostFnBodyKind::make_token(TESTAPI_I32_ROLE, TESTAPI_TOKEN_TYPE),
    ),
    host_fn(
        "testapi/opaque",
        "token_value",
        HostFnBodyKind::token_value(TESTAPI_TOKEN_TYPE, TESTAPI_I32_ROLE),
    ),
];
const FNS_HOST_BOX: &[HostFnBinding] = &[
    host_fn(
        "testapi/arith",
        "box_get",
        HostFnBodyKind::BoxGet {
            box_type: TESTAPI_BOX_TYPE,
        },
    ),
    host_fn(
        "testapi/arith",
        "box_make",
        HostFnBodyKind::BoxMake {
            box_type: TESTAPI_BOX_TYPE,
        },
    ),
    host_fn(
        "testapi/fmt",
        "i32_to_string",
        HostFnBodyKind::numeric_to_string(TESTAPI_I32_ROLE, TESTAPI_STRING_ROLE),
    ),
    FNS_PRINT_STRING[0],
];
const FNS_HOST_CALLBACK: &[HostFnBinding] = &[
    host_fn(
        "testapi/arith",
        "add_i32",
        HostFnBodyKind::arithmetic("add", TESTAPI_I32_ROLE),
    ),
    host_fn(
        "testapi/arith",
        "call_step",
        HostFnBodyKind::call_step(TESTAPI_I32_ROLE, TESTAPI_STRING_ROLE, TESTAPI_BOOL_ROLE),
    ),
    host_fn(
        "testapi/arith",
        "make_pair",
        HostFnBodyKind::make_pair_callback(TESTAPI_I32_ROLE, TESTAPI_STRING_ROLE),
    ),
    host_fn(
        "testapi/fmt",
        "bool_to_string",
        HostFnBodyKind::bool_to_string(TESTAPI_BOOL_ROLE, TESTAPI_STRING_ROLE),
    ),
    host_fn(
        "testapi/fmt",
        "i32_to_string",
        HostFnBodyKind::numeric_to_string(TESTAPI_I32_ROLE, TESTAPI_STRING_ROLE),
    ),
    FNS_PRINT_STRING[0],
];
const FNS_COMPOUND_INPUT_ONCE: &[HostFnBinding] = &[host_fn(
    "testapi",
    "produce",
    HostFnBodyKind::ProducePair {
        i32: TESTAPI_I32_ROLE,
        string: TESTAPI_STRING_ROLE,
    },
)];
const FNS_HOST_CALLBACK_RETURN: &[HostFnBinding] = &[
    host_fn(
        "testapi/arith",
        "make_step",
        HostFnBodyKind::make_step(TESTAPI_I32_ROLE),
    ),
    host_fn(
        "testapi/fmt",
        "i32_to_string",
        HostFnBodyKind::numeric_to_string(TESTAPI_I32_ROLE, TESTAPI_STRING_ROLE),
    ),
    FNS_PRINT_STRING[0],
];
const FNS_HOST_RANKN: &[HostFnBinding] = &[
    host_fn(
        "testapi/arith",
        "apply_poly",
        HostFnBodyKind::apply_poly(TESTAPI_STRING_ROLE),
    ),
    FNS_PRINT_STRING[0],
];
const FNS_HOST_STRUCTURAL: &[HostFnBinding] = &[
    host_fn(
        "testapi/arith",
        "make_pair",
        HostFnBodyKind::make_pair_structural(TESTAPI_I32_ROLE, TESTAPI_STRING_ROLE),
    ),
    host_fn(
        "testapi/fmt",
        "i32_to_string",
        HostFnBodyKind::numeric_to_string(TESTAPI_I32_ROLE, TESTAPI_STRING_ROLE),
    ),
    host_fn(
        "testapi/fmt",
        "sum_to_string",
        HostFnBodyKind::sum_to_string(TESTAPI_I32_ROLE, TESTAPI_STRING_ROLE),
    ),
    FNS_PRINT_STRING[0],
];
const FNS_HOST_FUNCTOR: &[HostFnBinding] = &[
    host_fn(
        "testapi/arith",
        "round_functor",
        HostFnBodyKind::RoundFunctor,
    ),
    FNS_PRINT_STRING[0],
];
const FNS_HOST_POLY_FUNCTION_NEWTYPE: &[HostFnBinding] = &[
    host_fn("testapi/arith", "round_picker", HostFnBodyKind::RoundPicker),
    FNS_PRINT_STRING[0],
];
const FNS_HOST_POLY_UNIT_PAYLOAD: &[HostFnBinding] = &[
    host_fn(
        "testapi/arith",
        "round_poly_thunk",
        HostFnBodyKind::RoundPolyThunk,
    ),
    host_fn(
        "testapi/arith",
        "round_unit_slot",
        HostFnBodyKind::RoundPolyUnitSlot,
    ),
    FNS_PRINT_STRING[0],
];
const FNS_HOST_INTERLEAVED_STAGE: &[HostFnBinding] = &[
    host_fn(
        "testapi/arith",
        "staged",
        HostFnBodyKind::StagedSecond {
            string: TESTAPI_STRING_ROLE,
        },
    ),
    FNS_PRINT_STRING[0],
];
const FNS_NESTED_CURRIED_ROUNDTRIP: &[HostFnBinding] = &[host_fn(
    "testapi/api",
    "round_host",
    HostFnBodyKind::NestedCurriedRoundtrip {
        string: TESTAPI_STRING_ROLE,
    },
)];
const FNS_HOST_SUBSTITUTED_UNIT_CALLBACK: &[HostFnBinding] = &[host_fn(
    "testapi/api",
    "invoke",
    HostFnBodyKind::InvokeSubstitutedUnitCallback {
        text: TESTAPI_TEXT_ROLE,
    },
)];
const FNS_RETURNED_FORALL_CALL_BY_VALUE: &[HostFnBinding] = &[
    host_fn(
        "testapi/main",
        "produce",
        HostFnBodyKind::ReturnedForallUnit,
    ),
    host_fn(
        "testapi/main",
        "observe",
        HostFnBodyKind::TraceUnit { text: "observe" },
    ),
    host_fn(
        "testapi/main",
        "consume",
        HostFnBodyKind::TraceUnit { text: "consume" },
    ),
];
const FNS_HOST_STAGED_UNIT_CALL: &[HostFnBinding] = &[host_fn(
    "testapi/main",
    "staged",
    HostFnBodyKind::StagedUnitCall,
)];
const FNS_HOST_EXISTENTIAL: &[HostFnBinding] = &[host_fn(
    "testapi/arith",
    "observe",
    HostFnBodyKind::ObservePacked {
        i32: TESTAPI_I32_ROLE,
    },
)];
const FNS_FACADE_SELECTOR_COLLISIONS: &[HostFnBinding] = &[
    host_fn(
        "testapi/foo/bar",
        "read",
        HostFnBodyKind::arithmetic("add", SELECTOR_FOO_BAR_I32_ROLE),
    ),
    host_fn(
        "testapi/foo_bar",
        "read",
        HostFnBodyKind::arithmetic("mul", SELECTOR_FOO_UBAR_I32_ROLE),
    ),
    host_fn(
        "testapi/i",
        "read",
        HostFnBodyKind::arithmetic("sub", SELECTOR_I_I32_ROLE),
    ),
];
const FNS_EXPORT_CALLBACK: &[HostFnBinding] = &[host_fn(
    "testapi/arith",
    "add_i32",
    HostFnBodyKind::arithmetic("add", TESTAPI_I32_ROLE),
)];
const FNS_EXPORT_STRUCTURAL: &[HostFnBinding] = &[host_fn(
    "testapi/fmt",
    "int_to_string",
    HostFnBodyKind::numeric_to_string(TESTAPI_I32_ROLE, TESTAPI_STRING_ROLE),
)];
const FNS_COEXIST: &[HostFnBinding] = &[host_fn(
    "greeter",
    "print",
    HostFnBodyKind::print(GREETER_STRING_ROLE),
)];

// This one table produces both the test registry and an exhaustive match over
// `RunnerProtocol`. Adding an enum variant without adding it here therefore
// fails every test build instead of silently omitting that protocol from the
// registry invariants.
macro_rules! complete_runner_protocol_registry {
    ($($variant:path),+ $(,)?) => {
        /// Every accepted protocol, in canonical registry order, for registry
        /// invariants exercised by this module's tests.
        #[cfg(test)]
        pub const ALL: &'static [Self] = &[$($variant),+];

        #[cfg(test)]
        fn assert_registry_complete(self) {
            match self {
                $($variant => ()),+
            }
        }
    };
}

impl RunnerProtocol {
    complete_runner_protocol_registry!(
        Self::Empty,
        Self::CompileOnly,
        Self::ConstructOnly,
        Self::SameLeafHostLiteralRoles,
        Self::MainBoxFixture,
        Self::EmptyApiMain,
        Self::GeneratedCoreMain,
        Self::GeneratedSurfaceMain,
        Self::ExportNamespaceRoundtrip,
        Self::ExportCallbackRoundtrip,
        Self::ExportModuleRoundtrip,
        Self::ExportMultilabelRoundtrip,
        Self::ExportPolyRoundtrip,
        Self::ExportPolyCallbackRoundtrip,
        Self::ExportStructuralRoundtrip,
        Self::ExportScalarRoundtrip,
        Self::ExportHostOwnedRoundtrip,
        Self::ExportCallableSlotsRoundtrip,
        Self::RustCallbackAliases,
        Self::ExportFunctorDictRoundtrip,
        Self::HostExistentialRoundtrip,
        Self::ExportPositionalProductRoundtrip,
        Self::ExportTypeRoundtrip,
        Self::ExportCurriedFacade,
        Self::ExportWideCallable,
        Self::ExportNewtypeSumRoundtrip,
        Self::ExportNewtypeScalarRoundtrip,
        Self::ExportNewtypeIgnoredArgumentRoundtrip,
        Self::RecursiveNewtypeBoundary,
        Self::NewtypeVisibilityFacade,
        Self::ExportNestedProductRoundtrip,
        Self::ExportCompoundInputOnce,
        Self::HostCallbackReturnRoundtrip,
        Self::HostCallbackRoundtrip,
        Self::NestedCurriedRoundtrip,
        Self::HostSubstitutedUnitCallback,
        Self::ReturnedForallCallByValue,
        Self::HostStagedUnitCall,
        Self::FacadeSelectorCollisions,
        Self::PublicWordNames,
        Self::ModuleAliasScopeCollision,
        Self::HostGenericReturnOnlyRoundtrip,
        Self::HostGenericTypeRoundtrip,
        Self::HostRanknRoundtrip,
        Self::HostStructuralRoundtrip,
        Self::HostStructuralElabRoundtrip,
        Self::HostTypeRoundtrip,
        Self::HostFunctorDictRoundtrip,
        Self::HostPolyFunctionNewtypeRoundtrip,
        Self::HostPolyUnitPayloadRoundtrip,
        Self::HostInterleavedStageRoundtrip,
        Self::Elab,
        Self::RootScopedHostEnv,
        Self::TestApiPrint,
        Self::TestApiPrintMarkedString,
        Self::TestApiBareCollection,
        Self::TestApiArray,
        Self::TestApiArrayClear,
        Self::TestApiBigint,
        Self::TestApiText,
        Self::TestApiCompute,
        Self::TestApiArithCollection,
        Self::TestApiFmt,
        Self::TestApiArith,
        Self::TestApiBareArith,
        Self::TestApiBareCompute,
        Self::TestApiIo,
        Self::TestApiFloat,
        Self::TestApiDynLoad,
        Self::TestApiArithAddI32,
        Self::TestApiArithAddInt,
        Self::TestApiArithCollectionElab,
        Self::TestApiArithCollectionElabNoBool,
        Self::TestApiArithCollectionCompositeElab,
        Self::TestApiArithCollectionDictElab,
        Self::TestApiArithCollectionListElab,
        Self::TestApiArithCollectionOpticsElab,
        Self::TestApiArithCollectionQueueElab,
        Self::TestApiArrayRoot,
        Self::TestApiArrayElabNoCloneSwap,
        Self::TestApiArrayElabPushOnly,
        Self::TestApiArrayElabFixed,
        Self::TestApiArrayElabStack,
        Self::TestApiArrayGetFilled,
        Self::TestApiBareArithElabI32,
        Self::TestApiBareArithBoolI32,
        Self::TestApiBareCollectionElabReduced,
        Self::TestApiBareComputeElabI32Reduced,
        Self::TestApiBigintU128,
        Self::TestApiComputeRoot,
        Self::TestApiComputeLoop,
        Self::TestApiComputeElabNoI32Format,
        Self::TestApiComputeElabNoInputParse,
        Self::TestApiComputeListElab,
        Self::TestApiComputeDiff,
        Self::TestApiComputeElabInt,
        Self::TestApiComputeRecBinder,
        Self::TestApiComputeRecOrder,
        Self::TestApiComputeRecPartialLet,
        Self::TestApiComputeIntStr,
        Self::TestApiComputeSubPrint,
        Self::TestApiFloatF32F64,
        Self::TestApiFmtRootI32,
        Self::TestApiFmtElabInt,
        Self::TestApiFmtRootInt,
        Self::TestApiFmtIntOnly,
        Self::TestApiPrintElabString,
        Self::TestApiPrintElabCore,
        Self::TestApiPrintElabI32,
        Self::TestApiPrintBoolString,
        Self::TestApiPrintElabI32Int,
        Self::TestApiPrintElabBoolString,
        Self::TestApiPrintLogicBool,
        Self::TestApiPrintStr,
        Self::TestApiTextElabI32,
        Self::TestApiTextElabInt,
        Self::TestApiTextRootInt,
        Self::TestApiTextBoolInt,
        Self::TestApiTextConcat,
        Self::Coexist,
    );

    /// The canonical command-line spelling for this protocol.
    #[allow(dead_code)] // Consumed by tests and the dyn-load-prime runner.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Empty => EMPTY_PROTOCOL_NAME,
            Self::CompileOnly => COMPILE_ONLY_PROTOCOL_NAME,
            Self::ConstructOnly => CONSTRUCT_ONLY_PROTOCOL_NAME,
            Self::SameLeafHostLiteralRoles => SAME_LEAF_HOST_LITERAL_PROTOCOL_NAME,
            Self::MainBoxFixture => MAIN_BOX_FIXTURE_PROTOCOL_NAME,
            Self::EmptyApiMain => EMPTY_API_MAIN_PROTOCOL_NAME,
            Self::GeneratedCoreMain => GENERATED_CORE_MAIN_PROTOCOL_NAME,
            Self::GeneratedSurfaceMain => GENERATED_SURFACE_MAIN_PROTOCOL_NAME,
            Self::ExportNamespaceRoundtrip => EXPORT_NAMESPACE_ROUNDTRIP_PROTOCOL_NAME,
            Self::ExportCallbackRoundtrip => EXPORT_CALLBACK_ROUNDTRIP_PROTOCOL_NAME,
            Self::ExportModuleRoundtrip => EXPORT_MODULE_ROUNDTRIP_PROTOCOL_NAME,
            Self::ExportMultilabelRoundtrip => EXPORT_MULTILABEL_ROUNDTRIP_PROTOCOL_NAME,
            Self::ExportPolyRoundtrip => EXPORT_POLY_ROUNDTRIP_PROTOCOL_NAME,
            Self::ExportPolyCallbackRoundtrip => EXPORT_POLY_CALLBACK_ROUNDTRIP_PROTOCOL_NAME,
            Self::ExportStructuralRoundtrip => EXPORT_STRUCTURAL_ROUNDTRIP_PROTOCOL_NAME,
            Self::ExportScalarRoundtrip => EXPORT_SCALAR_ROUNDTRIP_PROTOCOL_NAME,
            Self::ExportHostOwnedRoundtrip => EXPORT_HOST_OWNED_ROUNDTRIP_PROTOCOL_NAME,
            Self::ExportCallableSlotsRoundtrip => EXPORT_CALLABLE_SLOTS_ROUNDTRIP_PROTOCOL_NAME,
            Self::RustCallbackAliases => RUST_CALLBACK_ALIASES_PROTOCOL_NAME,
            Self::ExportFunctorDictRoundtrip => EXPORT_FUNCTOR_DICT_ROUNDTRIP_PROTOCOL_NAME,
            Self::HostExistentialRoundtrip => HOST_EXISTENTIAL_ROUNDTRIP_PROTOCOL_NAME,
            Self::ExportPositionalProductRoundtrip => {
                EXPORT_POSITIONAL_PRODUCT_ROUNDTRIP_PROTOCOL_NAME
            }
            Self::ExportTypeRoundtrip => EXPORT_TYPE_ROUNDTRIP_PROTOCOL_NAME,
            Self::ExportCurriedFacade => EXPORT_CURRIED_FACADE_PROTOCOL_NAME,
            Self::ExportWideCallable => EXPORT_WIDE_CALLABLE_PROTOCOL_NAME,
            Self::ExportNewtypeSumRoundtrip => EXPORT_NEWTYPE_SUM_ROUNDTRIP_PROTOCOL_NAME,
            Self::ExportNewtypeScalarRoundtrip => EXPORT_NEWTYPE_SCALAR_ROUNDTRIP_PROTOCOL_NAME,
            Self::ExportNewtypeIgnoredArgumentRoundtrip => {
                EXPORT_NEWTYPE_IGNORED_ARGUMENT_ROUNDTRIP_PROTOCOL_NAME
            }
            Self::RecursiveNewtypeBoundary => RECURSIVE_NEWTYPE_BOUNDARY_PROTOCOL_NAME,
            Self::NewtypeVisibilityFacade => NEWTYPE_VISIBILITY_FACADE_PROTOCOL_NAME,
            Self::ExportNestedProductRoundtrip => EXPORT_NESTED_PRODUCT_ROUNDTRIP_PROTOCOL_NAME,
            Self::ExportCompoundInputOnce => EXPORT_COMPOUND_INPUT_ONCE_PROTOCOL_NAME,
            Self::HostCallbackReturnRoundtrip => HOST_CALLBACK_RETURN_ROUNDTRIP_PROTOCOL_NAME,
            Self::HostCallbackRoundtrip => HOST_CALLBACK_ROUNDTRIP_PROTOCOL_NAME,
            Self::NestedCurriedRoundtrip => NESTED_CURRIED_ROUNDTRIP_PROTOCOL_NAME,
            Self::HostSubstitutedUnitCallback => HOST_SUBSTITUTED_UNIT_CALLBACK_PROTOCOL_NAME,
            Self::ReturnedForallCallByValue => RETURNED_FORALL_CALL_BY_VALUE_PROTOCOL_NAME,
            Self::HostStagedUnitCall => HOST_STAGED_UNIT_CALL_PROTOCOL_NAME,
            Self::FacadeSelectorCollisions => FACADE_SELECTOR_COLLISIONS_PROTOCOL_NAME,
            Self::PublicWordNames => PUBLIC_WORD_NAMES_PROTOCOL_NAME,
            Self::ModuleAliasScopeCollision => MODULE_ALIAS_SCOPE_COLLISION_PROTOCOL_NAME,
            Self::HostGenericReturnOnlyRoundtrip => {
                HOST_GENERIC_RETURN_ONLY_ROUNDTRIP_PROTOCOL_NAME
            }
            Self::HostGenericTypeRoundtrip => HOST_GENERIC_TYPE_ROUNDTRIP_PROTOCOL_NAME,
            Self::HostRanknRoundtrip => HOST_RANKN_ROUNDTRIP_PROTOCOL_NAME,
            Self::HostStructuralRoundtrip => HOST_STRUCTURAL_ROUNDTRIP_PROTOCOL_NAME,
            Self::HostStructuralElabRoundtrip => HOST_STRUCTURAL_ELAB_ROUNDTRIP_PROTOCOL_NAME,
            Self::HostTypeRoundtrip => HOST_TYPE_ROUNDTRIP_PROTOCOL_NAME,
            Self::HostFunctorDictRoundtrip => HOST_FUNCTOR_DICT_ROUNDTRIP_PROTOCOL_NAME,
            Self::HostPolyFunctionNewtypeRoundtrip => {
                HOST_POLY_FUNCTION_NEWTYPE_ROUNDTRIP_PROTOCOL_NAME
            }
            Self::HostPolyUnitPayloadRoundtrip => HOST_POLY_UNIT_PAYLOAD_ROUNDTRIP_PROTOCOL_NAME,
            Self::HostInterleavedStageRoundtrip => HOST_INTERLEAVED_STAGE_ROUNDTRIP_PROTOCOL_NAME,
            Self::Elab => ELAB_PROTOCOL_NAME,
            Self::RootScopedHostEnv => ROOT_SCOPED_HOST_ENV_PROTOCOL_NAME,
            Self::TestApiPrint => TESTAPI_PRINT_PROTOCOL_NAME,
            Self::TestApiPrintMarkedString => TESTAPI_PRINT_MARKED_STRING_PROTOCOL_NAME,
            Self::TestApiBareCollection => TESTAPI_BARE_COLLECTION_PROTOCOL_NAME,
            Self::TestApiArray => TESTAPI_ARRAY_PROTOCOL_NAME,
            Self::TestApiArrayClear => TESTAPI_ARRAY_CLEAR_PROTOCOL_NAME,
            Self::TestApiBigint => TESTAPI_BIGINT_PROTOCOL_NAME,
            Self::TestApiText => TESTAPI_TEXT_PROTOCOL_NAME,
            Self::TestApiCompute => TESTAPI_COMPUTE_PROTOCOL_NAME,
            Self::TestApiArithCollection => TESTAPI_ARITH_COLLECTION_PROTOCOL_NAME,
            Self::TestApiFmt => TESTAPI_FMT_PROTOCOL_NAME,
            Self::TestApiArith => TESTAPI_ARITH_PROTOCOL_NAME,
            Self::TestApiBareArith => TESTAPI_BARE_ARITH_PROTOCOL_NAME,
            Self::TestApiBareCompute => TESTAPI_BARE_COMPUTE_PROTOCOL_NAME,
            Self::TestApiIo => TESTAPI_IO_PROTOCOL_NAME,
            Self::TestApiFloat => TESTAPI_FLOAT_PROTOCOL_NAME,
            Self::TestApiDynLoad => TESTAPI_DYN_LOAD_PROTOCOL_NAME,
            Self::TestApiArithAddI32 => TESTAPI_ARITH_ADD_I32_PROTOCOL_NAME,
            Self::TestApiArithAddInt => TESTAPI_ARITH_ADD_INT_PROTOCOL_NAME,
            Self::TestApiArithCollectionElab => TESTAPI_ARITH_COLLECTION_ELAB_PROTOCOL_NAME,
            Self::TestApiArithCollectionElabNoBool => {
                TESTAPI_ARITH_COLLECTION_ELAB_NO_BOOL_PROTOCOL_NAME
            }
            Self::TestApiArithCollectionCompositeElab => {
                TESTAPI_ARITH_COLLECTION_COMPOSITE_ELAB_PROTOCOL_NAME
            }
            Self::TestApiArithCollectionDictElab => {
                TESTAPI_ARITH_COLLECTION_DICT_ELAB_PROTOCOL_NAME
            }
            Self::TestApiArithCollectionListElab => {
                TESTAPI_ARITH_COLLECTION_LIST_ELAB_PROTOCOL_NAME
            }
            Self::TestApiArithCollectionOpticsElab => {
                TESTAPI_ARITH_COLLECTION_OPTICS_ELAB_PROTOCOL_NAME
            }
            Self::TestApiArithCollectionQueueElab => {
                TESTAPI_ARITH_COLLECTION_QUEUE_ELAB_PROTOCOL_NAME
            }
            Self::TestApiArrayRoot => TESTAPI_ARRAY_ROOT_PROTOCOL_NAME,
            Self::TestApiArrayElabNoCloneSwap => TESTAPI_ARRAY_ELAB_NO_CLONE_SWAP_PROTOCOL_NAME,
            Self::TestApiArrayElabPushOnly => TESTAPI_ARRAY_ELAB_PUSH_ONLY_PROTOCOL_NAME,
            Self::TestApiArrayElabFixed => TESTAPI_ARRAY_ELAB_FIXED_PROTOCOL_NAME,
            Self::TestApiArrayElabStack => TESTAPI_ARRAY_ELAB_STACK_PROTOCOL_NAME,
            Self::TestApiArrayGetFilled => TESTAPI_ARRAY_GET_FILLED_PROTOCOL_NAME,
            Self::TestApiBareArithElabI32 => TESTAPI_BARE_ARITH_ELAB_I32_PROTOCOL_NAME,
            Self::TestApiBareArithBoolI32 => TESTAPI_BARE_ARITH_BOOL_I32_PROTOCOL_NAME,
            Self::TestApiBareCollectionElabReduced => {
                TESTAPI_BARE_COLLECTION_ELAB_REDUCED_PROTOCOL_NAME
            }
            Self::TestApiBareComputeElabI32Reduced => {
                TESTAPI_BARE_COMPUTE_ELAB_I32_REDUCED_PROTOCOL_NAME
            }
            Self::TestApiBigintU128 => TESTAPI_BIGINT_U128_PROTOCOL_NAME,
            Self::TestApiComputeRoot => TESTAPI_COMPUTE_ROOT_PROTOCOL_NAME,
            Self::TestApiComputeLoop => TESTAPI_COMPUTE_LOOP_PROTOCOL_NAME,
            Self::TestApiComputeElabNoI32Format => TESTAPI_COMPUTE_ELAB_NO_I32_FORMAT_PROTOCOL_NAME,
            Self::TestApiComputeElabNoInputParse => {
                TESTAPI_COMPUTE_ELAB_NO_INPUT_PARSE_PROTOCOL_NAME
            }
            Self::TestApiComputeListElab => TESTAPI_COMPUTE_LIST_ELAB_PROTOCOL_NAME,
            Self::TestApiComputeDiff => TESTAPI_COMPUTE_DIFF_PROTOCOL_NAME,
            Self::TestApiComputeElabInt => TESTAPI_COMPUTE_ELAB_INT_PROTOCOL_NAME,
            Self::TestApiComputeRecBinder => TESTAPI_COMPUTE_REC_BINDER_PROTOCOL_NAME,
            Self::TestApiComputeRecOrder => TESTAPI_COMPUTE_REC_ORDER_PROTOCOL_NAME,
            Self::TestApiComputeRecPartialLet => TESTAPI_COMPUTE_REC_PARTIAL_LET_PROTOCOL_NAME,
            Self::TestApiComputeIntStr => TESTAPI_COMPUTE_INT_STR_PROTOCOL_NAME,
            Self::TestApiComputeSubPrint => TESTAPI_COMPUTE_SUB_PRINT_PROTOCOL_NAME,
            Self::TestApiFloatF32F64 => TESTAPI_FLOAT_F32_F64_PROTOCOL_NAME,
            Self::TestApiFmtRootI32 => TESTAPI_FMT_ROOT_I32_PROTOCOL_NAME,
            Self::TestApiFmtElabInt => TESTAPI_FMT_ELAB_INT_PROTOCOL_NAME,
            Self::TestApiFmtRootInt => TESTAPI_FMT_ROOT_INT_PROTOCOL_NAME,
            Self::TestApiFmtIntOnly => TESTAPI_FMT_INT_ONLY_PROTOCOL_NAME,
            Self::TestApiPrintElabString => TESTAPI_PRINT_ELAB_STRING_PROTOCOL_NAME,
            Self::TestApiPrintElabCore => TESTAPI_PRINT_ELAB_CORE_PROTOCOL_NAME,
            Self::TestApiPrintElabI32 => TESTAPI_PRINT_ELAB_I32_PROTOCOL_NAME,
            Self::TestApiPrintBoolString => TESTAPI_PRINT_BOOL_STRING_PROTOCOL_NAME,
            Self::TestApiPrintElabI32Int => TESTAPI_PRINT_ELAB_I32_INT_PROTOCOL_NAME,
            Self::TestApiPrintElabBoolString => TESTAPI_PRINT_ELAB_BOOL_STRING_PROTOCOL_NAME,
            Self::TestApiPrintLogicBool => TESTAPI_PRINT_LOGIC_BOOL_PROTOCOL_NAME,
            Self::TestApiPrintStr => TESTAPI_PRINT_STR_PROTOCOL_NAME,
            Self::TestApiTextElabI32 => TESTAPI_TEXT_ELAB_I32_PROTOCOL_NAME,
            Self::TestApiTextElabInt => TESTAPI_TEXT_ELAB_INT_PROTOCOL_NAME,
            Self::TestApiTextRootInt => TESTAPI_TEXT_ROOT_INT_PROTOCOL_NAME,
            Self::TestApiTextBoolInt => TESTAPI_TEXT_BOOL_INT_PROTOCOL_NAME,
            Self::TestApiTextConcat => TESTAPI_TEXT_CONCAT_PROTOCOL_NAME,
            Self::Coexist => COEXIST_PROTOCOL_NAME,
        }
    }

    /// Whether the dyn-load-prime runner has a complete execution adapter for
    /// this protocol's whole contract.
    #[allow(dead_code)] // Consumed only by the dyn-load-prime runner.
    pub fn supports_dyn_load_prime(self) -> bool {
        !DYN_LOAD_PRIME_UNSUPPORTED_PROTOCOL_NAMES.contains(&self.name())
    }

    pub fn parse(name: &str) -> Result<Self, String> {
        match name {
            EMPTY_PROTOCOL_NAME => Ok(Self::Empty),
            COMPILE_ONLY_PROTOCOL_NAME => Ok(Self::CompileOnly),
            CONSTRUCT_ONLY_PROTOCOL_NAME => Ok(Self::ConstructOnly),
            SAME_LEAF_HOST_LITERAL_PROTOCOL_NAME => Ok(Self::SameLeafHostLiteralRoles),
            MAIN_BOX_FIXTURE_PROTOCOL_NAME => Ok(Self::MainBoxFixture),
            EMPTY_API_MAIN_PROTOCOL_NAME => Ok(Self::EmptyApiMain),
            GENERATED_CORE_MAIN_PROTOCOL_NAME => Ok(Self::GeneratedCoreMain),
            GENERATED_SURFACE_MAIN_PROTOCOL_NAME => Ok(Self::GeneratedSurfaceMain),
            EXPORT_NAMESPACE_ROUNDTRIP_PROTOCOL_NAME => Ok(Self::ExportNamespaceRoundtrip),
            EXPORT_CALLBACK_ROUNDTRIP_PROTOCOL_NAME => Ok(Self::ExportCallbackRoundtrip),
            EXPORT_MODULE_ROUNDTRIP_PROTOCOL_NAME => Ok(Self::ExportModuleRoundtrip),
            EXPORT_MULTILABEL_ROUNDTRIP_PROTOCOL_NAME => Ok(Self::ExportMultilabelRoundtrip),
            EXPORT_POLY_ROUNDTRIP_PROTOCOL_NAME => Ok(Self::ExportPolyRoundtrip),
            EXPORT_POLY_CALLBACK_ROUNDTRIP_PROTOCOL_NAME => Ok(Self::ExportPolyCallbackRoundtrip),
            EXPORT_STRUCTURAL_ROUNDTRIP_PROTOCOL_NAME => Ok(Self::ExportStructuralRoundtrip),
            EXPORT_SCALAR_ROUNDTRIP_PROTOCOL_NAME => Ok(Self::ExportScalarRoundtrip),
            EXPORT_HOST_OWNED_ROUNDTRIP_PROTOCOL_NAME => Ok(Self::ExportHostOwnedRoundtrip),
            EXPORT_CALLABLE_SLOTS_ROUNDTRIP_PROTOCOL_NAME => Ok(Self::ExportCallableSlotsRoundtrip),
            RUST_CALLBACK_ALIASES_PROTOCOL_NAME => Ok(Self::RustCallbackAliases),
            EXPORT_FUNCTOR_DICT_ROUNDTRIP_PROTOCOL_NAME => Ok(Self::ExportFunctorDictRoundtrip),
            HOST_EXISTENTIAL_ROUNDTRIP_PROTOCOL_NAME => Ok(Self::HostExistentialRoundtrip),
            EXPORT_POSITIONAL_PRODUCT_ROUNDTRIP_PROTOCOL_NAME => {
                Ok(Self::ExportPositionalProductRoundtrip)
            }
            EXPORT_TYPE_ROUNDTRIP_PROTOCOL_NAME => Ok(Self::ExportTypeRoundtrip),
            EXPORT_CURRIED_FACADE_PROTOCOL_NAME => Ok(Self::ExportCurriedFacade),
            EXPORT_WIDE_CALLABLE_PROTOCOL_NAME => Ok(Self::ExportWideCallable),
            EXPORT_NEWTYPE_SUM_ROUNDTRIP_PROTOCOL_NAME => Ok(Self::ExportNewtypeSumRoundtrip),
            EXPORT_NEWTYPE_SCALAR_ROUNDTRIP_PROTOCOL_NAME => Ok(Self::ExportNewtypeScalarRoundtrip),
            EXPORT_NEWTYPE_IGNORED_ARGUMENT_ROUNDTRIP_PROTOCOL_NAME => {
                Ok(Self::ExportNewtypeIgnoredArgumentRoundtrip)
            }
            RECURSIVE_NEWTYPE_BOUNDARY_PROTOCOL_NAME => Ok(Self::RecursiveNewtypeBoundary),
            NEWTYPE_VISIBILITY_FACADE_PROTOCOL_NAME => Ok(Self::NewtypeVisibilityFacade),
            EXPORT_NESTED_PRODUCT_ROUNDTRIP_PROTOCOL_NAME => Ok(Self::ExportNestedProductRoundtrip),
            EXPORT_COMPOUND_INPUT_ONCE_PROTOCOL_NAME => Ok(Self::ExportCompoundInputOnce),
            HOST_CALLBACK_RETURN_ROUNDTRIP_PROTOCOL_NAME => Ok(Self::HostCallbackReturnRoundtrip),
            HOST_CALLBACK_ROUNDTRIP_PROTOCOL_NAME => Ok(Self::HostCallbackRoundtrip),
            NESTED_CURRIED_ROUNDTRIP_PROTOCOL_NAME => Ok(Self::NestedCurriedRoundtrip),
            HOST_SUBSTITUTED_UNIT_CALLBACK_PROTOCOL_NAME => Ok(Self::HostSubstitutedUnitCallback),
            RETURNED_FORALL_CALL_BY_VALUE_PROTOCOL_NAME => Ok(Self::ReturnedForallCallByValue),
            HOST_STAGED_UNIT_CALL_PROTOCOL_NAME => Ok(Self::HostStagedUnitCall),
            FACADE_SELECTOR_COLLISIONS_PROTOCOL_NAME => Ok(Self::FacadeSelectorCollisions),
            PUBLIC_WORD_NAMES_PROTOCOL_NAME => Ok(Self::PublicWordNames),
            MODULE_ALIAS_SCOPE_COLLISION_PROTOCOL_NAME => Ok(Self::ModuleAliasScopeCollision),
            HOST_GENERIC_RETURN_ONLY_ROUNDTRIP_PROTOCOL_NAME => {
                Ok(Self::HostGenericReturnOnlyRoundtrip)
            }
            HOST_GENERIC_TYPE_ROUNDTRIP_PROTOCOL_NAME => Ok(Self::HostGenericTypeRoundtrip),
            HOST_RANKN_ROUNDTRIP_PROTOCOL_NAME => Ok(Self::HostRanknRoundtrip),
            HOST_STRUCTURAL_ROUNDTRIP_PROTOCOL_NAME => Ok(Self::HostStructuralRoundtrip),
            HOST_STRUCTURAL_ELAB_ROUNDTRIP_PROTOCOL_NAME => Ok(Self::HostStructuralElabRoundtrip),
            HOST_TYPE_ROUNDTRIP_PROTOCOL_NAME => Ok(Self::HostTypeRoundtrip),
            HOST_FUNCTOR_DICT_ROUNDTRIP_PROTOCOL_NAME => Ok(Self::HostFunctorDictRoundtrip),
            HOST_POLY_FUNCTION_NEWTYPE_ROUNDTRIP_PROTOCOL_NAME => {
                Ok(Self::HostPolyFunctionNewtypeRoundtrip)
            }
            HOST_POLY_UNIT_PAYLOAD_ROUNDTRIP_PROTOCOL_NAME => {
                Ok(Self::HostPolyUnitPayloadRoundtrip)
            }
            HOST_INTERLEAVED_STAGE_ROUNDTRIP_PROTOCOL_NAME => {
                Ok(Self::HostInterleavedStageRoundtrip)
            }
            ELAB_PROTOCOL_NAME => Ok(Self::Elab),
            ROOT_SCOPED_HOST_ENV_PROTOCOL_NAME => Ok(Self::RootScopedHostEnv),
            TESTAPI_PRINT_PROTOCOL_NAME => Ok(Self::TestApiPrint),
            TESTAPI_PRINT_MARKED_STRING_PROTOCOL_NAME => Ok(Self::TestApiPrintMarkedString),
            TESTAPI_BARE_COLLECTION_PROTOCOL_NAME => Ok(Self::TestApiBareCollection),
            TESTAPI_ARRAY_PROTOCOL_NAME => Ok(Self::TestApiArray),
            TESTAPI_ARRAY_CLEAR_PROTOCOL_NAME => Ok(Self::TestApiArrayClear),
            TESTAPI_BIGINT_PROTOCOL_NAME => Ok(Self::TestApiBigint),
            TESTAPI_TEXT_PROTOCOL_NAME => Ok(Self::TestApiText),
            TESTAPI_COMPUTE_PROTOCOL_NAME => Ok(Self::TestApiCompute),
            TESTAPI_ARITH_COLLECTION_PROTOCOL_NAME => Ok(Self::TestApiArithCollection),
            TESTAPI_FMT_PROTOCOL_NAME => Ok(Self::TestApiFmt),
            TESTAPI_ARITH_PROTOCOL_NAME => Ok(Self::TestApiArith),
            TESTAPI_BARE_ARITH_PROTOCOL_NAME => Ok(Self::TestApiBareArith),
            TESTAPI_BARE_COMPUTE_PROTOCOL_NAME => Ok(Self::TestApiBareCompute),
            TESTAPI_IO_PROTOCOL_NAME => Ok(Self::TestApiIo),
            TESTAPI_FLOAT_PROTOCOL_NAME => Ok(Self::TestApiFloat),
            TESTAPI_DYN_LOAD_PROTOCOL_NAME => Ok(Self::TestApiDynLoad),
            TESTAPI_ARITH_ADD_I32_PROTOCOL_NAME => Ok(Self::TestApiArithAddI32),
            TESTAPI_ARITH_ADD_INT_PROTOCOL_NAME => Ok(Self::TestApiArithAddInt),
            TESTAPI_ARITH_COLLECTION_ELAB_PROTOCOL_NAME => Ok(Self::TestApiArithCollectionElab),
            TESTAPI_ARITH_COLLECTION_ELAB_NO_BOOL_PROTOCOL_NAME => {
                Ok(Self::TestApiArithCollectionElabNoBool)
            }
            TESTAPI_ARITH_COLLECTION_COMPOSITE_ELAB_PROTOCOL_NAME => {
                Ok(Self::TestApiArithCollectionCompositeElab)
            }
            TESTAPI_ARITH_COLLECTION_DICT_ELAB_PROTOCOL_NAME => {
                Ok(Self::TestApiArithCollectionDictElab)
            }
            TESTAPI_ARITH_COLLECTION_LIST_ELAB_PROTOCOL_NAME => {
                Ok(Self::TestApiArithCollectionListElab)
            }
            TESTAPI_ARITH_COLLECTION_OPTICS_ELAB_PROTOCOL_NAME => {
                Ok(Self::TestApiArithCollectionOpticsElab)
            }
            TESTAPI_ARITH_COLLECTION_QUEUE_ELAB_PROTOCOL_NAME => {
                Ok(Self::TestApiArithCollectionQueueElab)
            }
            TESTAPI_ARRAY_ROOT_PROTOCOL_NAME => Ok(Self::TestApiArrayRoot),
            TESTAPI_ARRAY_ELAB_NO_CLONE_SWAP_PROTOCOL_NAME => Ok(Self::TestApiArrayElabNoCloneSwap),
            TESTAPI_ARRAY_ELAB_PUSH_ONLY_PROTOCOL_NAME => Ok(Self::TestApiArrayElabPushOnly),
            TESTAPI_ARRAY_ELAB_FIXED_PROTOCOL_NAME => Ok(Self::TestApiArrayElabFixed),
            TESTAPI_ARRAY_ELAB_STACK_PROTOCOL_NAME => Ok(Self::TestApiArrayElabStack),
            TESTAPI_ARRAY_GET_FILLED_PROTOCOL_NAME => Ok(Self::TestApiArrayGetFilled),
            TESTAPI_BARE_ARITH_ELAB_I32_PROTOCOL_NAME => Ok(Self::TestApiBareArithElabI32),
            TESTAPI_BARE_ARITH_BOOL_I32_PROTOCOL_NAME => Ok(Self::TestApiBareArithBoolI32),
            TESTAPI_BARE_COLLECTION_ELAB_REDUCED_PROTOCOL_NAME => {
                Ok(Self::TestApiBareCollectionElabReduced)
            }
            TESTAPI_BARE_COMPUTE_ELAB_I32_REDUCED_PROTOCOL_NAME => {
                Ok(Self::TestApiBareComputeElabI32Reduced)
            }
            TESTAPI_BIGINT_U128_PROTOCOL_NAME => Ok(Self::TestApiBigintU128),
            TESTAPI_COMPUTE_ROOT_PROTOCOL_NAME => Ok(Self::TestApiComputeRoot),
            TESTAPI_COMPUTE_LOOP_PROTOCOL_NAME => Ok(Self::TestApiComputeLoop),
            TESTAPI_COMPUTE_ELAB_NO_I32_FORMAT_PROTOCOL_NAME => {
                Ok(Self::TestApiComputeElabNoI32Format)
            }
            TESTAPI_COMPUTE_ELAB_NO_INPUT_PARSE_PROTOCOL_NAME => {
                Ok(Self::TestApiComputeElabNoInputParse)
            }
            TESTAPI_COMPUTE_LIST_ELAB_PROTOCOL_NAME => Ok(Self::TestApiComputeListElab),
            TESTAPI_COMPUTE_DIFF_PROTOCOL_NAME => Ok(Self::TestApiComputeDiff),
            TESTAPI_COMPUTE_ELAB_INT_PROTOCOL_NAME => Ok(Self::TestApiComputeElabInt),
            TESTAPI_COMPUTE_REC_BINDER_PROTOCOL_NAME => Ok(Self::TestApiComputeRecBinder),
            TESTAPI_COMPUTE_REC_ORDER_PROTOCOL_NAME => Ok(Self::TestApiComputeRecOrder),
            TESTAPI_COMPUTE_REC_PARTIAL_LET_PROTOCOL_NAME => Ok(Self::TestApiComputeRecPartialLet),
            TESTAPI_COMPUTE_INT_STR_PROTOCOL_NAME => Ok(Self::TestApiComputeIntStr),
            TESTAPI_COMPUTE_SUB_PRINT_PROTOCOL_NAME => Ok(Self::TestApiComputeSubPrint),
            TESTAPI_FLOAT_F32_F64_PROTOCOL_NAME => Ok(Self::TestApiFloatF32F64),
            TESTAPI_FMT_ROOT_I32_PROTOCOL_NAME => Ok(Self::TestApiFmtRootI32),
            TESTAPI_FMT_ELAB_INT_PROTOCOL_NAME => Ok(Self::TestApiFmtElabInt),
            TESTAPI_FMT_ROOT_INT_PROTOCOL_NAME => Ok(Self::TestApiFmtRootInt),
            TESTAPI_FMT_INT_ONLY_PROTOCOL_NAME => Ok(Self::TestApiFmtIntOnly),
            TESTAPI_PRINT_ELAB_STRING_PROTOCOL_NAME => Ok(Self::TestApiPrintElabString),
            TESTAPI_PRINT_ELAB_CORE_PROTOCOL_NAME => Ok(Self::TestApiPrintElabCore),
            TESTAPI_PRINT_ELAB_I32_PROTOCOL_NAME => Ok(Self::TestApiPrintElabI32),
            TESTAPI_PRINT_BOOL_STRING_PROTOCOL_NAME => Ok(Self::TestApiPrintBoolString),
            TESTAPI_PRINT_ELAB_I32_INT_PROTOCOL_NAME => Ok(Self::TestApiPrintElabI32Int),
            TESTAPI_PRINT_ELAB_BOOL_STRING_PROTOCOL_NAME => Ok(Self::TestApiPrintElabBoolString),
            TESTAPI_PRINT_LOGIC_BOOL_PROTOCOL_NAME => Ok(Self::TestApiPrintLogicBool),
            TESTAPI_PRINT_STR_PROTOCOL_NAME => Ok(Self::TestApiPrintStr),
            TESTAPI_TEXT_ELAB_I32_PROTOCOL_NAME => Ok(Self::TestApiTextElabI32),
            TESTAPI_TEXT_ELAB_INT_PROTOCOL_NAME => Ok(Self::TestApiTextElabInt),
            TESTAPI_TEXT_ROOT_INT_PROTOCOL_NAME => Ok(Self::TestApiTextRootInt),
            TESTAPI_TEXT_BOOL_INT_PROTOCOL_NAME => Ok(Self::TestApiTextBoolInt),
            TESTAPI_TEXT_CONCAT_PROTOCOL_NAME => Ok(Self::TestApiTextConcat),
            COEXIST_PROTOCOL_NAME => Ok(Self::Coexist),
            _ => Err(format!("unknown protocol `{name}`")),
        }
    }

    /// Resolve the selected name to its complete semantic contract.
    pub const fn contract(self) -> ProtocolContract {
        match self {
            Self::Empty => invoke_contract(
                ExportDriver::Main { module: "main" },
                NO_HOST_TYPES,
                NO_HOST_FNS,
                false,
            ),
            Self::CompileOnly => ProtocolContract {
                execution: ProtocolExecution::CompileOnly,
                host_types: NO_HOST_TYPES,
                host_fns: NO_HOST_FNS,
                testapi_conformed: false,
            },
            Self::ConstructOnly => construct_contract(NO_HOST_TYPES, NO_HOST_FNS),
            Self::SameLeafHostLiteralRoles => invoke_contract(
                ExportDriver::Main { module: "main" },
                TYPES_SAME_LEAF_ROLES,
                NO_HOST_FNS,
                false,
            ),
            Self::MainBoxFixture => invoke_contract(
                ExportDriver::Main { module: "main" },
                TYPES_MAIN_BOX,
                NO_HOST_FNS,
                false,
            ),
            Self::EmptyApiMain => invoke_contract(
                ExportDriver::Main { module: "api" },
                NO_HOST_TYPES,
                NO_HOST_FNS,
                false,
            ),
            Self::GeneratedCoreMain => invoke_contract(
                ExportDriver::Main { module: "prog" },
                TYPES_GENERATED_CORE,
                NO_HOST_FNS,
                false,
            ),
            Self::GeneratedSurfaceMain => invoke_contract(
                ExportDriver::Main { module: "prog" },
                TYPES_GENERATED_SURFACE,
                NO_HOST_FNS,
                false,
            ),

            Self::ExportNamespaceRoundtrip => invoke_contract(
                ExportDriver::NamespaceRoundtrip,
                TYPES_TESTAPI_I32_STRING,
                NO_HOST_FNS,
                true,
            ),
            Self::ExportCallbackRoundtrip => invoke_contract(
                ExportDriver::CallbackRoundtrip,
                TYPES_TESTAPI_I32,
                FNS_EXPORT_CALLBACK,
                true,
            ),
            Self::ExportModuleRoundtrip => invoke_contract(
                ExportDriver::ModuleRoundtrip,
                TYPES_TESTAPI_I32_STRING,
                NO_HOST_FNS,
                true,
            ),
            Self::ExportMultilabelRoundtrip => invoke_contract(
                ExportDriver::MultilabelRoundtrip,
                TYPES_ELAB_I32_STRING,
                FNS_PRINT_STRING,
                true,
            ),
            Self::ExportPolyRoundtrip => invoke_contract(
                ExportDriver::PolyRoundtrip,
                NO_HOST_TYPES,
                NO_HOST_FNS,
                true,
            ),
            Self::ExportPolyCallbackRoundtrip => invoke_contract(
                ExportDriver::PolyCallbackRoundtrip,
                NO_HOST_TYPES,
                NO_HOST_FNS,
                true,
            ),
            Self::ExportStructuralRoundtrip => invoke_contract(
                ExportDriver::StructuralRoundtrip,
                TYPES_EXPORT_STRUCTURAL,
                FNS_EXPORT_STRUCTURAL,
                true,
            ),
            Self::ExportScalarRoundtrip => invoke_contract(
                ExportDriver::ScalarRoundtrip,
                TYPES_EXPORT_SCALAR,
                NO_HOST_FNS,
                true,
            ),
            Self::ExportHostOwnedRoundtrip => invoke_contract(
                ExportDriver::HostOwnedRoundtrip,
                TYPES_EXPORT_HOST_OWNED,
                NO_HOST_FNS,
                true,
            ),
            Self::ExportCallableSlotsRoundtrip => invoke_contract(
                ExportDriver::CallableSlotsRoundtrip,
                TYPES_EXPORT_CALLABLE_SLOTS,
                NO_HOST_FNS,
                true,
            ),
            Self::RustCallbackAliases => invoke_contract(
                ExportDriver::RustCallbackAliases,
                NO_HOST_TYPES,
                NO_HOST_FNS,
                true,
            ),
            Self::ExportFunctorDictRoundtrip => invoke_contract(
                ExportDriver::FunctorDictRoundtrip,
                TYPES_TESTAPI_I32_STRING,
                NO_HOST_FNS,
                true,
            ),
            Self::HostExistentialRoundtrip => invoke_contract(
                ExportDriver::HostExistentialRoundtrip,
                TYPES_TESTAPI_I32,
                FNS_HOST_EXISTENTIAL,
                true,
            ),
            Self::ExportPositionalProductRoundtrip => invoke_contract(
                ExportDriver::PositionalProductRoundtrip,
                TYPES_MAIN_I32_STRING,
                NO_HOST_FNS,
                false,
            ),
            Self::ExportTypeRoundtrip => invoke_contract(
                ExportDriver::TypeRoundtrip,
                NO_HOST_TYPES,
                NO_HOST_FNS,
                true,
            ),
            Self::ExportCurriedFacade => invoke_contract(
                ExportDriver::CurriedFacade,
                TYPES_TESTAPI_I32_STR,
                NO_HOST_FNS,
                true,
            ),
            Self::ExportWideCallable => invoke_contract(
                ExportDriver::WideCallable,
                TYPES_TESTAPI_I32,
                NO_HOST_FNS,
                true,
            ),
            Self::ExportNewtypeSumRoundtrip => invoke_contract(
                ExportDriver::NewtypeSumRoundtrip,
                TYPES_ELAB_I32_STRING,
                NO_HOST_FNS,
                true,
            ),
            Self::ExportNewtypeScalarRoundtrip => invoke_contract(
                ExportDriver::NewtypeScalarRoundtrip,
                TYPES_TESTAPI_I32,
                NO_HOST_FNS,
                true,
            ),
            Self::ExportNewtypeIgnoredArgumentRoundtrip => invoke_contract(
                ExportDriver::NewtypeIgnoredArgumentRoundtrip,
                TYPES_TESTAPI_I32,
                NO_HOST_FNS,
                true,
            ),
            Self::RecursiveNewtypeBoundary => invoke_contract(
                ExportDriver::RecursiveNewtypeBoundary,
                TYPES_MAIN_I32,
                NO_HOST_FNS,
                false,
            ),
            Self::NewtypeVisibilityFacade => invoke_contract(
                ExportDriver::NewtypeVisibilityFacade,
                TYPES_TESTAPI_SELECTED_I32,
                NO_HOST_FNS,
                true,
            ),
            Self::ExportNestedProductRoundtrip => invoke_contract(
                ExportDriver::NestedProductRoundtrip,
                TYPES_TESTAPI_I32_STRING,
                NO_HOST_FNS,
                true,
            ),
            Self::ExportCompoundInputOnce => invoke_contract(
                ExportDriver::CompoundInputOnce,
                TYPES_ELAB_I32_STRING,
                FNS_COMPOUND_INPUT_ONCE,
                true,
            ),
            Self::HostCallbackReturnRoundtrip => {
                main_contract(TYPES_TESTAPI_I32_STRING, FNS_HOST_CALLBACK_RETURN)
            }
            Self::HostCallbackRoundtrip => main_contract(TYPES_TESTAPI_CORE, FNS_HOST_CALLBACK),
            Self::NestedCurriedRoundtrip => invoke_contract(
                ExportDriver::NestedCurriedRoundtrip,
                TYPES_TESTAPI_STRING,
                FNS_NESTED_CURRIED_ROUNDTRIP,
                true,
            ),
            Self::HostSubstitutedUnitCallback => invoke_contract(
                ExportDriver::HostSubstitutedUnitCallback,
                TYPES_TESTAPI_TEXT,
                FNS_HOST_SUBSTITUTED_UNIT_CALLBACK,
                true,
            ),
            Self::ReturnedForallCallByValue => invoke_contract(
                ExportDriver::ReturnedForallCallByValue,
                NO_HOST_TYPES,
                FNS_RETURNED_FORALL_CALL_BY_VALUE,
                true,
            ),
            Self::HostStagedUnitCall => main_contract(NO_HOST_TYPES, FNS_HOST_STAGED_UNIT_CALL),
            Self::FacadeSelectorCollisions => invoke_contract(
                ExportDriver::FacadeSelectorCollisions,
                TYPES_FACADE_SELECTOR_COLLISIONS,
                FNS_FACADE_SELECTOR_COLLISIONS,
                true,
            ),
            Self::PublicWordNames => invoke_contract(
                ExportDriver::PublicWordNames,
                TYPES_PUBLIC_WORD_NAMES,
                NO_HOST_FNS,
                false,
            ),
            Self::ModuleAliasScopeCollision => invoke_contract(
                ExportDriver::ModuleAliasScopeCollision,
                NO_HOST_TYPES,
                NO_HOST_FNS,
                true,
            ),
            Self::HostGenericReturnOnlyRoundtrip => {
                main_contract(TYPES_HOST_ARRAY_RETURN_ONLY, FNS_HOST_ARRAY_RETURN_ONLY)
            }
            Self::HostGenericTypeRoundtrip => main_contract(TYPES_HOST_BOX, FNS_HOST_BOX),
            Self::HostRanknRoundtrip => main_contract(TYPES_TESTAPI_STRING, FNS_HOST_RANKN),
            Self::HostStructuralRoundtrip => {
                main_contract(TYPES_TESTAPI_I32_STRING, FNS_HOST_STRUCTURAL)
            }
            Self::HostStructuralElabRoundtrip => {
                main_contract(TYPES_ELAB_I32_STRING, FNS_HOST_STRUCTURAL)
            }
            Self::HostTypeRoundtrip => main_contract(TYPES_HOST_TOKEN, FNS_HOST_TOKEN),
            Self::HostFunctorDictRoundtrip => main_contract(TYPES_TESTAPI_STRING, FNS_HOST_FUNCTOR),
            Self::HostPolyFunctionNewtypeRoundtrip => {
                main_contract(TYPES_TESTAPI_STRING, FNS_HOST_POLY_FUNCTION_NEWTYPE)
            }
            Self::HostPolyUnitPayloadRoundtrip => {
                main_contract(TYPES_TESTAPI_STRING, FNS_HOST_POLY_UNIT_PAYLOAD)
            }
            Self::HostInterleavedStageRoundtrip => {
                main_contract(TYPES_TESTAPI_STRING, FNS_HOST_INTERLEAVED_STAGE)
            }
            Self::Elab => invoke_contract(
                ExportDriver::Main {
                    module: "elab/main",
                },
                TYPES_TESTAPI_CORE,
                FNS_ELAB,
                false,
            ),
            Self::RootScopedHostEnv => invoke_contract(
                ExportDriver::Main {
                    module: TESTAPI_MAIN_MODULE,
                },
                TYPES_TESTAPI_I32_STRING,
                FNS_ROOT_SCOPED,
                true,
            ),

            Self::TestApiPrint => main_contract(TYPES_TESTAPI_STRING, FNS_PRINT_STRING),
            Self::TestApiPrintMarkedString => {
                main_contract(TYPES_TESTAPI_MARKED_STRING, FNS_PRINT_MARKED_STRING)
            }
            Self::TestApiBareCollection => {
                main_contract(TYPES_TESTAPI_CORE, FNS_BARE_COLLECTION_I32)
            }
            Self::TestApiArray => main_contract(TYPES_ARRAY_ELAB, FNS_ARRAY_NO_CLEAR),
            Self::TestApiArrayClear => main_contract(TYPES_HOST_ARRAY_RETURN_ONLY, FNS_ARRAY_CLEAR),
            Self::TestApiBigint => main_contract(TYPES_BIGINT, FNS_BIGINT),
            Self::TestApiText => main_contract(TYPES_TESTAPI_CORE, FNS_TEXT_I32),
            Self::TestApiCompute => main_contract(TYPES_ELAB_CORE, FNS_COMPUTE_I32),
            Self::TestApiArithCollection => {
                main_contract(TYPES_TESTAPI_CORE, FNS_ARITH_COLLECTION_I32)
            }
            Self::TestApiFmt => main_contract(TYPES_ELAB_CORE, FNS_FMT_I32),
            Self::TestApiArith => main_contract(TYPES_TESTAPI_I32_STRING, FNS_ARITH_I32),
            Self::TestApiBareArith => main_contract(TYPES_TESTAPI_INT_STRING, FNS_BARE_ARITH_INT),
            Self::TestApiBareCompute => {
                main_contract(TYPES_ELAB_BOOL_INT_STRING, FNS_BARE_COMPUTE_INT)
            }
            Self::TestApiIo => main_contract(TYPES_TESTAPI_I32_STRING, FNS_IO),
            Self::TestApiFloat => main_contract(TYPES_FLOAT_F64, FNS_FLOAT_F64),
            Self::TestApiDynLoad => main_contract(TYPES_DYN_LOAD, FNS_DYN_LOAD),

            Self::TestApiArithAddI32 => main_contract(TYPES_TESTAPI_I32_STRING, FNS_ARITH_ADD_I32),
            Self::TestApiArithAddInt => main_contract(TYPES_TESTAPI_I32_STRING, FNS_ARITH_ADD_INT),
            Self::TestApiArithCollectionElab => {
                main_contract(TYPES_ELAB_CORE, FNS_ARITH_COLLECTION_I32)
            }
            Self::TestApiArithCollectionElabNoBool => {
                main_contract(TYPES_ELAB_CORE, FNS_ARITH_COLLECTION_NO_BOOL)
            }
            Self::TestApiArithCollectionCompositeElab => {
                main_contract(TYPES_ARITH_COLLECTION_COMPOSITE, FNS_ARITH_COLLECTION_I32)
            }
            Self::TestApiArithCollectionDictElab => {
                main_contract(TYPES_ARITH_COLLECTION_DICT, FNS_ARITH_COLLECTION_I32)
            }
            Self::TestApiArithCollectionListElab => {
                main_contract(TYPES_ARITH_COLLECTION_LIST, FNS_ARITH_COLLECTION_I32)
            }
            Self::TestApiArithCollectionOpticsElab => {
                main_contract(TYPES_ARITH_COLLECTION_OPTICS, FNS_ARITH_COLLECTION_I32)
            }
            Self::TestApiArithCollectionQueueElab => {
                main_contract(TYPES_ARITH_COLLECTION_QUEUE, FNS_ARITH_COLLECTION_I32)
            }

            Self::TestApiArrayRoot => main_contract(TYPES_ARRAY_ROOT, FNS_ARRAY_NO_CLEAR),
            Self::TestApiArrayElabNoCloneSwap => {
                main_contract(TYPES_ARRAY_ELAB, FNS_ARRAY_NO_CLONE_SWAP_STRING_EQ)
            }
            Self::TestApiArrayElabPushOnly => main_contract(TYPES_ARRAY_ELAB, FNS_ARRAY_PUSH_ONLY),
            Self::TestApiArrayElabFixed => main_contract(TYPES_ARRAY_ELAB, FNS_ARRAY_FIXED),
            Self::TestApiArrayElabStack => main_contract(TYPES_ARRAY_ELAB, FNS_ARRAY_STACK),
            Self::TestApiArrayGetFilled => main_contract(TYPES_ARRAY_ROOT, FNS_ARRAY_GET_FILLED),

            Self::TestApiBareArithElabI32 => {
                main_contract(TYPES_ELAB_I32_STRING, FNS_BARE_ARITH_I32)
            }
            Self::TestApiBareArithBoolI32 => main_contract(TYPES_TESTAPI_CORE, FNS_BARE_ARITH_I32),
            Self::TestApiBareCollectionElabReduced => {
                main_contract(TYPES_ELAB_CORE, FNS_BARE_COLLECTION_REDUCED)
            }
            Self::TestApiBareComputeElabI32Reduced => {
                main_contract(TYPES_ELAB_CORE, FNS_BARE_COMPUTE_I32_REDUCED)
            }
            Self::TestApiBigintU128 => main_contract(TYPES_BIGINT_U128, FNS_BIGINT_U128),

            Self::TestApiComputeRoot => main_contract(TYPES_TESTAPI_CORE, FNS_COMPUTE_I32),
            Self::TestApiComputeLoop => main_contract(NO_HOST_TYPES, FNS_COMPUTE_LOOP),
            Self::TestApiComputeElabNoI32Format => {
                main_contract(TYPES_ELAB_CORE, FNS_COMPUTE_NO_I32_FORMAT)
            }
            Self::TestApiComputeElabNoInputParse => {
                main_contract(TYPES_ELAB_CORE, FNS_COMPUTE_NO_INPUT_PARSE)
            }
            Self::TestApiComputeListElab => {
                main_contract(TYPES_COMPUTE_LIST_ELAB, FNS_COMPUTE_NO_I32_FORMAT)
            }
            Self::TestApiComputeDiff => main_contract(TYPES_ELAB_CORE, FNS_COMPUTE_DIFF),
            Self::TestApiComputeElabInt => {
                main_contract(TYPES_ELAB_BOOL_INT_STRING, FNS_COMPUTE_INT)
            }
            Self::TestApiComputeRecBinder => {
                main_contract(TYPES_TESTAPI_CORE, FNS_COMPUTE_REC_BINDER)
            }
            Self::TestApiComputeRecOrder => {
                main_contract(TYPES_TESTAPI_CORE, FNS_COMPUTE_REC_ORDER)
            }
            Self::TestApiComputeRecPartialLet => {
                main_contract(TYPES_TESTAPI_CORE, FNS_COMPUTE_REC_PARTIAL_LET)
            }
            Self::TestApiComputeIntStr => {
                main_contract(TYPES_TESTAPI_BOOL_INT_STR, FNS_COMPUTE_INT_STR)
            }
            Self::TestApiComputeSubPrint => {
                main_contract(TYPES_TESTAPI_I32_STRING, FNS_COMPUTE_SUB_PRINT)
            }
            Self::TestApiFloatF32F64 => main_contract(TYPES_FLOAT_F32_F64, FNS_FLOAT_F32_F64),

            Self::TestApiFmtRootI32 => main_contract(TYPES_TESTAPI_CORE, FNS_FMT_I32),
            Self::TestApiFmtElabInt => main_contract(TYPES_ELAB_BOOL_INT_STRING, FNS_FMT_INT),
            Self::TestApiFmtRootInt => main_contract(TYPES_TESTAPI_BOOL_INT_STRING, FNS_FMT_INT),
            Self::TestApiFmtIntOnly => main_contract(TYPES_TESTAPI_INT_STRING, FNS_FMT_INT_ONLY),

            Self::TestApiPrintElabString => main_contract(TYPES_ELAB_STRING, FNS_PRINT_STRING),
            Self::TestApiPrintElabCore => main_contract(TYPES_ELAB_CORE, FNS_PRINT_STRING),
            Self::TestApiPrintElabI32 => main_contract(TYPES_ELAB_I32_STRING, FNS_PRINT_STRING),
            Self::TestApiPrintBoolString => {
                main_contract(TYPES_TESTAPI_BOOL_STRING, FNS_PRINT_STRING)
            }
            Self::TestApiPrintElabI32Int => {
                main_contract(TYPES_ELAB_I32_INT_STRING, FNS_PRINT_STRING)
            }
            Self::TestApiPrintElabBoolString => {
                main_contract(TYPES_ELAB_BOOL_STRING, FNS_PRINT_STRING)
            }
            Self::TestApiPrintLogicBool => main_contract(TYPES_LOGIC_BOOL, FNS_PRINT_STRING),
            Self::TestApiPrintStr => main_contract(TYPES_TESTAPI_STR, FNS_PRINT_STR),

            Self::TestApiTextElabI32 => main_contract(TYPES_ELAB_CORE, FNS_TEXT_I32),
            Self::TestApiTextElabInt => main_contract(TYPES_ELAB_BOOL_I32_INT_STRING, FNS_TEXT_I32),
            Self::TestApiTextRootInt => {
                main_contract(TYPES_TESTAPI_BOOL_I32_INT_STRING, FNS_TEXT_I32)
            }
            Self::TestApiTextBoolInt => main_contract(TYPES_TESTAPI_BOOL_INT_STRING, FNS_TEXT_INT),
            Self::TestApiTextConcat => main_contract(TYPES_TESTAPI_STRING, FNS_TEXT_CONCAT),

            Self::Coexist => {
                invoke_contract(ExportDriver::Coexist, TYPES_COEXIST, FNS_COEXIST, false)
            }
        }
    }

    /// Whether this protocol's golden is **testapi-conformed**: its
    /// whole test-facing surface lives under the fixed [`TESTAPI_ROOT`]
    /// namespace, with every host item's exact declaring module carried
    /// by its binding, and exported `main` reached through the backend's
    /// published facade selectors. The conformed set is the exact
    /// testapi main protocols plus the bespoke FFI-surface roundtrips
    /// rooted under `testapi`.
    // This shared source is path-included by every runner binary. Under
    // `--all-features`, adapter-specific projections are also compiled in
    // bins that do not consume them.
    #[allow(dead_code)]
    pub fn is_testapi(self) -> bool {
        self.contract().testapi_conformed
    }

    /// The package namespace root used by bespoke non-main export drivers.
    ///
    /// Main drivers consume their exact declaring module from
    /// [`ExportDriver::Main`] and must not reconstruct it from this root.
    #[allow(dead_code)]
    pub fn export_root(self) -> Option<String> {
        match self.contract().execution {
            ProtocolExecution::Invoke(ExportDriver::Main { module }) => {
                module.strip_suffix("/main").map(str::to_owned)
            }
            _ => self.is_testapi().then(|| TESTAPI_ROOT.to_owned()),
        }
    }

    /// The exact module that declares the main leaf for a main protocol.
    #[allow(dead_code)]
    pub const fn main_module(self) -> Option<&'static str> {
        match self.contract().execution {
            ProtocolExecution::Invoke(ExportDriver::Main { module }) => Some(module),
            _ => None,
        }
    }
}

/// Spell one protocol-owned canonical host fn's trait method the way the
/// emitted Rust trait spells it. Shaped slots reference the `ffi` aliases of
/// the emitted crate; the crate-name prefix is rewritten by the runner when it
/// drops the signature into the driver crate.
///
/// The namespaced host boundary prefixes both the trait method name
/// and the matching `ffi::env` submodule with the declaring module:
/// `print` in module `testapi/io` is `testapi_io__print`, and `loop`
/// in module `testapi/iter` is `testapi_iter__loop` with its sum alias at
/// `crate::ffi::env::testapi_iter__loop::arg0_cbret`. The declaring
/// module is baked into the protocol contract, never read from an emitted
/// file. `host_module` is `None` only for flat-name unit calls, where names
/// stay bare (keyword-escaped where needed, `r#loop`).
///
/// `TraitMethod.name` carries the **emitted** (namespaced) spelling so
/// the synthesized `impl Host` matches the trait. Function behavior and
/// signature shape come only from [`HostFnBinding::body`]; the leaf is used
/// only to spell the member and matching `ffi` namespace.
///
/// `binding` supplies the exact function identity. `host_types` supplies the
/// exact associated-type identities used by Array and Scalar functions; the
/// translator never reconstructs their modules from a capability name.
///
/// Shared between standard exact protocols and bespoke roundtrips that mix
/// these standard functions with custom host functions.
#[cfg(feature = "rust")]
#[allow(dead_code)]
pub fn rust_native_role_type(
    role: HostRoleRef,
    host_types: &[HostTypeBinding],
    borrow_string: bool,
) -> &'static str {
    let binding = role.resolve(host_types);
    match binding.fixture {
        HostTypeFixture::SelectedRole(RoleFixture::I32) => "SelectedI32",
        HostTypeFixture::SelectedRole(RoleFixture::String) if borrow_string => "&SelectedString",
        HostTypeFixture::SelectedRole(RoleFixture::String) => "SelectedString",
        HostTypeFixture::SelectedRole(other) => {
            unreachable!("Rust runner lacks a distinct selected-role fixture for {other:?}")
        }
        HostTypeFixture::Role(RoleFixture::String) if borrow_string => "&String",
        HostTypeFixture::Role(RoleFixture::String) => "String",
        HostTypeFixture::Role(role) => role.role(),
        other => unreachable!("role reference resolved to non-role fixture {other:?}"),
    }
}

#[cfg(feature = "rust")]
#[allow(dead_code)]
pub fn canonical_host_method(
    binding: &HostFnBinding,
    host_types: &[HostTypeBinding],
) -> TraitMethod {
    let host_module = (!binding.module.is_empty()).then_some(binding.module);
    // The emitted trait member name: `<module>__<leaf>`
    // when namespaced, else the keyword-escaped bare leaf.
    let member = match host_module {
        Some(module) => crate::host_api::rust_host_member(module, binding.leaf),
        None => escape_rust_keyword_runner(binding.leaf),
    };
    let m = |args: &[&str], ret: &str| TraitMethod {
        name: member.clone(),
        type_params: Vec::new(),
        arg_types: args.iter().map(|s| (*s).to_owned()).collect(),
        ret_type: ret.to_owned(),
        where_clause: String::new(),
    };
    // The `ffi` alias for the env member of leaf `leaf`, sub-leaf `sub`,
    // in the emitted crate. The env submodule carries the same
    // `<module>__<leaf>` name the trait method does. The runner
    // rewrites the `crate::` prefix to the emitted library crate name
    // when rendering the driver.
    let ffi = |sub: &str| format!("crate::ffi::env::{member}::{sub}");
    let ffi_for_host = |sub: &str| format!("{}<Self>", ffi(sub));
    let assoc_member = |identity: HostTypeIdentity| {
        let type_binding = host_types
            .iter()
            .find(|candidate| {
                candidate.module == identity.module && candidate.leaf == identity.leaf
            })
            .unwrap_or_else(|| {
                unreachable!(
                    "protocol host fn `{}` references undeclared host type `{}/{}`",
                    binding.leaf, identity.module, identity.leaf
                )
            });
        if type_binding.module.is_empty() {
            escape_rust_keyword_runner(type_binding.leaf)
        } else {
            crate::host_api::rust_host_member(type_binding.module, type_binding.leaf)
        }
    };
    let qualify_array = |method: TraitMethod| -> TraitMethod {
        let HostFnBodyKind::Array { array, .. } = binding.body else {
            unreachable!("array host method `{}` lacks an Array body", binding.leaf);
        };
        let array_assoc = assoc_member(array);
        let fix = |s: String| s.replace("Self::Array", &format!("Self::{array_assoc}"));
        TraitMethod {
            arg_types: method.arg_types.into_iter().map(fix).collect(),
            ret_type: fix(method.ret_type),
            ..method
        }
    };
    let qualify_scalar = |method: TraitMethod| -> TraitMethod {
        let scalar = match binding.body {
            HostFnBodyKind::MakeScalar { scalar, .. }
            | HostFnBodyKind::ScalarOf { scalar, .. }
            | HostFnBodyKind::ScalarAs { scalar, .. }
            | HostFnBodyKind::ScalarIsTrue { scalar, .. } => scalar,
            _ => unreachable!("scalar host method `{}` lacks a Scalar body", binding.leaf),
        };
        let scalar_assoc = assoc_member(scalar);
        let fix = |s: String| s.replace("Self::Scalar", &format!("Self::{scalar_assoc}"));
        TraitMethod {
            arg_types: method.arg_types.into_iter().map(fix).collect(),
            ret_type: fix(method.ret_type),
            ..method
        }
    };
    let native_role = |role, borrow_string| rust_native_role_type(role, host_types, borrow_string);

    match binding.body {
        HostFnBodyKind::Print { string } => m(&[native_role(string, true)], "()"),
        HostFnBodyKind::Eprint { string } => m(&[native_role(string, true)], "()"),
        // The emitter renders a `-> !` host fn as
        // `-> ::std::convert::Infallible` (the Rust trait method's
        // never-type stand-in); the runner's body is a `process::exit`
        // whose `!` coerces to it.
        HostFnBodyKind::Exit { status_i32 } => m(
            &[native_role(status_i32, false)],
            "::std::convert::Infallible",
        ),
        HostFnBodyKind::PrintI32 { value } => m(&[native_role(value, false)], "()"),
        HostFnBodyKind::NumericToString { value, string } => {
            m(&[native_role(value, false)], native_role(string, false))
        }
        HostFnBodyKind::BoolToString { bool_, string } => {
            m(&[native_role(bool_, false)], native_role(string, false))
        }
        HostFnBodyKind::StringConcat { string } => m(
            &[native_role(string, true), native_role(string, true)],
            native_role(string, false),
        ),
        HostFnBodyKind::StringEq { string, bool_ } => m(
            &[native_role(string, true), native_role(string, true)],
            native_role(bool_, false),
        ),
        HostFnBodyKind::StringLen { string, index } => {
            m(&[native_role(string, true)], native_role(index, false))
        }
        HostFnBodyKind::StringSlice { string, index } => m(
            &[
                native_role(string, true),
                native_role(index, false),
                native_role(index, false),
            ],
            native_role(string, false),
        ),
        HostFnBodyKind::StringCodeAt { string, index } => TraitMethod {
            ret_type: ffi_for_host("ret"),
            ..m(&[native_role(string, true), native_role(index, false)], "")
        },
        HostFnBodyKind::StringToInt { string, .. } => TraitMethod {
            ret_type: ffi_for_host("ret"),
            ..m(&[native_role(string, true)], "")
        },
        HostFnBodyKind::ReadAsciiLine { .. } => TraitMethod {
            ret_type: ffi_for_host("ret"),
            ..m(&[], "")
        },
        // `dyn_load_prime`'s opaque-scalar surface. `make_scalar` / `scalar_of_*`
        // build the `Self::Scalar` box; `scalar_as_*` project a `. | <Kind>`
        // sum (named via the ffi `ret` alias); `scalar_is_true` tests a bool.
        HostFnBodyKind::MakeScalar { string, .. } => {
            let string = native_role(string, true);
            qualify_scalar(m(&[string, string], "Self::Scalar"))
        }
        HostFnBodyKind::ScalarOf { value, .. } => {
            let borrow_string = value.fixture == RoleFixture::String;
            qualify_scalar(m(&[native_role(value, borrow_string)], "Self::Scalar"))
        }
        HostFnBodyKind::ScalarAs { .. } => qualify_scalar(TraitMethod {
            ret_type: ffi_for_host("ret"),
            ..m(&["Self::Scalar"], "")
        }),
        HostFnBodyKind::ScalarIsTrue { bool_, .. } => {
            qualify_scalar(m(&["Self::Scalar"], native_role(bool_, false)))
        }
        HostFnBodyKind::Arithmetic { number, .. } => {
            let ty = native_role(number, false);
            m(&[ty, ty], ty)
        }
        HostFnBodyKind::FloatArithmetic { number, .. } => {
            let ty = native_role(number, false);
            m(&[ty, ty], ty)
        }
        HostFnBodyKind::Compare { number, bool_, .. } => {
            let ty = native_role(number, false);
            m(&[ty, ty], native_role(bool_, false))
        }
        HostFnBodyKind::Loop => {
            // `fn loop[S][R](step: S -> (S | R), state: S) -> R;`
            // The Rust trait emits a synthesized `__F0` fn-typed
            // tparam for the step closure; the step's return shape is
            // a `S | R` sum, named via the ffi alias for the step
            // param's callback return (`arg0_cbret`).
            // The alias is `arg0_cbret<S, R> = Sum_<hash><S, R>`, so the
            // step's `S | R` return — `Sum<s, r>` with this method's
            // lowercase `s` / `r` params — is `arg0_cbret<s, r>`. Under
            // the namespaced boundary the env submodule is
            // `<module>__loop` (the module prefix already makes the name
            // keyword-safe, so no `r#` escape).
            let cbret = ffi("arg0_cbret");
            TraitMethod {
                name: member,
                type_params: vec!["s".to_owned(), "r".to_owned(), "__F0".to_owned()],
                arg_types: vec!["__F0".to_owned(), "s".to_owned()],
                ret_type: "r".to_owned(),
                where_clause: format!("__F0: Fn(s) -> {cbret}<s, r>"),
            }
        }
        HostFnBodyKind::Array {
            operation, index, ..
        } => {
            let index = index.map(|index| native_role(index, false));
            match operation {
                "make-empty" => qualify_array(TraitMethod {
                    type_params: vec!["t".to_owned()],
                    ..m(&[], "Self::Array<t>")
                }),
                "make-filled" => qualify_array(TraitMethod {
                    type_params: vec!["t".to_owned()],
                    ..m(
                        &[index.expect("make-filled index role"), "t"],
                        "Self::Array<t>",
                    )
                }),
                "len" => qualify_array(TraitMethod {
                    type_params: vec!["t".to_owned()],
                    ..m(&["Self::Array<t>"], index.expect("len index role"))
                }),
                "get" => qualify_array(TraitMethod {
                    type_params: vec!["t".to_owned()],
                    ..m(&["Self::Array<t>", index.expect("get index role")], "t")
                }),
                "set" => qualify_array(TraitMethod {
                    type_params: vec!["t".to_owned()],
                    ..m(
                        &["Self::Array<t>", index.expect("set index role"), "t"],
                        "()",
                    )
                }),
                "push" => qualify_array(TraitMethod {
                    type_params: vec!["t".to_owned()],
                    ..m(&["Self::Array<t>", "t"], "()")
                }),
                "pop-back" => qualify_array(TraitMethod {
                    type_params: vec!["t".to_owned()],
                    // The return alias is generic in the element type.
                    ret_type: format!("{}<t>", ffi("ret")),
                    ..m(&["Self::Array<t>"], "")
                }),
                "swap" => qualify_array(TraitMethod {
                    type_params: vec!["t".to_owned()],
                    ..m(
                        &[
                            "Self::Array<t>",
                            index.expect("swap index role"),
                            index.expect("swap index role"),
                        ],
                        "()",
                    )
                }),
                "clear" => qualify_array(TraitMethod {
                    type_params: vec!["t".to_owned()],
                    ..m(&["Self::Array<t>"], "()")
                }),
                "clone" => qualify_array(TraitMethod {
                    type_params: vec!["t".to_owned()],
                    ..m(&["Self::Array<t>"], "Self::Array<t>")
                }),
                other => unreachable!("unknown protocol array operation `{other}`"),
            }
        }
        HostFnBodyKind::MakeToken { .. }
        | HostFnBodyKind::TokenValue { .. }
        | HostFnBodyKind::BoxGet { .. }
        | HostFnBodyKind::BoxMake { .. }
        | HostFnBodyKind::CallStep { .. }
        | HostFnBodyKind::MakePairCallback { .. }
        | HostFnBodyKind::MakeStep { .. }
        | HostFnBodyKind::ApplyPoly { .. }
        | HostFnBodyKind::MakePairStructural { .. }
        | HostFnBodyKind::ProducePair { .. }
        | HostFnBodyKind::SumToString { .. }
        | HostFnBodyKind::RoundFunctor
        | HostFnBodyKind::ObservePacked { .. }
        | HostFnBodyKind::RoundPicker
        | HostFnBodyKind::RoundPolyThunk
        | HostFnBodyKind::RoundPolyUnitSlot
        | HostFnBodyKind::StagedSecond { .. }
        | HostFnBodyKind::NestedCurriedRoundtrip { .. }
        | HostFnBodyKind::InvokeSubstitutedUnitCallback { .. }
        | HostFnBodyKind::ReturnedForallUnit
        | HostFnBodyKind::TraceUnit { .. }
        | HostFnBodyKind::StagedUnitCall
        | HostFnBodyKind::UnreachableI32Print { .. } => unreachable!(
            "bespoke protocol host body reached canonical Rust signature rendering for `{}`",
            binding.leaf
        ),
    }
}

/// The canonical Rust-runner fixture for a fixed-width integer role, or
/// `None` if `kind` is not one. Production traits keep the exact associated
/// type in each host-fn slot; the runner chooses the corresponding primitive
/// as its own associated-type alias, so its normalized impl signature uses
/// that primitive too.
#[cfg(feature = "rust")]
#[allow(dead_code)]
fn rust_integer_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "i8" => Some("i8"),
        "i16" => Some("i16"),
        "i32" => Some("i32"),
        "i64" => Some("i64"),
        "i128" => Some("i128"),
        "u8" => Some("u8"),
        "u16" => Some("u16"),
        "u32" => Some("u32"),
        "u64" => Some("u64"),
        "u128" => Some("u128"),
        _ => None,
    }
}

/// The canonical Rust-runner fixture for a float role (`f32` / `f64`), or
/// `None` otherwise. Production traits retain the exact associated type; this
/// primitive is only the runner's selected alias for it.
#[cfg(feature = "rust")]
#[allow(dead_code)]
fn rust_float_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "f32" => Some("f32"),
        "f64" => Some("f64"),
        _ => None,
    }
}

/// Apply the Rust-keyword escape the emitter applies to host-fn names
/// (`loop` → `r#loop`, `mod` → `r#mod`). The runner replicates the
/// small subset it needs so a protocol method name matches the emitted
/// trait method name.
#[cfg(feature = "rust")]
#[allow(dead_code)]
fn escape_rust_keyword_runner(name: &str) -> String {
    match name {
        "as" | "break" | "const" | "continue" | "crate" | "else" | "enum" | "extern" | "false"
        | "fn" | "for" | "if" | "impl" | "in" | "let" | "loop" | "match" | "mod" | "move"
        | "mut" | "pub" | "ref" | "return" | "self" | "Self" | "static" | "struct" | "super"
        | "trait" | "true" | "type" | "unsafe" | "use" | "where" | "while" | "async" | "await"
        | "dyn" | "abstract" | "become" | "box" | "do" | "final" | "macro" | "override"
        | "priv" | "typeof" | "unsized" | "virtual" | "yield" | "try" | "gen" => {
            format!("r#{name}")
        }
        _ => name.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_empty_as_default() {
        assert_eq!(
            RunnerProtocol::parse(EMPTY_PROTOCOL_NAME),
            Ok(RunnerProtocol::Empty)
        );
        assert_eq!(RunnerProtocol::default(), RunnerProtocol::Empty);
        assert_eq!(DEFAULT_PROTOCOL_NAME, "empty-main");
    }

    #[test]
    fn canonical_names_roundtrip_for_the_complete_registry() {
        let mut names = std::collections::BTreeSet::new();

        for &protocol in RunnerProtocol::ALL {
            protocol.assert_registry_complete();
            let name = protocol.name();
            assert!(names.insert(name), "duplicate protocol name `{name}`");
            assert_eq!(RunnerProtocol::parse(name), Ok(protocol));
        }
    }

    #[test]
    fn every_contract_has_unique_host_item_identities() {
        for &protocol in RunnerProtocol::ALL {
            let contract = protocol.contract();
            let mut types = std::collections::BTreeSet::new();
            let mut functions = std::collections::BTreeSet::new();

            for binding in contract.host_types {
                assert!(
                    types.insert((binding.module, binding.leaf)),
                    "{} repeats host type {}/{}",
                    protocol.name(),
                    binding.module,
                    binding.leaf
                );
            }
            for binding in contract.host_fns {
                assert!(
                    functions.insert((binding.module, binding.leaf)),
                    "{} repeats host function {}/{}",
                    protocol.name(),
                    binding.module,
                    binding.leaf
                );
            }
        }
    }

    #[test]
    fn string_code_at_keeps_both_runtime_value_slots() {
        assert_eq!(
            HostFnBodyKind::string_code_at(TESTAPI_STRING_ROLE, TESTAPI_I32_ROLE)
                .canonical_prime_group_slots(),
            &[2]
        );
    }

    #[test]
    fn staged_second_preserves_interleaved_type_and_value_groups() {
        let body = HostFnBodyKind::StagedSecond {
            string: TESTAPI_STRING_ROLE,
        };
        assert_eq!(
            body.canonical_prime_signature(),
            "[#0] (h{testapi.String}) -> [#1] (h{testapi.String}) -> h{testapi.String}"
        );
        assert_eq!(body.canonical_prime_group_slots(), &[1, 1]);
    }

    #[test]
    fn nested_curried_protocol_pins_two_callable_layers_on_both_legs() {
        let contract = RunnerProtocol::NestedCurriedRoundtrip.contract();
        assert_eq!(
            contract.execution,
            ProtocolExecution::Invoke(ExportDriver::NestedCurriedRoundtrip)
        );
        assert_eq!(contract.host_types, TYPES_TESTAPI_STRING);
        assert_eq!(contract.host_fns, FNS_NESTED_CURRIED_ROUNDTRIP);
        let body = contract.host_fns[0].body;
        assert_eq!(
            body.canonical_prime_signature(),
            "((h{testapi.String} -> (h{testapi.String} -> h{testapi.String}))) -> (h{testapi.String} -> (h{testapi.String} -> h{testapi.String}))"
        );
        assert_eq!(body.canonical_prime_group_slots(), &[1]);
    }

    #[test]
    fn substituted_unit_callback_protocol_retains_one_callback_value_slot() {
        let contract = RunnerProtocol::HostSubstitutedUnitCallback.contract();
        assert_eq!(
            contract.execution,
            ProtocolExecution::Invoke(ExportDriver::HostSubstitutedUnitCallback)
        );
        assert_eq!(contract.host_types, TYPES_TESTAPI_TEXT);
        assert_eq!(contract.host_fns, FNS_HOST_SUBSTITUTED_UNIT_CALLBACK);
        assert!(matches!(
            contract.host_fns[0].body,
            HostFnBodyKind::InvokeSubstitutedUnitCallback { .. }
        ));
        assert_eq!(
            contract.host_fns[0].body.canonical_prime_signature(),
            "((. -> h{testapi.Text})) -> h{testapi.Text}"
        );
    }

    #[test]
    fn returned_forall_protocol_pins_call_by_value_host_inventory() {
        let contract = RunnerProtocol::ReturnedForallCallByValue.contract();
        assert_eq!(
            contract.execution,
            ProtocolExecution::Invoke(ExportDriver::ReturnedForallCallByValue)
        );
        assert_eq!(contract.host_fns, FNS_RETURNED_FORALL_CALL_BY_VALUE);
        assert_eq!(
            contract.host_fns[0].body.canonical_prime_group_slots(),
            &[0]
        );
        assert_eq!(
            contract.host_fns[0].body.canonical_prime_signature(),
            "(.) -> [#0] #0"
        );
        assert_eq!(
            contract.host_fns[1].body,
            HostFnBodyKind::TraceUnit { text: "observe" }
        );
        assert_eq!(
            contract.host_fns[2].body,
            HostFnBodyKind::TraceUnit { text: "consume" }
        );
    }

    #[test]
    fn staged_unit_protocol_distinguishes_retained_and_zero_slot_groups() {
        let contract = RunnerProtocol::HostStagedUnitCall.contract();
        assert_eq!(contract.host_fns, FNS_HOST_STAGED_UNIT_CALL);
        assert_eq!(
            contract.host_fns[0].body.canonical_prime_signature(),
            "[#0] (#0) -> [#1] (.) -> ."
        );
        assert_eq!(
            contract.host_fns[0].body.canonical_prime_group_slots(),
            &[1, 0]
        );
    }

    #[test]
    fn scalar_and_host_owned_exports_have_exact_native_inventories() {
        for (protocol, driver, host_types) in [
            (
                RunnerProtocol::ExportScalarRoundtrip,
                ExportDriver::ScalarRoundtrip,
                TYPES_EXPORT_SCALAR,
            ),
            (
                RunnerProtocol::ExportHostOwnedRoundtrip,
                ExportDriver::HostOwnedRoundtrip,
                TYPES_EXPORT_HOST_OWNED,
            ),
        ] {
            let contract = protocol.contract();
            assert_eq!(contract.execution, ProtocolExecution::Invoke(driver));
            assert_eq!(contract.host_types, host_types);
            assert!(contract.host_fns.is_empty());
            assert!(contract.testapi_conformed);
        }
    }

    #[test]
    fn callable_slots_export_has_only_its_exact_literal_dependencies() {
        assert_eq!(
            RunnerProtocol::parse(EXPORT_CALLABLE_SLOTS_ROUNDTRIP_PROTOCOL_NAME),
            Ok(RunnerProtocol::ExportCallableSlotsRoundtrip)
        );
        let contract = RunnerProtocol::ExportCallableSlotsRoundtrip.contract();
        assert_eq!(
            contract.execution,
            ProtocolExecution::Invoke(ExportDriver::CallableSlotsRoundtrip)
        );
        assert_eq!(contract.host_types, TYPES_EXPORT_CALLABLE_SLOTS);
        assert!(contract.host_fns.is_empty());
        assert!(contract.testapi_conformed);
    }

    #[test]
    fn rust_callback_aliases_has_no_host_dependencies() {
        let protocol = RunnerProtocol::parse(RUST_CALLBACK_ALIASES_PROTOCOL_NAME).unwrap();
        assert_eq!(protocol, RunnerProtocol::RustCallbackAliases);
        let contract = protocol.contract();
        assert_eq!(
            contract.execution,
            ProtocolExecution::Invoke(ExportDriver::RustCallbackAliases)
        );
        assert!(contract.host_types.is_empty());
        assert!(contract.host_fns.is_empty());
        assert!(contract.testapi_conformed);
    }

    #[test]
    fn exported_functor_dictionary_has_only_two_scalar_roles() {
        let protocol = RunnerProtocol::parse(EXPORT_FUNCTOR_DICT_ROUNDTRIP_PROTOCOL_NAME).unwrap();
        assert_eq!(protocol, RunnerProtocol::ExportFunctorDictRoundtrip);
        let contract = protocol.contract();
        assert_eq!(
            contract.execution,
            ProtocolExecution::Invoke(ExportDriver::FunctorDictRoundtrip)
        );
        assert_eq!(contract.host_types, TYPES_TESTAPI_I32_STRING);
        assert!(contract.host_fns.is_empty());
        assert!(contract.testapi_conformed);
    }

    #[test]
    fn host_existential_has_one_exact_nominal_observer() {
        let protocol = RunnerProtocol::parse(HOST_EXISTENTIAL_ROUNDTRIP_PROTOCOL_NAME).unwrap();
        assert_eq!(protocol, RunnerProtocol::HostExistentialRoundtrip);
        let contract = protocol.contract();
        assert_eq!(
            contract.execution,
            ProtocolExecution::Invoke(ExportDriver::HostExistentialRoundtrip)
        );
        assert_eq!(contract.host_types, TYPES_TESTAPI_I32);
        assert_eq!(contract.host_fns, FNS_HOST_EXISTENTIAL);
        assert_eq!(
            contract.host_fns[0].body.canonical_prime_signature(),
            "(n{testapi/types.Packed}) -> h{testapi.I32}"
        );
        assert!(contract.testapi_conformed);
    }

    #[test]
    fn portable_followup_protocols_pin_fixed_export_drivers() {
        for (protocol, driver, host_types, testapi_conformed) in [
            (
                RunnerProtocol::ExportNewtypeIgnoredArgumentRoundtrip,
                ExportDriver::NewtypeIgnoredArgumentRoundtrip,
                TYPES_TESTAPI_I32,
                true,
            ),
            (
                RunnerProtocol::RecursiveNewtypeBoundary,
                ExportDriver::RecursiveNewtypeBoundary,
                TYPES_MAIN_I32,
                false,
            ),
            (
                RunnerProtocol::ModuleAliasScopeCollision,
                ExportDriver::ModuleAliasScopeCollision,
                NO_HOST_TYPES,
                true,
            ),
            (
                RunnerProtocol::PublicWordNames,
                ExportDriver::PublicWordNames,
                TYPES_PUBLIC_WORD_NAMES,
                false,
            ),
        ] {
            let contract = protocol.contract();
            assert_eq!(contract.execution, ProtocolExecution::Invoke(driver));
            assert_eq!(contract.host_types, host_types);
            assert!(contract.host_fns.is_empty());
            assert_eq!(contract.testapi_conformed, testapi_conformed);
        }

        let selector = RunnerProtocol::FacadeSelectorCollisions.contract();
        assert_eq!(
            selector.execution,
            ProtocolExecution::Invoke(ExportDriver::FacadeSelectorCollisions)
        );
        assert_eq!(selector.host_types, TYPES_FACADE_SELECTOR_COLLISIONS);
        assert_eq!(selector.host_fns, FNS_FACADE_SELECTOR_COLLISIONS);
        assert!(selector.testapi_conformed);
    }

    #[test]
    fn array_clear_protocol_is_exact_without_broadening_legacy_array_contracts() {
        let leaves = |bindings: &[HostFnBinding]| {
            bindings
                .iter()
                .map(|binding| binding.leaf)
                .collect::<Vec<_>>()
        };

        assert_eq!(
            leaves(FNS_ARRAY_FULL),
            vec![
                "add_i32",
                "leq_i32",
                "mul_i32",
                "sub_i32",
                "array_clear",
                "array_clone",
                "array_get",
                "array_len",
                "array_make_empty",
                "array_make_filled",
                "array_pop_back",
                "array_push",
                "array_set",
                "array_swap",
                "bool_to_string",
                "int_to_string",
                "print",
                "loop",
                "string_concat",
                "string_eq",
            ]
        );
        assert_eq!(
            leaves(FNS_ARRAY_NO_CLEAR),
            vec![
                "add_i32",
                "leq_i32",
                "mul_i32",
                "sub_i32",
                "array_clone",
                "array_get",
                "array_len",
                "array_make_empty",
                "array_make_filled",
                "array_pop_back",
                "array_push",
                "array_set",
                "array_swap",
                "bool_to_string",
                "int_to_string",
                "print",
                "loop",
                "string_concat",
                "string_eq",
            ]
        );

        let clear = RunnerProtocol::TestApiArrayClear.contract();
        assert_eq!(clear.host_types, TYPES_HOST_ARRAY_RETURN_ONLY);
        assert_eq!(clear.host_fns, FNS_ARRAY_CLEAR);
        assert_eq!(
            leaves(clear.host_fns),
            vec![
                "array_clear",
                "array_len",
                "array_make_filled",
                "int_to_string",
                "print",
            ]
        );
        assert!(!RunnerProtocol::TestApiArrayClear.supports_dyn_load_prime());

        for protocol in [
            RunnerProtocol::HostGenericReturnOnlyRoundtrip,
            RunnerProtocol::TestApiArray,
            RunnerProtocol::TestApiArrayRoot,
            RunnerProtocol::TestApiArrayElabNoCloneSwap,
            RunnerProtocol::TestApiArrayElabPushOnly,
            RunnerProtocol::TestApiArrayElabFixed,
            RunnerProtocol::TestApiArrayElabStack,
            RunnerProtocol::TestApiArrayGetFilled,
        ] {
            assert!(
                protocol
                    .contract()
                    .host_fns
                    .iter()
                    .all(|binding| binding.leaf != "array_clear"),
                "{} inherited array_clear",
                protocol.name()
            );
        }
    }

    #[test]
    fn every_body_operation_uses_a_supported_shape() {
        const INTEGER_OPERATIONS: &[&str] = &["add", "sub", "mul", "div", "mod"];
        const FLOAT_OPERATIONS: &[&str] = &["add", "sub", "mul", "div"];
        const COMPARISON_OPERATIONS: &[&str] = &["eq", "lt", "leq", "gt", "geq"];
        const ARRAY_OPERATIONS: &[&str] = &[
            "make-empty",
            "make-filled",
            "len",
            "get",
            "set",
            "push",
            "pop-back",
            "swap",
            "clear",
            "clone",
        ];
        let assert_integer = |label: &str, role: HostRoleRef| {
            assert!(
                matches!(
                    role.fixture,
                    RoleFixture::I8
                        | RoleFixture::I16
                        | RoleFixture::I32
                        | RoleFixture::I64
                        | RoleFixture::I128
                        | RoleFixture::U8
                        | RoleFixture::U16
                        | RoleFixture::U32
                        | RoleFixture::U64
                        | RoleFixture::U128
                ),
                "{label} uses non-integer fixture {:?}",
                role.fixture
            );
        };
        let assert_float = |label: &str, role: HostRoleRef| {
            assert!(
                matches!(role.fixture, RoleFixture::F32 | RoleFixture::F64),
                "{label} uses non-float fixture {:?}",
                role.fixture
            );
        };
        let assert_numeric = |label: &str, role: HostRoleRef| {
            assert!(
                !matches!(role.fixture, RoleFixture::Bool | RoleFixture::String),
                "{label} uses non-numeric fixture {:?}",
                role.fixture
            );
        };

        for &protocol in RunnerProtocol::ALL {
            for function in protocol.contract().host_fns {
                let assert_supported = |label: &str, spelling: &str, supported: &[&str]| {
                    assert!(
                        supported.contains(&spelling),
                        "{} function {}/{} has unsupported {label} `{spelling}`",
                        protocol.name(),
                        function.module,
                        function.leaf
                    );
                };
                match function.body {
                    HostFnBodyKind::NumericToString { value, .. } => {
                        assert_numeric("numeric formatter", value);
                    }
                    HostFnBodyKind::Arithmetic { operation, number } => {
                        assert_supported("integer operation", operation, INTEGER_OPERATIONS);
                        assert_integer("integer arithmetic", number);
                    }
                    HostFnBodyKind::FloatArithmetic { operation, number } => {
                        assert_supported("float operation", operation, FLOAT_OPERATIONS);
                        assert_float("float arithmetic", number);
                    }
                    HostFnBodyKind::Compare {
                        operation, number, ..
                    } => {
                        assert_supported("comparison operation", operation, COMPARISON_OPERATIONS);
                        assert_integer("comparison", number);
                    }
                    HostFnBodyKind::Array {
                        operation, index, ..
                    } => {
                        assert_supported("array operation", operation, ARRAY_OPERATIONS);
                        let requires_index =
                            matches!(operation, "make-filled" | "len" | "get" | "set" | "swap");
                        assert_eq!(
                            index.is_some(),
                            requires_index,
                            "{} function {}/{} has the wrong Array index-role shape",
                            protocol.name(),
                            function.module,
                            function.leaf
                        );
                        if let Some(index) = index {
                            assert_integer("array index", index);
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    #[test]
    fn every_body_host_type_reference_resolves_exactly() {
        for &protocol in RunnerProtocol::ALL {
            let contract = protocol.contract();
            for function in contract.host_fns {
                for role in function.body.role_refs().into_iter().flatten() {
                    let matches = contract
                        .host_types
                        .iter()
                        .filter(|host_type| {
                            host_type.module == role.identity.module
                                && host_type.leaf == role.identity.leaf
                        })
                        .count();
                    assert_eq!(
                        matches,
                        1,
                        "{} function {}/{} role type {}/{} resolves {matches} times",
                        protocol.name(),
                        function.module,
                        function.leaf,
                        role.identity.module,
                        role.identity.leaf
                    );
                    role.resolve(contract.host_types);
                }
                let Some(identity) = function.body.referenced_host_type() else {
                    continue;
                };
                let matches = contract
                    .host_types
                    .iter()
                    .filter(|host_type| {
                        host_type.module == identity.module && host_type.leaf == identity.leaf
                    })
                    .collect::<Vec<_>>();
                assert_eq!(
                    matches.len(),
                    1,
                    "{} function {}/{} opaque type {}/{} resolves {} times",
                    protocol.name(),
                    function.module,
                    function.leaf,
                    identity.module,
                    identity.leaf,
                    matches.len()
                );
                let host_type = matches[0];
                let expected_fixture = match function.body {
                    HostFnBodyKind::Array { .. } => HostTypeFixture::Array,
                    HostFnBodyKind::MakeScalar { .. }
                    | HostFnBodyKind::ScalarOf { .. }
                    | HostFnBodyKind::ScalarAs { .. }
                    | HostFnBodyKind::ScalarIsTrue { .. } => HostTypeFixture::Scalar,
                    HostFnBodyKind::MakeToken { .. } | HostFnBodyKind::TokenValue { .. } => {
                        HostTypeFixture::Token
                    }
                    HostFnBodyKind::BoxGet { .. } | HostFnBodyKind::BoxMake { .. } => {
                        HostTypeFixture::Box
                    }
                    _ => unreachable!("referenced_host_type returned Some for an unrelated body"),
                };
                assert_eq!(
                    host_type.fixture,
                    expected_fixture,
                    "{} function {}/{} references host type {}/{} with the wrong fixture",
                    protocol.name(),
                    function.module,
                    function.leaf,
                    identity.module,
                    identity.leaf
                );
            }
        }
    }

    #[test]
    fn distinct_protocol_names_have_distinct_complete_contracts() {
        for (index, &left) in RunnerProtocol::ALL.iter().enumerate() {
            for &right in &RunnerProtocol::ALL[index + 1..] {
                assert_ne!(
                    left.contract(),
                    right.contract(),
                    "{} and {} are aliases for one complete contract",
                    left.name(),
                    right.name()
                );
            }
        }
    }

    #[test]
    fn empty_compile_and_construct_protocols_have_distinct_execution() {
        assert_eq!(
            RunnerProtocol::Empty.contract().execution,
            ProtocolExecution::Invoke(ExportDriver::Main { module: "main" })
        );
        assert_eq!(
            RunnerProtocol::CompileOnly.contract().execution,
            ProtocolExecution::CompileOnly
        );
        assert_eq!(
            RunnerProtocol::ConstructOnly.contract().execution,
            ProtocolExecution::ConstructOnly
        );
    }

    #[test]
    fn nonstandard_main_protocols_keep_exact_declaring_modules() {
        assert_eq!(RunnerProtocol::EmptyApiMain.main_module(), Some("api"));
        assert_eq!(
            RunnerProtocol::GeneratedCoreMain.main_module(),
            Some("prog")
        );
        assert_eq!(
            RunnerProtocol::GeneratedSurfaceMain.main_module(),
            Some("prog")
        );
        assert_eq!(
            RunnerProtocol::HostCallbackRoundtrip.main_module(),
            Some(TESTAPI_MAIN_MODULE)
        );
    }

    #[test]
    fn generated_protocols_own_their_exact_role_type_inventories() {
        let core = RunnerProtocol::GeneratedCoreMain.contract();
        assert_eq!(core.host_types, TYPES_GENERATED_CORE);
        assert_eq!(core.host_types.len(), 14);
        assert!(
            core.host_types
                .iter()
                .all(|binding| binding.module == "prog")
        );

        let surface = RunnerProtocol::GeneratedSurfaceMain.contract();
        assert_eq!(surface.host_types, TYPES_GENERATED_SURFACE);
        assert_eq!(surface.host_types.len(), 18);
        assert_eq!(
            surface
                .host_types
                .iter()
                .filter(|binding| binding.module == "testapi")
                .count(),
            4
        );
    }

    #[test]
    fn dict_collection_protocol_binds_list_elaborator_host_types() {
        let contract = RunnerProtocol::TestApiArithCollectionDictElab.contract();
        assert_eq!(contract.host_types.len(), 15);
        assert_eq!(
            contract
                .host_types
                .iter()
                .copied()
                .filter(|binding| binding.module == "list/elab/testapi")
                .collect::<Vec<_>>(),
            vec![
                HostTypeBinding::role("list/elab/testapi", "Bool", RoleFixture::Bool),
                HostTypeBinding::role("list/elab/testapi", "I32", RoleFixture::I32),
                HostTypeBinding::role("list/elab/testapi", "Int", RoleFixture::I32),
                HostTypeBinding::role("list/elab/testapi", "String", RoleFixture::String),
            ]
        );
        assert_eq!(contract.host_fns, FNS_ARITH_COLLECTION_I32);
        assert_eq!(
            contract.execution,
            ProtocolExecution::Invoke(ExportDriver::Main {
                module: TESTAPI_MAIN_MODULE,
            })
        );
    }

    #[test]
    fn queue_collection_protocol_binds_nested_list_elaborator_host_types() {
        let contract = RunnerProtocol::TestApiArithCollectionQueueElab.contract();
        assert_eq!(contract.host_types.len(), 15);
        assert_eq!(
            contract
                .host_types
                .iter()
                .copied()
                .filter(|binding| binding.module == "queue/list/elab/testapi")
                .collect::<Vec<_>>(),
            vec![
                HostTypeBinding::role("queue/list/elab/testapi", "Bool", RoleFixture::Bool),
                HostTypeBinding::role("queue/list/elab/testapi", "I32", RoleFixture::I32),
                HostTypeBinding::role("queue/list/elab/testapi", "Int", RoleFixture::I32),
                HostTypeBinding::role("queue/list/elab/testapi", "String", RoleFixture::String),
            ]
        );
        assert_eq!(contract.host_fns, FNS_ARITH_COLLECTION_I32);
        assert_eq!(
            contract.execution,
            ProtocolExecution::Invoke(ExportDriver::Main {
                module: TESTAPI_MAIN_MODULE,
            })
        );
    }

    #[test]
    fn same_leaf_host_types_retain_qualified_identity() {
        assert_eq!(
            RunnerProtocol::SameLeafHostLiteralRoles
                .contract()
                .host_types,
            &[
                HostTypeBinding::role("left", "Shared", RoleFixture::I64),
                HostTypeBinding::role("right", "Shared", RoleFixture::I32),
            ]
        );
    }

    #[test]
    fn rejects_unknown_protocol() {
        let err = RunnerProtocol::parse("default-main").unwrap_err();
        assert!(err.contains("unknown protocol"));
    }

    #[test]
    fn parses_every_roundtrip() {
        for (name, want) in [
            (
                EXPORT_NAMESPACE_ROUNDTRIP_PROTOCOL_NAME,
                RunnerProtocol::ExportNamespaceRoundtrip,
            ),
            (
                EXPORT_CALLBACK_ROUNDTRIP_PROTOCOL_NAME,
                RunnerProtocol::ExportCallbackRoundtrip,
            ),
            (
                EXPORT_MODULE_ROUNDTRIP_PROTOCOL_NAME,
                RunnerProtocol::ExportModuleRoundtrip,
            ),
            (
                EXPORT_MULTILABEL_ROUNDTRIP_PROTOCOL_NAME,
                RunnerProtocol::ExportMultilabelRoundtrip,
            ),
            (
                EXPORT_POLY_ROUNDTRIP_PROTOCOL_NAME,
                RunnerProtocol::ExportPolyRoundtrip,
            ),
            (
                EXPORT_STRUCTURAL_ROUNDTRIP_PROTOCOL_NAME,
                RunnerProtocol::ExportStructuralRoundtrip,
            ),
            (
                EXPORT_SCALAR_ROUNDTRIP_PROTOCOL_NAME,
                RunnerProtocol::ExportScalarRoundtrip,
            ),
            (
                EXPORT_HOST_OWNED_ROUNDTRIP_PROTOCOL_NAME,
                RunnerProtocol::ExportHostOwnedRoundtrip,
            ),
            (
                EXPORT_POLY_CALLBACK_ROUNDTRIP_PROTOCOL_NAME,
                RunnerProtocol::ExportPolyCallbackRoundtrip,
            ),
            (
                EXPORT_POSITIONAL_PRODUCT_ROUNDTRIP_PROTOCOL_NAME,
                RunnerProtocol::ExportPositionalProductRoundtrip,
            ),
            (
                EXPORT_TYPE_ROUNDTRIP_PROTOCOL_NAME,
                RunnerProtocol::ExportTypeRoundtrip,
            ),
            (
                EXPORT_CURRIED_FACADE_PROTOCOL_NAME,
                RunnerProtocol::ExportCurriedFacade,
            ),
            (
                EXPORT_WIDE_CALLABLE_PROTOCOL_NAME,
                RunnerProtocol::ExportWideCallable,
            ),
            (
                EXPORT_NEWTYPE_SUM_ROUNDTRIP_PROTOCOL_NAME,
                RunnerProtocol::ExportNewtypeSumRoundtrip,
            ),
            (
                EXPORT_NEWTYPE_SCALAR_ROUNDTRIP_PROTOCOL_NAME,
                RunnerProtocol::ExportNewtypeScalarRoundtrip,
            ),
            (
                NEWTYPE_VISIBILITY_FACADE_PROTOCOL_NAME,
                RunnerProtocol::NewtypeVisibilityFacade,
            ),
            (
                EXPORT_NESTED_PRODUCT_ROUNDTRIP_PROTOCOL_NAME,
                RunnerProtocol::ExportNestedProductRoundtrip,
            ),
            (
                EXPORT_COMPOUND_INPUT_ONCE_PROTOCOL_NAME,
                RunnerProtocol::ExportCompoundInputOnce,
            ),
            (
                HOST_CALLBACK_RETURN_ROUNDTRIP_PROTOCOL_NAME,
                RunnerProtocol::HostCallbackReturnRoundtrip,
            ),
            (
                HOST_CALLBACK_ROUNDTRIP_PROTOCOL_NAME,
                RunnerProtocol::HostCallbackRoundtrip,
            ),
            (
                NESTED_CURRIED_ROUNDTRIP_PROTOCOL_NAME,
                RunnerProtocol::NestedCurriedRoundtrip,
            ),
            (
                HOST_SUBSTITUTED_UNIT_CALLBACK_PROTOCOL_NAME,
                RunnerProtocol::HostSubstitutedUnitCallback,
            ),
            (
                RETURNED_FORALL_CALL_BY_VALUE_PROTOCOL_NAME,
                RunnerProtocol::ReturnedForallCallByValue,
            ),
            (
                HOST_STAGED_UNIT_CALL_PROTOCOL_NAME,
                RunnerProtocol::HostStagedUnitCall,
            ),
            (
                EXPORT_NEWTYPE_IGNORED_ARGUMENT_ROUNDTRIP_PROTOCOL_NAME,
                RunnerProtocol::ExportNewtypeIgnoredArgumentRoundtrip,
            ),
            (
                RECURSIVE_NEWTYPE_BOUNDARY_PROTOCOL_NAME,
                RunnerProtocol::RecursiveNewtypeBoundary,
            ),
            (
                FACADE_SELECTOR_COLLISIONS_PROTOCOL_NAME,
                RunnerProtocol::FacadeSelectorCollisions,
            ),
            (
                PUBLIC_WORD_NAMES_PROTOCOL_NAME,
                RunnerProtocol::PublicWordNames,
            ),
            (
                MODULE_ALIAS_SCOPE_COLLISION_PROTOCOL_NAME,
                RunnerProtocol::ModuleAliasScopeCollision,
            ),
            (
                HOST_GENERIC_RETURN_ONLY_ROUNDTRIP_PROTOCOL_NAME,
                RunnerProtocol::HostGenericReturnOnlyRoundtrip,
            ),
            (
                HOST_GENERIC_TYPE_ROUNDTRIP_PROTOCOL_NAME,
                RunnerProtocol::HostGenericTypeRoundtrip,
            ),
            (
                HOST_RANKN_ROUNDTRIP_PROTOCOL_NAME,
                RunnerProtocol::HostRanknRoundtrip,
            ),
            (
                HOST_STRUCTURAL_ROUNDTRIP_PROTOCOL_NAME,
                RunnerProtocol::HostStructuralRoundtrip,
            ),
            (
                HOST_TYPE_ROUNDTRIP_PROTOCOL_NAME,
                RunnerProtocol::HostTypeRoundtrip,
            ),
            (
                HOST_FUNCTOR_DICT_ROUNDTRIP_PROTOCOL_NAME,
                RunnerProtocol::HostFunctorDictRoundtrip,
            ),
            (
                HOST_POLY_FUNCTION_NEWTYPE_ROUNDTRIP_PROTOCOL_NAME,
                RunnerProtocol::HostPolyFunctionNewtypeRoundtrip,
            ),
            (
                HOST_POLY_UNIT_PAYLOAD_ROUNDTRIP_PROTOCOL_NAME,
                RunnerProtocol::HostPolyUnitPayloadRoundtrip,
            ),
            (
                HOST_INTERLEAVED_STAGE_ROUNDTRIP_PROTOCOL_NAME,
                RunnerProtocol::HostInterleavedStageRoundtrip,
            ),
            (ELAB_PROTOCOL_NAME, RunnerProtocol::Elab),
            (
                ROOT_SCOPED_HOST_ENV_PROTOCOL_NAME,
                RunnerProtocol::RootScopedHostEnv,
            ),
        ] {
            assert_eq!(RunnerProtocol::parse(name), Ok(want));
        }
    }

    #[cfg(feature = "rust")]
    fn rust_method(protocol: RunnerProtocol, module: &str, leaf: &str) -> TraitMethod {
        let contract = protocol.contract();
        let binding = contract
            .host_fns
            .iter()
            .find(|binding| binding.module == module && binding.leaf == leaf)
            .unwrap_or_else(|| panic!("protocol method `{module}/{leaf}` missing"));
        canonical_host_method(binding, contract.host_types)
    }

    #[cfg(feature = "rust")]
    #[test]
    fn exact_testapi_contract_preserves_each_declaring_module() {
        // The testapi bare-collection protocol roots each host function under
        // the exact module carried by each binding.
        let contract = RunnerProtocol::TestApiBareCollection.contract();
        let methods: Vec<_> = contract
            .host_fns
            .iter()
            .map(|binding| canonical_host_method(binding, contract.host_types))
            .collect();
        // `print` → `testapi/io`, `loop` → `testapi/iter` (no `r#`
        // escape — the module prefix makes the name keyword-safe),
        // `add` → `testapi/arith`, `string_concat` → `testapi/text`,
        // `int_to_string` → `testapi/fmt`.
        let names: Vec<&str> = methods.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"testapi_io__print"));
        assert!(names.contains(&"testapi_iter__loop"));
        assert!(names.contains(&"testapi_arith__add"));
        assert!(names.contains(&"testapi_text__stringConcat"));
        assert!(names.contains(&"testapi_fmt__intToString"));
    }

    #[cfg(feature = "rust")]
    #[test]
    fn testapi_array_contract_qualifies_assoc_at_type_root() {
        // The testapi array protocol declares `Array` at the `testapi` type
        // root, while the `array_*` host fns live in `testapi/array`. The
        // array methods must reference the type-root spelling
        // `Self::testapi__Array`, NOT the function's declaring-module spelling
        // `Self::testapi_array__Array` — the emitted trait names the assoc
        // type after its declaring (type) module.
        let array_method = rust_method(RunnerProtocol::TestApiArray, "testapi/array", "array_get");
        assert!(
            array_method
                .arg_types
                .iter()
                .any(|t| t.contains("Self::testapi__Array")),
            "array method must reference Self::testapi__Array, got {:?}",
            array_method.arg_types
        );
        assert!(
            !array_method
                .arg_types
                .iter()
                .any(|t| t.contains("testapi_array__Array"))
        );
        assert!(!array_method.ret_type.contains("testapi_array__Array"));
    }

    #[cfg(feature = "rust")]
    #[test]
    fn testapi_bigint_contract_renders_runner_alias_wide_int_signatures() {
        // The runner aliases each exact wide-integer associated type to its
        // canonical primitive, so its normalized impl signatures use `i64`,
        // `u64`, and `i128`. The emitted trait itself retains the exact
        // associated types.
        let by_leaf =
            |module: &str, leaf: &str| rust_method(RunnerProtocol::TestApiBigint, module, leaf);
        let add_i64 = by_leaf("testapi/arith", "add_i64");
        assert_eq!(add_i64.arg_types, vec!["i64".to_owned(), "i64".to_owned()]);
        assert_eq!(add_i64.ret_type, "i64");
        let mul_u64 = by_leaf("testapi/arith", "mul_u64");
        assert_eq!(mul_u64.arg_types, vec!["u64".to_owned(), "u64".to_owned()]);
        assert_eq!(mul_u64.ret_type, "u64");
        let i128_to_string = by_leaf("testapi/fmt", "i128_to_string");
        assert_eq!(i128_to_string.arg_types, vec!["i128".to_owned()]);
        assert_eq!(i128_to_string.ret_type, "String");
    }

    #[cfg(feature = "rust")]
    #[test]
    fn rust_signature_semantics_come_from_the_structured_body_not_the_leaf() {
        let binding = HostFnBinding {
            module: "unexpected/module",
            leaf: "looks_like_a_bool_helper",
            body: HostFnBodyKind::numeric_to_string(TESTAPI_I64_ROLE, TESTAPI_STRING_ROLE),
        };
        let method = canonical_host_method(&binding, TYPES_BIGINT);

        assert_eq!(
            method.name,
            crate::host_api::rust_host_member(binding.module, binding.leaf)
        );
        assert_eq!(method.arg_types, vec!["i64".to_owned()]);
        assert_eq!(method.ret_type, "String");
    }

    #[test]
    fn testapi_protocols_are_marked() {
        for p in [
            RunnerProtocol::TestApiPrint,
            RunnerProtocol::TestApiPrintMarkedString,
            RunnerProtocol::TestApiBareCollection,
            RunnerProtocol::HostTypeRoundtrip,
            RunnerProtocol::ExportTypeRoundtrip,
            RunnerProtocol::ExportNewtypeSumRoundtrip,
            RunnerProtocol::ExportNewtypeScalarRoundtrip,
            RunnerProtocol::NewtypeVisibilityFacade,
            RunnerProtocol::ExportNestedProductRoundtrip,
            RunnerProtocol::ExportCompoundInputOnce,
            RunnerProtocol::ExportWideCallable,
            RunnerProtocol::RootScopedHostEnv,
            RunnerProtocol::HostCallbackRoundtrip,
            RunnerProtocol::HostCallbackReturnRoundtrip,
            RunnerProtocol::NestedCurriedRoundtrip,
            RunnerProtocol::HostSubstitutedUnitCallback,
            RunnerProtocol::ReturnedForallCallByValue,
            RunnerProtocol::HostStagedUnitCall,
            RunnerProtocol::ExportNewtypeIgnoredArgumentRoundtrip,
            RunnerProtocol::FacadeSelectorCollisions,
            RunnerProtocol::ModuleAliasScopeCollision,
            RunnerProtocol::HostGenericReturnOnlyRoundtrip,
            RunnerProtocol::HostRanknRoundtrip,
            RunnerProtocol::HostGenericTypeRoundtrip,
            RunnerProtocol::HostStructuralRoundtrip,
            RunnerProtocol::HostFunctorDictRoundtrip,
            RunnerProtocol::HostPolyFunctionNewtypeRoundtrip,
            RunnerProtocol::HostPolyUnitPayloadRoundtrip,
            RunnerProtocol::HostInterleavedStageRoundtrip,
            RunnerProtocol::TestApiArray,
            RunnerProtocol::TestApiArrayClear,
            RunnerProtocol::TestApiBigint,
            RunnerProtocol::TestApiText,
            RunnerProtocol::TestApiCompute,
            RunnerProtocol::TestApiArithCollection,
            RunnerProtocol::TestApiFmt,
            RunnerProtocol::TestApiArith,
            RunnerProtocol::TestApiBareArith,
            RunnerProtocol::TestApiBareCompute,
            RunnerProtocol::TestApiIo,
            RunnerProtocol::ExportMultilabelRoundtrip,
            RunnerProtocol::ExportStructuralRoundtrip,
            RunnerProtocol::ExportScalarRoundtrip,
            RunnerProtocol::ExportHostOwnedRoundtrip,
        ] {
            assert!(p.is_testapi(), "{p:?} should be testapi-conformed");
        }
        assert!(!RunnerProtocol::Elab.is_testapi());
    }

    #[test]
    fn parses_testapi_protocols() {
        assert_eq!(
            RunnerProtocol::parse(TESTAPI_PRINT_PROTOCOL_NAME),
            Ok(RunnerProtocol::TestApiPrint)
        );
        assert_eq!(
            RunnerProtocol::parse(TESTAPI_BARE_COLLECTION_PROTOCOL_NAME),
            Ok(RunnerProtocol::TestApiBareCollection)
        );
        assert_eq!(
            RunnerProtocol::parse(TESTAPI_ARRAY_PROTOCOL_NAME),
            Ok(RunnerProtocol::TestApiArray)
        );
        assert_eq!(
            RunnerProtocol::parse(TESTAPI_ARRAY_CLEAR_PROTOCOL_NAME),
            Ok(RunnerProtocol::TestApiArrayClear)
        );
        assert_eq!(
            RunnerProtocol::parse(TESTAPI_BIGINT_PROTOCOL_NAME),
            Ok(RunnerProtocol::TestApiBigint)
        );
        assert_eq!(
            RunnerProtocol::parse(TESTAPI_TEXT_PROTOCOL_NAME),
            Ok(RunnerProtocol::TestApiText)
        );
        assert_eq!(
            RunnerProtocol::parse(TESTAPI_COMPUTE_PROTOCOL_NAME),
            Ok(RunnerProtocol::TestApiCompute)
        );
        assert_eq!(
            RunnerProtocol::parse(TESTAPI_ARITH_COLLECTION_PROTOCOL_NAME),
            Ok(RunnerProtocol::TestApiArithCollection)
        );
        assert_eq!(
            RunnerProtocol::parse(TESTAPI_FMT_PROTOCOL_NAME),
            Ok(RunnerProtocol::TestApiFmt)
        );
        assert_eq!(
            RunnerProtocol::parse(TESTAPI_ARITH_PROTOCOL_NAME),
            Ok(RunnerProtocol::TestApiArith)
        );
        assert_eq!(
            RunnerProtocol::parse(TESTAPI_BARE_ARITH_PROTOCOL_NAME),
            Ok(RunnerProtocol::TestApiBareArith)
        );
        assert_eq!(
            RunnerProtocol::parse(TESTAPI_BARE_COMPUTE_PROTOCOL_NAME),
            Ok(RunnerProtocol::TestApiBareCompute)
        );
        assert_eq!(
            RunnerProtocol::parse(TESTAPI_IO_PROTOCOL_NAME),
            Ok(RunnerProtocol::TestApiIo)
        );
    }
}
