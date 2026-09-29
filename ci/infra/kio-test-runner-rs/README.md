# kio-test-runner-rs

Per-backend test runners that drive the golden corpus through emitted artifacts.

- `kio-test-runner-js` — evaluates JS package modules (`*.js`) under rquickjs (an embedded QuickJS engine).
- `kio-test-runner-ts` — evaluates the TypeScript backend's package module. `kio build ts` writes the JS backend's `<pkg>.js` byte-identical (`specs/backends/ts.md` § Output layout) plus a `<pkg>.d.ts` typed-skin sidecar; this runner runs the `<pkg>.js` (the `.d.ts` is type-checked separately by `tsc --strict --noEmit` in CI, never by the runner). It therefore runs exactly the same artifact, the same way, as `kio-test-runner-js`, sharing the entire JS execution engine in [`src/shared/js_exec.rs`](src/shared/js_exec.rs) — the host-record builders, native callables, protocol drivers, and exact `<namespace>.js` loading.
- `kio-test-runner-python` — imports the Python backend's `<ns>.py` module (the file stem is the package namespace) with a synthesized protocol host object, instantiates it through the namespace-derived `create_<value-brand>` factory, and invokes the selected export driver.
- `kio-test-runner-java` — compiles the Java backend's emitted typed facade with a synthesized typed `Driver.java` (a `StubHost` implementing the emitted `<Handle>Host`), then runs the driver under the Java VM.
- `kio-test-runner-rust` — compiles and runs the emitted Cargo crate via direct `rustc` invocations.
- `kio-test-runner-go` — compiles and runs the emitted Go package with a synthesized driver.
- `kio-test-runner-swift` — compiles and runs the emitted Swift package with a synthesized driver.
- `kio-test-runner-haskell` — compiles and runs the emitted Haskell package with a synthesized driver.
- `kio-test-runner-dyn-load-prime` — interprets a golden's Kio' (Prime) image through `dyn_load_prime`'s `prime_eval` evaluator, for the interpreter-vs-compile-and-run differential. It is not per-backend like the runners above: it drives an AOT-compiled driver package vendoring `dyn_load_prime` (a JS module, built by [`dyn-load-prime-driver/build-driver.sh`](dyn-load-prime-driver/build-driver.sh)) under QuickJS as a `dyn_load_prime` host. It resolves the same exact shared protocol as every backend runner, then loads, constructs, invokes `main` in its exact declaring module, or drives a supported export-surface script according to that protocol's execution contract. See its module docs and [`dyn-load-prime-driver/README.md`](dyn-load-prime-driver/README.md). Its interpreter host remains independent of `HostApi` and the native canonical renderers.

A per-backend runner is per *backend*, not per compiler: it doesn't care which compiler produced the artifact, only that the artifact conforms to the per-backend contract in [`specs/backends/`](../../../specs/backends/). The js / ts / python / java / rust / go / swift / haskell bins share exact protocol and runner-support code via files in `src/shared/`, notably [`host_api.rs`](src/shared/host_api.rs), [`protocol.rs`](src/shared/protocol.rs), and [`runner.rs`](src/shared/runner.rs), included into each bin with `#[path]`. Typed-native bins additionally map structured protocol bodies through the [`CanonicalKind`](src/shared/canonical.rs) rendering vocabulary; js / ts share the whole JS execution engine ([`js_exec.rs`](src/shared/js_exec.rs)), since the `ts` backend's `<pkg>.js` is the JS backend's, byte-identical. There is no lib facade — `kio-test-runner-rs` is a bin-only crate.

## The protocol model

Every runner invocation runs one **named protocol**. A protocol is the complete
semantic contract the runner needs:

- the exact host types, with qualified identities, arities, roles, and native
  runner fixtures;
- the exact host functions, with qualified identities, structured signatures,
  and native bodies;
- the export driver; and
- whether the runner only compiles, constructs, or invokes the artifact.

Given a protocol, the runner builds that exact host implementation, links it
against the emitted package, and performs the protocol's execution step. A
case selects the protocol with `--protocol NAME`; it never supplements the
protocol with host declarations or fixture choices. If two cases need
different host contracts, they select different, sharpened protocols.

A package may export more than the selected driver calls; those extra exports
do not alter the protocol. Its host boundary, however, must match the selected
protocol exactly. This exactness is what lets the host compiler independently
detect emitter drift.

### What runners read from emitted artifacts

The runner reads no package source (`.kio` / `.pkg.kio`) and does not inspect
generated source, interfaces, manifests, descriptors, or comments to discover
host declarations, signatures, shapes, roles, fixtures, or package identity.
The harness supplies package/artifact invocation identity separately; that
identity addresses the artifact and does not change the protocol's semantic
host contract.

Runner invocations describe artifacts in positional order. Each descriptor
starts with `--package-name <source-name>` and may carry one target-local
`--artifact-namespace <effective-namespace>` before the next package
descriptor. The package name is only the input to that backend's default
namespace derivation when no override is present; it is not backend artifact
identity and never keys an override. Standard `run.args` files remain portable
by spelling configured values as
`--artifact-namespace <target-id>=<namespace>`; `ci/run-tests.sh` selects the
current target's value before invoking the runner.

Artifact loading and compilation are not interface discovery: QuickJS reads
the emitted JS module to execute it, native compilers receive their ordinary
compile roots, and build caches hash source trees generically. Likewise, a
runner may reference a stable published FFI alias by its protocol-derived
name. It may not parse the corresponding generated file to learn what that
alias means.

The payoff is regression detection for free. The runner spells every host
declaration and target signature from the protocol alone. If the emitted FFI
drifts, the independently synthesized host stops compiling or fails to link
or execute, and the regression surfaces normally.

The Rust runner needs to *name* the package's emitted shapes (sum returns, the `loop` step's result, roundtrip payloads) to spell its host implementation's signatures. It does so through the emitted crate's `ffi` boundary-alias module — `crate::ffi::env::<member>::<leaf>` / `crate::ffi::exp::<member>::<leaf>` — which the Rust emitter writes precisely so a consumer can reference a boundary type without re-deriving its structural spelling. Naming a shape through its alias is not reading build output to learn the interface; it is referencing a stable, emitted name the same way the package's own code does.

## Exact main protocols

Most corpus cases invoke an exported `main`. The common exact contracts use
names such as `testapi-print`, `testapi-fmt`, `testapi-text`,
`testapi-arith`, `testapi-compute`, `testapi-array`, and
`testapi-dyn-load`. A case whose host boundary differs—even by one function,
type leaf, or declaring module—uses a sharpened sibling name such as
`testapi-fmt-int-only` or `testapi-compute-loop`; the runner never filters a
larger contract after inspecting the artifact.

`empty-main` is the default exact empty-host contract and invokes `main` in the
`main` module. `empty-api-main` is the corresponding empty-host contract for a
`main` declared directly in module `api`. `main-box-fixture` binds the unary
opaque host type `main.Box[T]` to the runner's `Box` fixture and invokes
`main.main`. `generated-core-main` supplies the
generated core package's exact fourteen `prog` role types, while
`generated-surface-main` additionally supplies the four role types declared by
its bundled `testapi` support module; both invoke `main` in module `prog`.
`compile-only` only requires that the selected backend artifact
can be built or loaded. `construct-only` instantiates an exact empty-host
artifact but invokes no export. `elab-main` is the exact contract for the elab
POC, whose host boundary lives under `testapi` while its application entry
lives under `elab/main`.

[`RunnerProtocol::parse`](src/shared/protocol.rs) is the accepted-name
registry. Every accepted name resolves through
[`RunnerProtocol::contract`](src/shared/protocol.rs) to one complete
[`ProtocolContract`](src/shared/protocol.rs); tests enforce unique names,
unique qualified host-item identities, distinct complete contracts, and exact
resolution of every nominal host-type reference over the exhaustive
test-only `RunnerProtocol::ALL` catalogue.

### The coexist protocol

`--protocol coexist` is the one **two-artifact** protocol: it takes exactly two positional output directories (every other protocol takes exactly one) and drives one host program hosting **both** artifacts — the executable witness for `specs/backends/README.md` § The package facade § Coexistence. Its fixed contract: each artifact declares env `{print}` in module `greeter` and exports `greeter/main.main` plus the same-shaped `greeter/main.pair() -> (I32 & String)`; the driver instantiates the first artifact behind a host whose `print` prefixes `"first: "` and the second behind `"second: "` (positional, so the pinned output is backend-independent), calls the greetings first → second → first, then reads `pair()`'s positional product from both artifacts in the same order (`<label> pair: <n> <s>` lines). `ffi_two_packages_coexist` supplies two maximally colliding source packages, while `ffi_same_package_namespaces_coexist` uses two separate manifest roots with the same package name and byte-identical modules, each configured with a distinct artifact namespace. Together they prove package identity is not the isolation mechanism: the effective namespace isolates facades, runtime support, and structurally identical boundary shapes. Every host-backend bin implements the protocol; `kio-test-runner-dyn-load-prime` is exempt because it drives the Kio-authored interpreter rather than two host-language artifacts.

The principal **testapi-conformed** protocol families re-root the complete
test-facing surface under the fixed `testapi` namespace (see
[§ The namespaced host boundary](#the-namespaced-host-boundary--the-testapi-namespace)).
The table describes the family contracts; narrower cases select one of the
sharpened exact variants in the protocol registry.

| Protocol family | exact host contract |
| --- | --- |
| `testapi-print` | the exact `String` role type and `testapi/io.print`. |
| `testapi-print-marked-string` | the same executable shape with the exact marked role type `_String`, independently exercising each backend's public host-type naming. |
| `testapi-fmt` | print plus the exact integer/bool formatting functions and role types. |
| `testapi-text` | formatting plus the exact string-operation functions and role types. |
| `testapi-arith` | print, integer formatting, and `_i32` arithmetic functions. |
| `testapi-bare-arith` | print, integer formatting, and unsuffixed i32 arithmetic functions. |
| `testapi-compute` | the exact arithmetic, comparison, text, input, and loop contract used by compute cases. |
| `testapi-compute-rec-partial-let` | the exact `Bool`, `I32`, and `String` role types plus `leq_i32`, `i32_to_string`, `print`, and `loop`. |
| `testapi-bare-compute` | the corresponding compute contract with unsuffixed i32 arithmetic. |
| `testapi-io` | the exact print, stderr, exit, and input contract. |
| `testapi-arith-collection` | print, formatting, string concatenation, `_i32` arithmetic/comparison, and loop functions used by the collection family. |
| `testapi-bare-collection` | the corresponding collection contract with unsuffixed i32 arithmetic and `string_eq`. |
| `testapi-array` | exact array, formatting, arithmetic, text, and loop functions plus the non-role `testapi/Array[T]` host type. |
| `testapi-array-clear` | exactly `Array[T]`, `I32`, and `String`, with `array_clear`, `array_len`, `array_make_filled`, `int_to_string`, and `print`. |
| `testapi-bigint` | a wide-integer contract: role host types `I64` / `U64` / `I128` / `U128`, their formatters, and selected arithmetic. Exercises the JS `BigInt` value shape. |
| `testapi-float` | `{print, f64_to_string, add_f64, sub_f64, mul_f64}` with exactly the `F64` and `String` role types. |
| `testapi-float-f32-f64` | The `testapi-float` contract plus the `F32` role type, `f32_to_string`, and `add_f32`. |
| `testapi-dyn-load` | exactly the `dyn_load_prime` package's `testapi` host surface, including the opaque `Scalar` type and its conversion functions. |

The `testapi-array` surface declares `host type Array[T];` at the
`testapi` root and the array primitives in `testapi/array`:

```text
host fn array_make_empty[T]() -> Array(T);
host fn array_make_filled[T](I32, T) -> Array(T);
host fn array_len[T] Array(T) -> I32;
host fn array_get[T](Array(T), I32) -> T;
host fn array_set[T](Array(T), I32, T) -> .;
host fn array_push[T](Array(T), T) -> .;
host fn array_pop_back[T] Array(T) -> T | .;
host fn array_swap[T](Array(T), I32, I32) -> .;
host fn array_clone[T] Array(T) -> Array(T);
```

The focused `testapi-array-clear` protocol uses only the `Array[T]`, `I32`,
and `String` host types and this exact function inventory:

```text
host fn array_clear[T](Array(T)) -> .;
host fn array_len[T](Array(T)) -> I32;
host fn array_make_filled[T](I32, T) -> Array(T);
host fn int_to_string(I32) -> String;
host fn print(String) -> .;
```

`array_clear` removes every element from the mutable array.

The collection families cover recursive libraries that fold a host `loop`,
compare keys or indices, and render `Bool`. Sharpened siblings omit functions
the selected package does not declare; that omission is represented in the
protocol name and contract, never discovered from generated code.

## Roundtrip protocols

The roundtrip protocols pin one exact host contract and export driver for one
FFI-surface regression each. FFI-focused goldens add a protocol here rather
than broadening an unrelated exact contract; golden case names use the `ffi_`
prefix and protocol names are kebab-case. The `root-scoped-*` protocols are
multi-root exact contracts (see
[§ The namespaced host boundary](#the-namespaced-host-boundary--the-testapi-namespace)).

Declaration spellings below identify Kio source entries. Each driver applies
its backend's published public-name projection when calling those entries;
explicit host aliases are shown in their emitted spelling.

| Protocol | What it exercises |
| --- | --- |
| `export-namespace-roundtrip` | Exported functions from the `main` and `utils` module namespaces, reached through each backend's published facade selectors. |
| `export-callback-roundtrip` | A callback passed into exported `apply_twice`, and the callback returned by exported `make_step`. |
| `export-module-roundtrip` | Functions exposed from semantic module `testapi/api`, reached through each backend's published facade selectors. |
| `export-multi-label-roundtrip` | Label-generated newtypes across product, function, and nominal member boundaries. The driver calls `say({A: 42, B: "shown\n"})`, round-trips `{A: 88, B: "99"}` through `echo_pair` and prints both projected slots, then constructs `A.mk(111)`, passes it through `echo_a`, projects it with `A.get`, and prints `111`. |
| `export-poly-roundtrip` | Polymorphic exported functions at several concrete instantiations. |
| `export-poly-callback-roundtrip` | A host closure crossing into a polymorphic export typed by the export's own type parameters (golden `ffi_export_poly_callback`): `apply_via[K][R](f: K -> R, x: K) -> R` in `testapi/main`, threading the closure through an internal generic call — the same export shape `exec_host_closure_type_param_leg` declares, exercised from the host side. The driver calls it at two concrete instantiations (a string transform printing `via: apply`, an integer step printing `15`), so a boundary wrapper that fails to convert the erased argument to `K` before invoking the host closure (or to re-erase the `R` result) fails at runtime. Empty env (no host items — the signature reaches only type parameters). Cross-backend: js / ts / python / java / rust / go / swift / haskell. |
| `export-structural-roundtrip` | Exported structural values built only through emitted `exp::...` FFI aliases / pattern synonyms: `pair_swap((I32 & String)) -> (String & I32)`, `dispatch_left((I32 \| String)) -> String` on both sum arms, `rotate` over a 12-slot product, and `echo_sum` / `classify` roundtripping and observing every payload in a 10-arm sum, including integers beyond JS Number precision and both Boolean values. Chooser exports also return early/middle/late sum arms that the driver feeds back into `classify`. |
| `export-scalar-roundtrip` | Native-value assertions on direct I128, U128, F32, and F64 export arguments and returns. The integers exceed 64 bits; finite fractional floats include an F64 value that cannot roundtrip through F32. |
| `export-host-owned-roundtrip` | Roleless `Token` and parameterized host-owned `Box[T]` cross exported identity functions. The host observes two token payloads and box payloads at I32 and String through its selected representation and the published host-type carriers. No object-identity guarantee is assumed. |
| `export-callable-slots-roundtrip` | Bare product and sum slots carry I32 callbacks. The package invokes host callbacks; the host invokes roundtripped callbacks and package-produced identities with distinct values. Exact callback counts and the scalar sum arm are asserted through the public structural interface. |
| `export-functor-dict-roundtrip` | An exported `Functor[Box]` dictionary crosses in both directions. Hosts invoke the package's generic application helper and directly project and invoke `fmap` at I32-to-String and String-to-I32, checking each Box payload and the exact callback event sequence. |
| `host-existential-roundtrip` | The host receives `Packed<U>: U & (U -> I32)`, opens it through the public CPS projector, and invokes the operation on the same hidden-type seed. Scalar and product witnesses return 37 and 83; both host observations and continuation calls are counted. Testapi-conformed; only I32 and `testapi/arith.observe`. All eight native backends. |
| `rust-callback-aliases` | Rust-only public-name regression: callback parameter/result aliases retain forall binders, an existential continuation names its nested polymorphic payload, and a returned callback names its polymorphic result. Host implementations use only stable aliases and invoke identities at distinct native types. Other backends have different naming contracts and are not applicable. Empty host inventory. |
| `export-positional-product-roundtrip` | Flat (not testapi-conformed) golden. Exported `make_pair(I32, String) -> I32 & String` read back through its positional `_0` / `_1` fields, after a product of label-minted newtypes forced the shared 2-slot `Prod_<hash>` struct to register first — guards that structural-product FFI field names stay positional regardless of registration order. Empty env; the return is named through the `exp::main__makePair::ret` alias (Rust) / the `{_0,_1}` object shape (JS). |
| `export-type-roundtrip` | The exported `Pair.mk_pair` constructor with a `(String & String)` payload (built through Rust's `ffi::exp::testapi_types__Pair::mkPair_arg0`), projected back through `Pair.un_pair`. Testapi-conformed: `Pair` lives in `testapi/types`; the runner reaches that semantic module and type identity through each backend's published role-framed selectors. |
| `export-newtype-sum-roundtrip` | A sum-arm newtype-over-product export regression (golden `ffi_export_newtype_sum_roundtrip`): `Tagged = Pr \| .`, where the label-minted `Pr` wraps `(I32 & String)`. Exported `pack(I32, String) -> Tagged` (Out) and `first_or(I32, Tagged) -> I32` (In); the driver calls `first_or(0, pack(7, "hi"))` and prints the recovered first field. The In conversion reads the `Pr` arm's product fields, so the boundary representation must retain the typed product payload rather than erase it to the universal value. Testapi-conformed: `pack` + `first_or` in `testapi/main`. Empty env; the driver only chains the two exports. Cross-backend: js / ts / python / java / rust / go. |
| `export-curried-facade` | Exported curried fns `pick(a: Str)(b: Str) -> Str` and `last(a: I32)(b: I32, c: I32) -> I32` driven **flat** through the facade — `pick("ku", "rz")`, `last(1, 2, 3)` — pinning that every backend's host surface flattens value groups into one call while routing each group to its internal layer (`specs/backends/README.md` § Function-type FFI canonicalization). The env has no host functions; exact role host types still appear as associated types on statically typed host contracts. Cross-backend: js / ts / python / java / rust / go / swift / haskell. |
| `export-wide-callable` | A 255-slot exported `select` callable split across three 85-slot source groups, plus `make_select`, which returns a callable with one 255-slot stage. A real host invokes both with 0 through 254 and reads each returned first, middle, and last I32 value. This crosses Java's 254-reference-slot cutoff for both the whole declaration head and the nested SAM stage, while exercising the same facade semantics on js / ts / python / java / rust / go / swift / haskell. Testapi-conformed: both exports live in `testapi/main`; exact I32 role type and no host functions. |
| `export-newtype-scalar-roundtrip` | Exported `bump(Wrap) -> Wrap` where `Wrap` is a **bare scalar-payload newtype** (`newtype Wrap : I32`) crossing a `pub fn` as both parameter (In) and return (Out). Guards that each export wrapper bridges the newtype against the erased payload the body holds: on Rust the nominal `Wrap<H>` struct crosses and the wrapper converts via the newtype bridge (a bare `as_any` / `from_any::<struct>` would panic at runtime); the erased-static backends (go / swift) and the native-skin backend (haskell) erase the bare scalar at the boundary, so the export takes / returns the raw payload, and JS/Python/Java cross it as the `{ Wrap: <payload> }` boundary record. Testapi-conformed: `Wrap` + `bump` in `testapi/main`. Empty env; the driver round-trips one value, printing `7`. Cross-backend: js / ts / python / java / rust / go / swift / haskell. |
| `export-newtype-ignored-argument-roundtrip` | A recursive-looking ignored type argument (golden `ffi_export_newtype_ignored_argument_roundtrip`): `Const[A]` always carries I32, while `Wrap` carries `Const(Wrap)`. The driver chains `from_i32(7)` into `to_i32`, so the boundary must remain a finite scalar-backed newtype rather than introduce a recursive carrier. Go additionally type-checks the returned public carrier as `int32`; Python asserts the exact `{Wrap: {Const: 7}}` public record before projection. Testapi-conformed: the types and exports live in `testapi/main`; the root declares the exact I32 role. Cross-backend: js / ts / python / java / rust / go / swift / haskell, plus Kio' JS. |
| `recursive-newtype-boundary` | Alias-hidden and generic-sibling recursive public newtypes (goldens `build_alias_hidden_recursive_carrier` and `build_generic_newtype_recursive_sibling_boundary`). One shared driver obtains a finite structural payload from `main.base_payload`, invokes `Root.make_root` directly, round-trips the nominal value through `main.keep`, invokes `Root.read_root` directly, then prints `main.accept_payload`'s I32 base-branch tag. This causally pins the constructor/projector and wrapper In/Out conversions without reconstructing a recursive host value. Flat `main`; exact I32 role and no host functions. Cross-backend: js / ts / python / java / rust / go / swift / haskell, plus Kio' JS. |
| `newtype-visibility-facade` | Natural host-facade use of public newtypes. The driver round-trips opaque values, uses constructor-only and projector-only members, and keeps same-leaf newtypes in distinct module namespaces through the selected `testapi.I32` host type. Polymorphic constructor/projector payloads execute at I32 and Unit; existential polymorphic and recursive callable payloads cross actual CPS openers with counted continuations. Those two existential APIs provide no hidden-type seed, so their payload callables are observed through opening, not invoked. Testapi-conformed; only the exact `I32` role. Cross-backend: js / ts / python / java / rust / go / swift / haskell. |
| `export-nested-product-roundtrip` | A **named newtype product nested inside another named product** crossing an exported-fn return (golden `ffi_export_nested_product_roundtrip`): the label-minted `Inner` wraps `(I32 & I32)` and `Outer` wraps `(String & Inner)`. Exported `make(String, I32, I32) -> Outer`; the driver calls `make("nest", 7, 9)` and prints the outer string slot and both inner integer slots. Each product nesting level's Out conversion opens its own binding scope, so the skin must mint a fresh binder per level — the focused regression for the JS skin's temporal-dead-zone `__slots` shadowing (the nested conversion's `const` captured the enclosing binder it reads in its own initializer, a `ReferenceError` at every call). Testapi-conformed: `Inner` / `Outer` / `make` in `testapi/main`. Empty env; the driver only reads one export's fields. Cross-backend: js / ts / python / java / rust / go / swift / haskell. |
| `export-compound-input-once` | Direct host and host-supplied callback product results print one invocation event each. The driver checks their values, round-trips a nested product and all three alternatives of a sum, then an atomic string. JS/TS count actual getter reads and reject reads of absent sum alternatives while preserving a legal one-key sum. Python counts accesses through a dictionary subclass. All eight native runners exercise the public facade; the dynamic script checks invocation counts, values and selected arms through the loaded Surface, without claiming native getter coverage. |
| `host-callback-return-roundtrip` | A host-returned callback (`make_step`) crossing back into module code. Testapi-conformed: `make_step` in `testapi/arith`, exported `main` in `testapi/main`. |
| `host-callback-roundtrip` | Module callbacks crossing into host calls (`call_step` with a compound-product argument, `make_pair` with a compound-product return), both named through the `env::*::arg0_cbarg` / `arg0_cbret` aliases. Testapi-conformed: the callback host fns live in `testapi/arith`, exported `main` in `testapi/main`. |
| `nested-curried-roundtrip` | A genuinely nested callback (golden `ffi_nested_curried_roundtrip`): `String -> String -> String` crosses the `round_host` host parameter and return, then crosses the exported `round_export` parameter and return. The driver obtains and invokes each returned callable layer separately on both legs, so a facade that flattens the function value cannot satisfy the protocol. Testapi-conformed: `round_host` and `round_export` live in `testapi/api`; the root declares the exact `String` role. Cross-backend: js / ts / python / java / rust / go / swift / haskell, plus Kio' JS. |
| `host-substituted-unit-callback` | A generic callback alias instantiated at Unit (golden `ffi_host_substituted_unit_callback`): `Callback[A] = A -> Text`, and `invoke(Callback[Unit]) -> Text` retains one public value slot after substitution. Every fixed host driver invokes the callback with an explicit Unit value. Testapi-conformed: `Callback`, `invoke`, and the export live in `testapi/api`; the root declares the exact `Text` role. Cross-backend: js / ts / python / java / rust / go / swift / haskell, plus Kio' JS. |
| `returned-forall-call-by-value` | A returned polymorphic host value (golden `ffi_returned_forall_call_by_value`): `produce(Unit) -> [A] A` is captured before `observe`, then instantiated at Unit twice and sent to `consume`. The first fixed `produce / observe / consume / consume` trace proves the host call occurs once and neither returned type application replays it. The driver then invokes `main` again; `produce` emits `throw` and signals the protocol's exact host failure. Runners with catchable host exceptions prove it propagates through the package boundary before emitting `caught`; Swift's non-throwing host method records the failure and suppresses the later calls before the driver asserts it and emits `caught`. The ordinary Unit parameters are zero-slot boundary groups. Testapi-conformed in `testapi/main`; empty host-type inventory. Cross-backend: js / ts / python / java / rust / go / swift / haskell, plus Kio' JS. |
| `host-staged-unit-call` | An alternating staged declaration (golden `ffi_host_staged_unit_call`): `[A](A)[B](Unit) -> Unit`, invoked with both type variables instantiated at Unit. The first generic value group retains one public slot after substitution, while the declaration's literal Unit argument remains a zero-slot group; every fixed host receives exactly the retained argument and prints one trace. Testapi-conformed in `testapi/main`; empty host-type inventory. Cross-backend: js / ts / python / java / rust / go / swift / haskell, plus Kio' JS. |
| `facade-selector-collisions` | Public-selector collision facts (golden `ffi_facade_selector_collisions`): root modules whose names collide with generated facade members, `foo/bar` versus `foo_bar`, the Turkish-case-folding-sensitive `i`, and a same-spelling function, nested module, and public newtype in `api`. The driver reaches every surface through each backend's published selectors; the three qualified `read(I32, I32) -> I32` host members use identity-distinct operations — add `(9, 1)`, multiply `(10, 2)`, subtract `(41, 1)` — while all print `10`, `20`, and `40`, so cross-wiring changes the observable output. Testapi-conformed with exact I32 roles. Cross-backend: js / ts / python / java / rust / go / swift / haskell, plus Kio' JS. |
| `public-word-names` | Word-separated names and distinct leading/trailing underscore affixes on five exported values, an affixed generic newtype and its public members, and nested module paths (golden `ffi_public_word_names`). Fixed drivers call the published names independently. JS/TS/Python additionally construct and inspect the transparent `_WordBox__` wrapper and a same-leaf product's qualified `wordApi/otherNodes._WordBox__` key. The exact host contract is `word_api/Count_value` with the I32 role and no host functions. Cross-backend: js / ts / python / java / rust / go / swift / haskell, plus Kio' JS; this host-name protocol is not a dynamic-loader witness. |
| `module-alias-scope-collision` | Two modules export same-leaf `State`, `make`, and `consume` declarations with distinct product shapes (golden `ffi_module_alias_scope_collision`). Each fixed driver feeds each module's `make` result only to that module's `consume`, proving public aliases remain scoped to their semantic owner. Testapi-conformed; empty host inventory. Cross-backend: js / ts / python / java / rust / go / swift / haskell. |
| `host-generic-return-only-roundtrip` | The exact narrow array contract where `array_make_empty` is passed as a function value; `array_push` writes `7`, `array_get` reads it back, and `int_to_string` plus `print` render the result. |
| `host-generic-type-roundtrip` | A polymorphic host-owned type `Box[T]` with `box_make` / `box_get`. Testapi-conformed: `Box` at the `testapi` root, the helpers in `testapi/arith`. |
| `host-rankn-roundtrip` | A rank-n exported function value (`apply_poly`) crossing into a host call. Native-HKT hosts retain the first-class type-application stage; the Haskell runner visibly applies `Text`, sequences the resulting action, and invokes the returned function. Erased backends retain no runtime type argument. Testapi-conformed: `apply_poly` in `testapi/arith`, exported `main` in `testapi/main`. |
| `host-structural-roundtrip` | Direct host product returns (`make_pair`) — the host function value structural-return case. Testapi-conformed: `make_pair` in `testapi/arith`, `sum_to_string` in `testapi/fmt`, exported `main` in `testapi/main`. |
| `host-type-roundtrip` | Exact declaration binding plus an opaque `Token`: `I32` and `Count` deliberately select distinct public host types backed by the same native i32 fixture, `String` selects a third type, and `make_token` / `token_value` preserve the roleless declaration. Testapi-conformed: the types live at the `testapi` root and helpers in `testapi/{arith,fmt,opaque}`. |
| `host-functor-dict-roundtrip` | A functor/monad **dictionary** (`Functor[*F]`) crossing the host boundary — `round_functor(Functor(Box)) -> Functor(Box)` receives a built dictionary and hands one back (the dictionary crosses both as a parameter and a return). The dictionary is an ordinary value (a record of functions, not a host typeclass instance); each runner's body is the identity. Testapi-conformed: `round_functor` in `testapi/arith`, the `Box` / `Functor` newtypes in `testapi/types`, exported `main` in `testapi/main`. Cross-backend: js / ts / python / java / rust / go / swift / haskell; Haskell retains the native rank-N dictionary type. |
| `host-poly-function-newtype-roundtrip` | Minimal host-identity regression (golden `ffi_host_poly_function_newtype_roundtrip`) for `Pick_first : [A] (A & A) -> A`: natural two-binder source is retained internally through a packed one-argument ABI adapter, exposed as the canonical two-argument public callable, passed to and returned unchanged by `round_picker`, then projected and invoked. This isolates both directions of the public-boundary/internal-ABI repartition. Testapi-conformed: `round_picker` in `testapi/arith`, `Pick_first` in `testapi/types`, exported `main` in `testapi/main`. Cross-backend: js / ts / python / java / rust / go / swift / haskell, plus Kio' JS and the dyn-load-prime differential. |
| `host-poly-unit-payload-roundtrip` | Paired host-identity regression (golden `ffi_host_poly_unit_payload_roundtrip`) for `Poly_thunk : [A] . -> .` and `Unit_slot[T] : [A] T -> T` instantiated at Unit. The former callable has zero public value slots; the latter retains one explicit Unit-valued slot. Both newtypes cross the host boundary as a parameter and return before projection and invocation. Testapi-conformed: the identity host functions live in `testapi/arith`, the newtypes in `testapi/types`, and exported `main` in `testapi/main`. Cross-backend: js / ts / python / java / rust / go / swift / haskell, plus Kio' JS and the dyn-load-prime differential. |
| `host-interleaved-stage-roundtrip` | An interleaved host declaration (golden `ffi_host_interleaved_stage_roundtrip`): `staged[A](first: String)[B](second: String) -> String`. The host receives the canonical two-value call and returns `second`, pinning both value groups across intervening type stages. Testapi-conformed: `staged` in `testapi/arith`, `print` in `testapi/io`, exported `main` in `testapi/main`. Cross-backend: js / ts / python / java / rust / go / swift / haskell, plus Kio' JS and the dyn-load-prime differential. |
| `root-scoped-host-env` | Two root modules under `testapi` declaring the same leaf with distinct signatures (`testapi/alpha`'s `print(String)` + `testapi/beta`'s `print(I32)`); only `alpha.print` is called, `beta.print` is a declared-but-uncalled member. The exact qualified bindings keep the two members distinct. |

## The namespaced host boundary — the `testapi` namespace

The host boundary is **module-namespaced**: the emitted Rust trait normally names each host item `<module-with-slashes-as-underscores>__<leaf>` (`testapi_io__print`). A source module containing `_` uses the Rust backend's reserved injective spelling instead (`__kio_host_`, with `_` → `_u` and `/` → `_s`). Associated types use the same rule. The JS package instead looks each item up at `__host__.<MODULE_NS>.<leaf>` (nested by the module's JS namespace). The runner must emit a matching `impl Host` / host record, so it needs the **declaring module** of every protocol host item.

The declaring module is **carried by each exact protocol binding**, never
carried by a case-specific `run.args` flag, read from `.kio` source, inferred
from a leaf, or derived from generated output. A **testapi-conformed** golden
re-roots its whole test-facing surface under one fixed `testapi` namespace,
and the runner reconstructs that namespacing from the protocol alone:

- Every host **function** binding stores its exact module and leaf, such as
  `testapi/io.print`, `testapi/arith.add_i32`, or
  `testapi/opaque.make_token`.
- Every host **type** binding likewise stores its exact module and leaf, such
  as `testapi/Token` or `testapi/Box`; an opaque type reference in a function
  signature points to that exact identity, not merely to a shared native
  fixture.
- The program **entry** is the `main` fn in `testapi/main`; other exported
  items (roundtrip goldens) live under `testapi/…` (e.g. an exported type in
  `testapi/types`). Each runner renders those semantic identities using its
  backend's published module and type selectors.

Each backend encodes those exact identities according to its published naming
rules. The JS record, for example, nests a leaf under the declaring module's
public module key (`testapi_io`, `testapi_arith`, …).

`testapi` is a test-corpus convention, not a language concept;
[`RunnerProtocol::is_testapi`](src/shared/protocol.rs) marks the conformed
protocols. The `root-scoped-host-env` contract demonstrates why identity is
structured: `testapi/alpha.print` and `testapi/beta.print` are distinct even
though they share one leaf. The host compiler or dynamic engine is the
checker: a namespace or signature mismatch against the emitted package fails.

## How the Rust runner composes `impl Host`

The Rust runner starts with the selected protocol's complete exact API.

1. **Protocol projection.** `host_api_for_protocol` renders every exact host
   type and function binding into the corresponding Rust associated type or
   trait method. Each protocol host-type binding selects the runner fixture:
   ordinary role fixtures use convenient canonical primitives, while an exact
   selected-role fixture may use a distinct wrapper to prove that the public
   host contract preserves the host's choice. Production hosts remain free to
   select any type satisfying the trait bounds. Shaped slots (a sum return, the `loop` step's
   where-clause, a roundtrip payload) are named through stable emitted `ffi`
   aliases whose paths are derived from the protocol identity, without parsing
   their definitions.
2. **Bodies from the protocol.** Each host function carries a structured body in
   the selected protocol. The Rust adapter maps that body to shared
   `CanonicalKind` rendering vocabulary where applicable and handles
   protocol-specific bodies explicitly. It never infers behavior from the
   function's name or emitted signature.
3. **Compile and run.** The harness-supplied artifact identity determines the
   crate name and branded factory. The runner passes `src/lib.rs` to rustc,
   writes the synthesized driver, links the two, and runs the binary. A
   content-addressed rlib/bin cache skips rustc on warm runs.

The JS runner does not need native signature syntax, but it still consumes the
same exact function bindings: it nests each leaf under its binding's module and
installs the body selected by the structured protocol kind.

## Canonical host functions

Within a protocol's env, each host function gets an explicit structured body —
the golden's source can use it without writing host glue. `HostFnBodyKind` in
[`src/shared/protocol.rs`](src/shared/protocol.rs) is the semantic source of
truth. [`CanonicalKind`](src/shared/canonical.rs) is only shared rendering
vocabulary used by typed-native adapters; names and emitted signatures never
select behavior. The index below documents those shared rendering shapes.

| Canonical kind | Signature (as written in the host) | Behavior |
| --- | --- | --- |
| `Print` | `fn print(s: Str) -> .;` | Write to stdout, no implicit newline. |
| `Eprint` | `fn eprint(s: Str) -> .;` | Write to stderr. |
| `Exit` | `fn exit(n: Int) -> !;` | Process exit; clamped into `0..=125`. |
| `ReadAsciiLine` | `fn read_ascii_line() -> Str \| .;` | Read the next normalized ASCII line from the runner process stdin. EOF returns `()`. |
| `StringLen` | `fn string_len(s: Str) -> Int;` | String byte length. |
| `StringSlice` | `fn string_slice(s: Str, start: Int, end: Int) -> Str;` | Half-open byte slice `[start, end)`; invalid ranges fail loudly. |
| `StringCodeAt` | `fn string_code_at(s: Str, index: Int) -> Int \| .;` | Byte value at `index`, or `()` when out of bounds. |
| `StringConcat` | `fn string_concat(a: Str, b: Str) -> Str;` | String `+` (JS) / `format!` (Rust). |
| `StringEq` | `fn string_eq(a: Str, b: Str) -> Bool;` | Character-by-character string equality. |
| `Loop` | `fn loop[S][R](step: S -> (S \| R), state: S) -> R;` | General-recursion driver; `step` returns the continue arm to loop or the exit arm to return. |
| `NumericToString` | `fn <kind>_to_string(v: <Kind>) -> Str;` | Stringify a numeric value (`String(v)` / `.to_string()` — using a JS `BigInt` for wide integers and shortest-round-trip rendering for floats). |
| `BoolToString` | `fn bool_to_string(v: Bool) -> Str;` | Stringify a bool. |
| `PrintI32` | `fn print_i32(v: I32) -> .;` | Write the decimal i32 value to stdout, no implicit newline. |
| `StringToInt` | `fn string_to_int(s: Str) -> Int \| .;` | Parse signed base-10 i32 text. Invalid or out-of-range input returns `()`. |
| `Arith` | `fn <op>_<kind>(a: <Kind>, b: <Kind>) -> <Kind>;` | Fixed-width integer arithmetic — `op` in `add`/`sub`/`mul`/`div`/`mod`, `kind` any `i8`…`i128` / `u8`…`u128`. Wraps at the kind's width (`wrapping_*` / `BigInt.asIntN`/`asUintN`); the wide (`i64`/`i128`/`u64`/`u128`) roles stay JS `BigInt`, the narrow roles narrow to JS `Number`. |
| `Cmp` | `fn <cmp>_<kind>(a: <Kind>, b: <Kind>) -> Bool;` | Fixed-width integer comparison — `cmp` in `eq`/`lt`/`leq`/`le`/`gt`/`geq`/`ge`. |
| `Array` | `type Array[T];` plus the canonical `array_*` primitive bodies. | Polymorphic mutable array. JS backs the type with a plain `Array`; Rust backs it with an `Rc<RefCell<Vec<T>>>` wrapper (`__ArrayCell<T>`) whose `PartialEq` is `Rc::ptr_eq` — reference-identity equality matches JS's default. Each exact protocol lists the array primitives it supplies. Negative sizes and out-of-bounds indices fail loudly. |
| `FloatArith` | `fn <op>_<kind>(a: <Kind>, b: <Kind>) -> <Kind>;` for a float kind (`f32` / `f64`). | The four IEEE-754 binary ops (`add` / `sub` / `mul` / `div`) applied directly — no width wrap. `mod` is excluded because `%` semantics differ across hosts. |
| `MakeScalar` / `ScalarOf` / `ScalarAs` / `ScalarIsTrue` | `type Scalar;` plus `make_scalar(Str, Str) -> Scalar`, `scalar_of_<kind>(<Kind>) -> Scalar`, `scalar_as_<kind>(Scalar) -> . \| <Kind>`, `scalar_is_true(Scalar) -> Bool`. | `dyn_load_prime`'s opaque host scalar. JS backs `Scalar` with a tagged `{k, v}` object; Rust backs it with a tagged `__Scalar` enum (`Clone + PartialEq`). For interpreter literals, the selected protocol maps the exact qualified host-type identity to its `HostTypeFixture`; `make_scalar` parses using that fixture's full `RoleFixture` vocabulary. A Kio role admits literal syntax but does not choose the native representation. |
| `Custom` | One of the closed protocol-specific `HostFnBodyKind` variants without a shared canonical renderer. | Every adapter handles each variant explicitly. Adding a new protocol-specific body makes the exhaustive adapter matches fail to compile until it is implemented. |

`role(R)` host types (`type String role(str);`, `type Int role(i32);`, …) remain exact host-selected types. The Rust and Haskell runners choose convenient canonical fixture types for their emitted exact associated members/families; production hosts may choose different types satisfying the backend contract. Roles are not host functions and add no runner-provided host record keys.

### Standard streams

All runners leave stdin, stdout, and stderr as ordinary process streams. Canonical `read_ascii_line()` consumes stdin one call at a time, so a test case that needs fixture input selects an exact protocol containing that host function and redirects the runner invocation from a checked-in file, for example `"$KIO_RUNNER" --protocol testapi-compute "out/$KIO_TARGET" < ../input.stdin`. Each call strips the line terminator (`\n`, and a preceding `\r` for CRLF), preserves empty lines, and returns a final unterminated line normally. EOF returns `()`. Non-ASCII input is a runner error.

## Adding or extending a protocol

A protocol's complete contract lives in
[`src/shared/protocol.rs`](src/shared/protocol.rs): add one
`RunnerProtocol` variant and accepted name, then resolve it to one
`ProtocolContract` containing its execution driver, exact host types, exact
host functions, and native fixtures.

Every applicable backend projects those same bindings into its native host
surface. Add a canonical body kind only when the operation is genuinely shared;
otherwise add an explicit protocol body variant and implement it exhaustively
in each applicable runner. Nominal host-type slots carry exact module+leaf
identities—never recover one from a leaf, fixture, or generated source. Add or
update the protocol registry tests and document the protocol family or
roundtrip here.

After changing shared protocols, run `sh ci/checks/hygiene/kio-test-runner-rs.sh`;
even backend-specific protocols can affect every runner's registry and
projection tests.

Keep `DYN_LOAD_PRIME_UNSUPPORTED_PROTOCOL_NAMES` in sync with the complete
dynamic adapter coverage, including its export script and host fixtures. Run
`sh ci/checks/repo-lint/dyn-load-prime-coverage.sh` from the repository root
when adding or extending a protocol; [TESTING.md § Test layers](../../../TESTING.md#test-layers)
defines the derived marker eligibility.

Goldens select a non-default protocol with `run.args`; the harness builds the
case, invokes the runner with those arguments, and appends `out/$KIO_TARGET`.
The case supplies no semantic host metadata.

### Adding a new canonical host function

1. Add or extend the structured `HostFnBodyKind` in
   `src/shared/protocol.rs`.
2. If typed-native adapters can share a rendering shape, add a
   `CanonicalKind` variant in `src/shared/canonical.rs`; otherwise handle the
   protocol body directly.
3. Update every applicable backend adapter with an exhaustive body mapping.
4. Update the canonical-function index above when shared rendering vocabulary
   changes.
5. Add a golden under
   [`test-data/goldens/00_success/`](../../../test-data/goldens/00_success/)
   exercising the new shape and select an exact protocol containing it.

## Runner build cache and compiler wrappers

The Rust, Go, Haskell, and Swift runners require `KIO_TEST_RUNNER_BUILD_CACHE_DIR`. The harness sets it to a corpus/target-scoped directory (`<cache-base>/<target>`); each runner stores content-addressed compiled artifacts there so a warm run can skip the compiler entirely. The orchestrators default `<cache-base>` to a machine-stable shared location (`$XDG_CACHE_HOME/kio/<suite>/`, else `~/.cache/kio/<suite>/`) so sibling worktrees reuse one cache; see `ai/topics/local-tools.md` § Compiler cache for the shared-location and override mechanics. The Rust runner caches **two** artifacts per golden — the package rlib and the driver bin — keyed by the `src/` tree, toolchain, target, and profile. The Go, Haskell, and Swift runners are **one-level**: a single final binary keyed by the toolchain identity, build flags, and the whole build tree's source bytes (the emitted package plus the synthesized driver). The Swift runner builds in two `swiftc` steps the way a real host does — the emitted package into its module (a **static** `lib<Ns>.a` + `.swiftmodule`), then a driver that `import`s and static-links it — but both steps run on one cache miss and stage one binary; static linking (no dynamic `.so`, no tempdir-relative rpath) is what keeps that binary relocatable enough to cache. All four share the generic cache machinery in `src/shared/build_cache/` via a per-backend adapter (`src/rust/rlib_cache/`, `src/go/bin_cache/`, `src/haskell/bin_cache/`, `src/swift/bin_cache/`).

Native compiler invocations share one Git-common scheduler resource with
top-level Cargo. `ci/run-tests.sh` supplies `KIO_CI_SCHEDULE_DIR`; it supplies
`KIO_CI_SCHEDULE_COMPILER_JOBS` only for a numeric fixed override. Runner setup
turns that state into an injected `CompilerAdmission` capability, using
adaptive admission when the capacity variable is absent. The build cache itself is
environment-agnostic. After the per-key lock and second cache probe prove that
this process will produce an artifact, the cache passes the capability to the
adapter; cache staging and publication consume no permit. Each adapter acquires
immediately around its compiler command, and Swift releases between its two
`swiftc` calls. Warm hits and same-key waiters consume no permit. A disabled
persistent cache still uses a fresh cache with the same admission capability.
Rust and Swift coexist paths and Java's direct `javac` route use the same
command boundary; Go and Haskell compilation flows through their admitted cache
adapters.

The native runners and `ci/schedule.sh --resource compiler` share the
`kio-ci-scheduler` crate's `CompilerAdmission` implementation and this protocol:

```text
<KIO_CI_SCHEDULE_DIR>/compiler/
  state.lock
  claims/
    <immutable-id>.lease
    <immutable-id>.pending | <immutable-id>.active
```

A client locks a zero-byte immutable lease; requested capacity, claim kind, and
FIFO ticket live in its identifier, while separate pending/active markers carry
state. The shared state lock serializes scans and transitions. Activation
creates active before removing pending, active wins an interrupted transition,
and stale cleanup removes only a lease proved unlocked. No client reads,
truncates, or renames a locked lease file. Contenders take the smallest fixed
capacity across live pending and active claims, so a pending smaller claim
stops new admission while existing producers drain. Omitted capacity uses
paced, best-effort CPU/memory feedback, bounded by live adaptive CPU ceilings
and explicit fixed capacities. This is not a per-command memory reservation
or an OOM guarantee. A numeric override retains its explicit fixed capacity.
See the scheduler's
[omitted-capacity policy](../kio-ci-scheduler-rs/README.md#omitted-compiler-capacity).

`AdmittedCommand` couples the permit to the actual spawn. On Unix the complete
compiler process tree inherits the private lease descriptor. On Windows the
child is created suspended, assigned to a kill-on-close Job Object before it
runs, and drained by active-process count. The generic explicit readiness hook
runs after admission but before either inheritance mechanism: it closes the
validated Unix lease inventory or breaks away from the Windows Job so a daemon
can outlive the probe without pinning capacity. `ci/infra/sccache.sh`, not the
Rust scheduler, owns sccache recognition and readiness. No readiness result is
cached between commands. Keep the shell facade, library clients, this
description, and the native process-tree self-tests in sync; the scheduler
crate's own README is the full cross-platform protocol reference.
The public capacity flag and explicit bypass contract are documented in
`ai/topics/local-ci.md` § Shared work, Cargo, and compiler admission.

**Runner-hygiene norm — no machine-shared state outside the cache root.** A runner writes machine-shared state only under `KIO_TEST_RUNNER_BUILD_CACHE_DIR` and the staging tempdirs the shared cache hands it. A toolchain's own default cache/state locations are *not* automatically inside that boundary: build caches (Go's `GOCACHE`), clang-style module caches (swiftc's `~/.cache/clang/ModuleCache`), and package-manager dirs each default to a machine-shared root and leak there unless the adapter redirects them. So every toolchain-default cache the adapter's compile touches is pinned per-compile under the staging tempdir (the Go adapter's `GOCACHE`, the Swift adapter's `-module-cache-path`) or into the cache root — never left at its default. A deliberate exception (a cache genuinely cheaper to keep machine-shared) is admissible only when documented as a row in `ai/topics/caches.md`. This is enforced doc-to-reality by [`ci/checks/orchestrators/runner-cache-hermeticity.sh`](../../../ci/checks/orchestrators/runner-cache-hermeticity.sh): it runs one tiny golden per cache-backed target with `XDG_CACHE_HOME` redirected to a scratch dir and fails on any file a runner writes there outside its own `kio/` cache root.

The cached artifacts are **path-neutral** so a populated cache reuses across worktrees: the Rust runner passes `--remap-path-prefix`, the Go runner passes `-trimpath`, and the Swift runner passes `-file-prefix-map` (swift's umbrella source-path remap). GHC has no source-path remap flag (no `--remap-path-prefix` / `-ffile-prefix-map` / `-trimpath`), so the Haskell runner strips no path — but because the runner asks for no debug info, GHC embeds no build path at any profile's `-O` level and produces byte-identical binaries across build directories, so cross-worktree reuse holds regardless. (Were a future change to enable debug info, GHC would embed source paths with no flag to strip them — a documented Haskell-runner limitation.) With no debug info swiftc likewise embeds no build path at any `-O` level even without the remap, so `-file-prefix-map` is belt-and-suspenders; swiftc does stamp a per-invocation random module hash into the binary (the `.swift_modhash` section), so Swift outputs are not byte-identical across compiles — but the cache content-addresses by *inputs* (the source bytes), so the modhash nonce does not impair cross-worktree reuse.

`KIO_TEST_RUNNER_BUILD_CACHE_SIZE` is optional at the runner level (unset = unbounded). When set, it caps the persistent runner build cache and prunes least-recently-used entries after successful writes. Plain bytes and binary `K` / `M` / `G` / `T` suffixes are accepted. The orchestrators set a default size-LRU cap so the shared cache self-bounds; the cap is per impl-target (each runner prunes only its own `<cache-base>/<target>/` subtree). See `ai/topics/local-tools.md` § Compiler cache.

`KIO_TEST_RUNNER_COMPILER_WRAPPER` is optional. The Rust runner treats it as an opaque `<wrapper> rustc ...` command, so setting `KIO_TEST_RUNNER_COMPILER_WRAPPER=sccache` accelerates cache misses without changing the cache key (which uses the real rustc identity). The explicit shell adapter is the sole sccache name recognizer: for its small declared tool vocabulary it accepts POSIX or Windows path separators, strips the native `.exe` suffix, and normalizes ASCII case; it sets `SCCACHE_IDLE_TIMEOUT=0` and registers the generic last-safe-point hook after compiler admission. A direct runner invocation outside that adapter must provide the equivalent environment and generic readiness-hook configuration explicitly; neither the Rust scheduler nor the runner infers it from a tool name. Restarting sccache while compilation is in flight would reopen the inherited-lease hazard described above. The JS, Go, Java, Haskell, and Swift runners have no `sccache`-compatible compiler invocation (`sccache` wraps C/C++/rustc-shaped compilers and rejects `go` / `javac` / `ghc` / `swiftc`) and ignore the variable; their build cache or direct compiler invocation is the only acceleration layer.

`KIO_DEBUG_TEST_RUNNER_COMPILER_OBSERVER` is an independent internal debug seam. Its value is one nonempty opaque executable path, never shell-split and never extended with inline arguments. Each actual native compile is nested beneath it: Rust cache misses use `<observer> <wrapper> rustc ...` when the cache wrapper is enabled and `<observer> rustc ...` otherwise; Go, Java, Haskell, Swift, and the distinct Rust/Swift coexist routes use `<observer> <compiler> ...`. The fully nested command is assembled before `CompilerAdmission::acquire_for`, so the outer observer receives the compiler lease and unchanged cwd, environment, stdio, and status behavior. Toolchain identity/version probes and runtime launches stay bare. Since the observer is applied only inside an artifact producer, warm cache hits and same-key waiters do not invoke it; it is absent from artifact keys and `meta.json`.

Readiness classification peels this layer only when argv[0] exactly equals the configured observer value, before any tool-basename recognition. It then inspects the exact inner old command, so even an observer executable whose basename is `rustc` cannot hide an inner sccache wrapper. The scheduler remains tool-agnostic; `ci/infra/sccache.sh` owns both this structural peel and sccache recognition.

`KIO_TEST_RUNNER_CACHE_DISABLE=1` disables persistent runner cache behavior and compiler wrappers; the Rust, Go, Haskell, and Swift runners use a fresh temporary cache for the invocation instead. It does not disable `KIO_DEBUG_TEST_RUNNER_COMPILER_OBSERVER`, so every resulting cold compile remains observable. Unset, empty, or `0` leaves caching enabled; any other value is a usage error.

## Compile profiles

The Rust, Go, Haskell, and Swift runners take a `--profile <name>` flag (also settable for a whole run via `KIO_TEST_RUNNER_PROFILE`; the flag wins). The three names are **backend-independent** — deliberately not `opt-level=N`, since the numbers differ per compiler — and each runner maps its selection to the compiler's real optimization flag. Debug info stays off on every profile.

| profile       | rustc            | ghc   | swiftc   | intent |
| ------------- | ---------------- | ----- | -------- | ------ |
| `unoptimized` | `-C opt-level=0` | `-O0` | `-Onone` | fastest compile |
| `default`     | `-C opt-level=1` | `-O0` | `-Onone` | CI default — each backend's current cheap level |
| `optimized`   | `-C opt-level=2` | `-O2` | `-O`     | slowest compile; most opt-sensitive coverage and most realistic runtime |

`default` preserves **each backend's current cheap level**. Rust's `opt-level=1` is near-free over `-O0` yet still runs the optimizer, catching codegen bugs an unoptimized build masks — so Rust's `default` sits one notch above `unoptimized`. Under ghc / swiftc, mild optimization (`-O1` / `-O`) costs real compile time, so `default` stays at the zero level (`-O0` / `-Onone`, coinciding with `unoptimized`) and `optimized` (`-O2` / `-O`) is the on-demand thorough pass.

Unset or empty `KIO_TEST_RUNNER_PROFILE` (and no `--profile`) means `default`; an unknown name is a usage error. Go has no optimization levels, so the profile is a **no-op** there — the flag is accepted and validated for parity but does not change the build. For the three optimizing runners the **selected profile feeds the artifact cache key** (the Rust profile name in the rlib/bin key; the ghc / swiftc `-O` flag in the one-level key), so a `default`-built artifact is never served for an `optimized` request and each profile compiles and caches independently (on Haskell and Swift `default` and `unoptimized` share the zero level, so they share a cache entry).

## Related

- [`TESTING.md`](../../../TESTING.md) — overall test-strategy doc.
- [`test-data/README.md`](../../../test-data/README.md) — golden layout and conventions.
- [`specs/backends/README.md`](../../../specs/backends/README.md) — per-backend FFI contracts the runner's emitted bodies honor.
