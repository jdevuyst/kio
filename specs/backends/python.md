# Python backend

**Host API stability:** `evolving`

Status policy: [`README.md` § Host API stability](README.md#host-api-stability).

**Family:** `typed-dynamic` (see [`README.md` § Language families](README.md#language-families)).

> **Known host-source compatibility caveat.** Python omits every declaration
> retained only by sealed signature history. Existing host source that imports
> or annotates with one of those removed generated names must delete or replace
> that reference. See
> [§ History-only generated names are not source-stable on Python](#history-only-generated-names-are-not-source-stable-on-python).

The `"python"` build target ([`specs/package.md` § Build target files](../package.md#build-target-files))
emits a Python module. This page describes the surface the host calls.

## Language version

Emitted code targets **Python 3.10+**. The gating surface is ordinary modern
Python module loading plus standard-library `json`, `types`, and
`importlib` support; no third-party package is required.

## Output layout

For a package named `<pkg>` whose `<pkg>.pkg.kio` carries a build block

```kio
build {
  cache "out/.kio-cache/";

  target python {
    out "out/python/";
  }
}
```

`kio build python` writes under the `out` directory:

- `<ns>.py` — the package's invocable Python module. Always exactly one.
- `<ns>/__init__.pyi` — the public entry point of a generated typed-stub
  package declaring `<ns>.py`'s public surface for pyright / mypy hosts
  (§ Typed stub).
- `<ns>/_kio_stub_<index>.pyi` — deterministic private declaration shards
  re-exported by `__init__.pyi`. Hosts import only `<ns>`; the shard modules
  are supporting artifacts rather than a second public API.

The stub package contains declarations only. With the output directory on the
import path, type checkers select `<ns>/__init__.pyi` as `<ns>`'s typed view,
while Python selects the sibling `<ns>.py` runtime module. Every build emits
the runtime and complete stub package together.

The file stem `<ns>` is the package's **namespace**: the kio package name by
default (a collision with a Python keyword that a stem could not be imported
under — `import <kw>` is a syntax error — uses `__kio_pkg_<escaped-name>`,
where source `_` is escaped as `_u`, so the default is
always importable), overridden by the `namespace` build-block key
([`specs/package.md` § Per-target keys](../package.md#per-target-keys)). An
explicit value must be a Python identifier matching `[A-Za-z_][A-Za-z0-9_]*`
and not a keyword; anything else is a build error. The package's public
factory is `create_<value-brand>`, keeping the fixed `create_` prefix.
An ordinary source-shaped namespace uses word casing with a lowercase
initial (`hello` → `create_hello`, `my_pkg` → `create_myPkg`). The handle
uses the corresponding title-cased brand, such as `MyPkg`.
Affixed source-shaped namespaces use the exact `KioPkg_...` brand, and other
explicit namespaces use `KioNs_<hex-UTF-8>` as on
[the shared branded naming rule](README.md#branded-naming); these framed brands keep their case in the factory.
The reserved default `__kio_pkg_...` namespace decodes to a `KioPkg_...`
brand (`class` → module `__kio_pkg_class`, factory `create_KioPkg_Class`).
The artifact stem remains the effective namespace, so two packages with
distinct namespaces load into one host program without a factory-name clash.

A stem that shadows a standard-library module name (a package named `json`) is
**not** mangled: which module the name resolves to is the host's to control
through `sys.path` order, and a host that loads the artifact by file path
(§ Loading protocol) bypasses the module namespace entirely, so a stdlib-name
collision is resolvable rather than fatal.

## Loading protocol

A host that wants to call into the package performs three steps in order.

### 1. Import the module

```python
import importlib.util

spec = importlib.util.spec_from_file_location("hello_kio", "out/python/hello.py")
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)
```

The module exposes a single public factory, `create_<value-brand>` — branded from the
module stem (§ Output layout), so `out/python/hello.py` exposes `create_hello`.
Importing the module performs no package I/O and calls no host functions; the
package comes online when the host invokes the factory.

### 2. Build the host record

The host argument is a namespace-like value that supplies one callable per
`host fn` the bridged modules declare. The record is **namespaced by module**:
each module contributes a nested object. A path without source `_` is keyed by
the module path with `/` replaced by `_`, keeping lowercase components. A path
containing `_` instead uses `KioModule_<exact-path>`: apply word casing
componentwise, then escape remaining `_` as `_u` and `/` as `_s`.
Thus `foo/bar` uses `foo_bar`, while `foo_bar` uses `KioModule_fooBar`.
Callable leaves use word casing (`do_work` → `doWork`). A `dict` with the same keys is also
accepted.

```python
import types

host = types.SimpleNamespace(
    KioHostIn_hello_String=lambda value: value,
    KioHostOut_hello_String=lambda value: value,
    hello=types.SimpleNamespace(
        print=lambda s: (print(s, end=""), None)[1],
    ),
)
```

### 3. Instantiate and invoke

```python
pkg = mod.create_hello(host)
pkg.hello.KioModule_main.main()
```

## Package API

`create_<value-brand>(host=None)` — the branded factory (§ Output layout) — returns a
namespace tree exposing the bridged modules' `pub` items under a role-framed
form of their declaring module path. The root module uses word casing,
each nested module uses `KioModule_<exact-component>`, and each public newtype
handle uses `KioType_<exact-component>`. An exact component applies word casing
before escaping remaining `_` as `_u`. Function leaves use word casing. A multi-value-group
export takes every group's parameters flat in one call — the exported callable
applies the internal curried layers group by group
([`README.md` § Function-type FFI canonicalization](README.md#function-type-ffi-canonicalization)).

```python
# module hello/main declares `pub fn main() -> .;`
pkg.hello.KioModule_main.main()
```

Polymorphic exports erase type arguments at the FFI, per
[`README.md` § Type erasure at the FFI](README.md#2-type-erasure-at-the-ffi).
A `pub fn id[T](x: T) -> T` is called as `pkg.m.id(value)`.

For each explicit `pub newtype` in a bridged module, the package exposes a
namespace object under `KioType_<exact-component>` whose attributes are the
newtype's public constructor and projector members. The constructor accepts the payload's
FFI shape and returns the newtype's FFI shape; the projector accepts that
newtype shape and returns the payload shape. An existential projector instead
returns its ordinary CPS callable, whose continuation receives the opened
payload in its FFI shape. These are the same conversions used by `pub fn` and
`host fn` boundaries, so a value from a public constructor can be passed
directly to any function accepting that newtype. A private member is absent.

A public newtype whose constructor and projector are both public has the
structural dictionary shape in § FFI surface. Every other public newtype uses
a declaration-specific nominal carrier with no public payload representation.
A constructor-only namespace exposes only a constructor that produces that
carrier; a projector-only namespace exposes only a projector that consumes it;
a namespace with neither member public is empty. Hosts can carry such a value
between calls that name the exact newtype, but cannot use a raw payload in its
place. Two declarations remain distinct even when their payload types are
equal.

## Host record contract

The host record mirrors the bridged modules' `host fn` surface: one nested
namespace per module that declares host functions, and one callable attribute
or mapping entry per `host fn`. It also realizes each exact role-bearing host
binding through the declaration-keyed conversion pair described below. Those
conversions are the Python representation choice for a declared host type,
not extra Kio `host fn` items; there is no allocator hook or ambient runtime.

For each `host fn <name>(p0: <args...>) -> <ret>;` declared in module `<m>`
(whose namespace key is `<NS>` — the readable or exact module key described
above):

- `host.<NS>.<name>` or `host["<NS>"]["<name>"]` is called synchronously with
  positional arguments.
- Argument and return values follow the FFI surface below.
- Extra host fields are tolerated.

Every nullary `host type` is an independent public type parameter on the
generated host protocol, package handle, namespace classes, and factory. Two
declarations remain two parameters even when they carry the same role or the
host chooses the same concrete Python type for both. A roleless declaration
adds no host member; its selected value crosses unchanged.

Following the shared [declaration-keyed conversion-adapter
rule](README.md#declaration-keyed-conversion-adapters), for each nullary
`host type T role(r)`, the host record additionally supplies two root-level
methods:

- `KioHostIn_<identity>(value: T) -> <role-type>` converts the exact public host
  value to the runtime's private role representation.
- `KioHostOut_<identity>(value: <role-type>) -> T` converts that private
  representation back to the exact public host value.

When every word-cased component is ASCII-alphanumeric, `<identity>` is those
components joined by `_` (`api.Count` → `api_Count`,
`data_api.Native_count` → `dataApi_NativeCount`). A remaining affix
underscore, another unspellable component, or the reserved leading `V1` class
selects the exact qualified-type-name frame from § Item naming. This choice is
per declaration and does not change when another declaration is added. A host
that selects the ordinary Python role type may implement both operations as
identity functions. A host may instead select a declaration-specific class,
including selecting different classes for two declarations with the same
role, without changing the runtime's primitive role operations.

A parameterized declaration such as `host type Box[T];` contributes the
module-level carrier `KioHostType_<frame>[T]`. `from_native(object)` creates
that exact carrier application and `to_native() -> object` recovers the
host-owned native value. These are the only deliberately opaque endpoints;
callable signatures keep the exact applied carrier rather than widening it to
`object`. Runtime subscription is erased — `KioHostType_<frame>[T]` evaluates
to the declaration's carrier class — while the typed stub preserves `T` for the
type checker.

`create_<value-brand>` validates that every declared `host fn` and every live role
conversion exists. A missing function raises
`RuntimeError("missing host item: <NS>.<name>")`; a missing conversion reports
its complete `KioHostIn_<identity>` or `KioHostOut_<identity>` name. A present but
non-callable value fails at first call with Python's ordinary callable error.

## Deprecated host items

This section instantiates the cross-backend removal-side idiom in
[`README.md` § Deprecated host items](README.md#deprecated-host-items) for
Python.

The Python backend is removal-tolerant: removing a `host fn` means the current
package no longer validates or calls that field, so a host that still supplies
the old field keeps working. Both the typed-stub package and `.py` are live-only: a sealed
removed signature contributes no host root, adapter, parameterized-host or
public-newtype carrier, method, or transitive support declaration. Adding a
`host fn` or role-bearing host type is a construction-time contract change:
hosts must supply the new callable or conversion, or `create_<value-brand>` raises the
missing-host-item `RuntimeError` described above.

### History-only generated names are not source-stable on Python

The Python 3.10 language floor has no standard-library deprecation marker for
declarations in a generated type stub. Retaining a history-only stub
declaration would therefore violate the cross-backend rule that every retained
declaration is visibly deprecated. Python omits the complete history-only
transitive closure from both artifacts instead. An existing host that imported
a removed generated type or used it in an annotation must delete that import
or replace that annotation with the current live type. Extra runtime host
fields remain valid because Python host records are structural and tolerate
them; only source that names a removed generated declaration requires an edit.

The omission sites are the live-origin and execution filters in
`PreparedPythonStub` in
[`stub.rs`](../../kio-rs/src/backends/python/stub.rs) and
`python_boundary_ir` in
[`serialized_runtime_ir.rs`](../../kio-rs/src/backends/serialized_runtime_ir.rs);
their comments cite this subsection in turn.

## Boundary semantics

The Python backend inherits the cross-cutting properties in
[`README.md`](README.md): synchronous calls, type erasure at the FFI,
package isolation, exception propagation, well-foundedness inheritance, and
behavioral additivity. Backend-specific notes:

- Host exceptions propagate through package calls unchanged. Package runtime
  guards surface as ordinary Python exceptions.
- **Facade topology and execution provenance** ([`README.md`](README.md#facade-topology-and-execution-provenance))
  — the compiler prepares the callable topology, semantic slot keys, binder
  scopes, nominal dependencies, and execution actions once. The `.py` embeds
  that prepared plan and the runtime consumes it directly; it does not scan
  declarations or reconstruct boundary call grouping. The typed-stub package, host
  validation, export tree, and runtime conversions consume the same prepared
  site inventory.
- **Open-world property** ([`README.md`](README.md)) — exact host binding and
  nominal names depend only on complete qualified declaration identities;
  callable and shape names depend only on their prepared site identities and
  semantic keys. No ambient declaration lookup, encounter order, or occupied
  Python name set participates. Adding an unrelated declaration therefore
  cannot rename an existing public binding, change an existing conversion, or
  alter a prepared callable's topology.
- Values with a public structural Python shape have structural equality by that
  shape; object identity is not a contract. Equality of nominal newtype carriers
  is not part of the host contract.

## FFI surface

The host selects the public Python type for every nullary host declaration.
For a declaration carrying `role(r)`, its exact adapter pair converts that
selected type to and from the following private runtime representation:

| Role | Python shape |
| --- | --- |
| `role(str)` | `str` |
| `role(bool)` | `bool` |
| `role(i8)` … `role(i128)` | `int` |
| `role(u8)` … `role(u128)` | non-negative `int` |
| `role(f32)`, `role(f64)` | `float` |

Ensuring an adapter result belongs to the declared role's range is the host's
responsibility. The package does not defensively coerce out-of-range or
wrong-shaped private role values. A roleless nullary declaration uses its
exact selected Python type unchanged.

A parameterized host declaration `F[A, ...]` uses
`KioHostType_<frame>[A, ...]`. The exact declaration and every applied type
argument remain visible in the stub. The carrier's private native value is not
part of the typed callable surface; explicit `from_native` / `to_native`
operations are the host integration boundary.

Structural products cross as dictionaries keyed by right-spine slot names.
The key preference is the bare newtype name, then the canonical
fully-qualified spelling, then the positional fallback `"_<index>"`.
Named keys word-case each source component, retaining `/` and `.` as route
delimiters (`data_api/model.Native_token` → `"dataApi/model.NativeToken"`);
positional keys remain unchanged.
Structural sums cross as a one-key dictionary whose key is the selected
variant's slot key. An explicit newtype whose constructor and projector are
both public crosses as `{ "<word-cased TypeName>": payload }` at every host-facing
boundary, including exported functions, host functions, and its public
members. The host builds and inspects this dictionary directly. If either
member is private, the newtype instead crosses as a declaration-specific
nominal carrier: no payload dictionary, field, or alias is public, and only a
value carrying that exact newtype identity is accepted on the way back into
the package. The namespace exposes whichever constructor or projector member
is public; those wrappers perform the same boundary conversion as an ordinary
public function.

The carve-outs are:

- Unit (`.`) crosses as `None`.
- Function values cross as Python callables.
- Bottom (`!`) has no value; an attempted absurd elimination raises a
  Python exception.

## Typed stub

Beside the runtime `<ns>.py`, `kio build python` emits a `<ns>/` typed-stub
package — the Python analogue of the TypeScript backend's `.d.ts`. Its public
`__init__.pyi` re-exports declarations from private `.pyi` shards; every
declaration has one defining shard, so cross-shard references retain one exact
type identity. The package contains no runtime code and types exactly the
public surface `create_<value-brand>` exposes, so a host that runs a static type checker
(pyright or mypy) checks the embedding at author time. The generated stubs carry
only targeted Pyright suppressions: `reportInvalidTypeVarUse` where Pyright
would otherwise widen an exact single-use Kio binder, and
`reportPrivateUsage` on the exact import or annotation line that transports a
deliberately private shard module or support declaration between sibling
shards. Pyright and mypy still check the transported type and the rest of the
surface. Type checkers resolve the stub package as the module's typed view;
a dynamic host that runs no checker selects and executes the sibling `.py`.
The stub is truthful to the runtime: every declaration matches a live value the `.py`
actually produces. Exact declaration bindings, generic applications, callable
stages, and constructor relationships remain typed even though the evaluator
body is dynamic.

Every name is branded off the namespace `<ns>` (§ Output layout): `<Handle>`
is its public title brand, `<Handle>Host` the host contract, `create_<value-brand>` the
factory.

- `def create_<value-brand>(host: <Handle>Host[...]) -> <Handle>[...]: ...` — the
  factory carries the same exact nullary host bindings from the input protocol
  to the returned handle. The `| None = None` default is present exactly when
  the package declares neither a `host fn` nor a live role conversion; when
  either is present the argument is required because an absent host makes
  `create_<value-brand>` raise the missing-host-item `RuntimeError` (§ Host record
  contract).

  When that default is present and a roleless exact binding has no host member
  from which to infer its selection, an expected result annotation selects it:
  `pkg: <Handle>[Token] = create_<value-brand>()`. A bare call has no evidence from
  which a type checker could infer that exact choice.
- `<Handle>` is a class whose attribute tree mirrors the package surface
  (§ Package API): role-framed nested module and public-type attributes, a
  method per `pub fn`, and a nested class per `pub newtype` carrying its
  public constructor / projector methods. The stub and runtime resolve the
  same selectors.
- `<Handle>Host[...]` is a `typing.Protocol` parameterized by one exact
  `KioHost_<frame>` binding per nullary host declaration. It has one read-only
  property per declaring-module namespace `<NS>` (§ Host record contract),
  each typed as a nested `Protocol` whose methods are that module's `host fn`s,
  plus the exact role conversion methods. The property is read-only so a
  host's own concrete class structurally matches it.

Each boundary value is typed at its § FFI surface Python shape: an exact
nullary host declaration at its `KioHost_<frame>` type parameter; a saturated
parameterized host declaration at `KioHostType_<frame>[...]`; a site type
binder at its exact `typing.TypeVar`; unit as `None`; a function or rank-N
value as an exact `typing.Protocol` callable; a structural product as a
`typing.TypedDict` keyed by its right-spine slots; a structural sum as the
union of its one-key `TypedDict` arms; and an explicit newtype with both
members public as a one-key generic `TypedDict`. Every other public newtype is
a declaration-specific nominal generic class with no payload member. A
newtype namespace's constructor and projector use those same exact payload and
newtype types (§ Package API); an existential projector retains its generic
CPS callable relationship.

`object` occurs only at an explicit parameterized-host-carrier native endpoint
and at the `__getattr__` escape for a Python hard keyword. A spellable exact
callable position never widens a host declaration, generic application,
newtype, structural shape, rank-N callable, or scoped binder to `object`.

A surface name that is a Python hard keyword cannot be a class-member
spelling; per § Item naming it is reached with `getattr`, so the containing
class carries a `__getattr__(self, name: str) -> object` and the member is
not declared — the access stays typed `object` rather than misdeclared.

## Higher-kinded types

Python is type-erased at runtime. Kio's higher-kinded type constructor
identity therefore has no separate Python value representation. The
`typed-dynamic` family keeps that dynamic body while its natural-exact static
skin preserves the constructor relationship described in
[`README.md` § Higher-kinded types](README.md#higher-kinded-types).
The typed stub nevertheless preserves the exact type relation. A saturated
known declaration is applied directly — for example
`KioHostType_<frame>[A]` or `KioNewtype_<frame>[A]`. Only an unsaturated
higher-kinded occurrence uses a declaration-owned `KioHostMk_<frame>` or
`KioNewtypeMk_<frame>` witness and `KioApplyN[Constructor, ...]`; ordinary
first-order applications do not pass through that witness machinery.
Lift/project operations are identity at the erased body level, and
polymorphic newtype payloads store and project the complete staged callable.
The bound types have no Python value representation, but every binder of the
stored first-class value remains one hidden callable stage. A work-free stage
returns the next callable, while a stage with computation performs it first.
A direct known call may compact adjacent leading type applications. The body's
constructor and projector are identities around the complete callable, and a
public boundary wrapper performs the ordinary function adaptation. Only value
arguments appear at the facade, as required by
[`README.md` § 2](README.md#2-type-erasure-at-the-ffi).

## Item naming

Python uses the [shared source-component word casing](README.md#source-derived-public-names)
(`do_work` → `doWork`, `Native_token` → `NativeToken`).
Package exports use word casing for the root module and callable leaves, use
`KioModule_<exact-component>` for nested modules, and use
`KioType_<exact-component>` for public newtype handles. Exact components apply
word casing before escaping remaining `_` as `_u`. Host-record namespace keys use module
paths with `/` replaced by `_` when the path contains no source underscore,
and `KioModule_<exact-path>` otherwise. When a Kio name cannot be written with
Python dot syntax, hosts use `getattr` / mapping access:

```python
getattr(pkg.someModule, "class")
```

Exact host and nominal bindings use a complete qualified-type-name frame:
`V1_M<count>_C<len>_<segment>...N<len>_<leaf>`. `<count>` is the number of
module components; each component is word-cased before its UTF-8 byte length
is calculated. For example, `data_api.Native_count` is
`V1_M1_C7_dataApiN11_NativeCount`, and `testapi.Count` is
`V1_M1_C7_testapiN5_Count`. The frame appears in `KioHost_<frame>`,
`KioHostType_<frame>`, fallback role-adapter names, and public newtype carrier
names. It depends only on the exact declaration identity, not
on source encounter order or which other declarations occupy the module.
The readable frame conversion does not rewrite raw nominal identity or
opaque structural identity encodings.

## Worked example

Given a package `hello` with:

```kio
// hello.pkg.kio
package hello;

build {
  cache "out/.kio-cache/";

  target python {
    out "out/python/";
  }
}

bridge {
  hello;
  hello/**;
}
```

```kio
// hello.kio
module hello;

host type String role(str);
host fn print(s: String) -> .;
```

```kio
// hello/main.kio
module hello/main;

import hello(String, print);

pub fn main() -> . { print("hello\n"(String)) }
```

A Python host loads and calls it as:

```python
import importlib.util
import types

spec = importlib.util.spec_from_file_location("hello_kio", "out/python/hello.py")
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)

host = types.SimpleNamespace(
    KioHostIn_hello_String=lambda value: value,
    KioHostOut_hello_String=lambda value: value,
    hello=types.SimpleNamespace(print=lambda s: (print(s, end=""), None)[1]),
)
pkg = mod.create_hello(host)
pkg.hello.KioModule_main.main()
```
