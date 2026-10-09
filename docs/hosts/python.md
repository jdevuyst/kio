# Hosting Kio in Python

**Host API stability:** `evolving`

Status policy: [Host API stability](../../specs/backends/README.md#host-api-stability).

This guide walks through compiling a Kio package to Python and
calling into it from a Python host. The authoritative contract is
[`specs/backends/python.md`](../../specs/backends/python.md); this guide is the
narrative companion.

> **Known host-source compatibility caveat.** Python omits every declaration
> retained only by sealed signature history. Existing host source that imports
> or annotates with one of those removed generated names must delete or replace
> that reference. See the Python contract's
> [history-only generated names section](../../specs/backends/python.md#history-only-generated-names-are-not-source-stable-on-python).

## The build target

A package emits to Python by declaring a `python` target in `<pkg>.pkg.kio`:

```kio {variant=package}
package greeter;

build {
  cache "out/.kio-cache/";

  target python {
    out "out/python/"
  }
}

bridge {
  greeter;
  greeter/**
}
```

`kio build python` writes the runtime module and its typed-stub package under the
target output directory:

```text
out/python/greeter.py
out/python/greeter/__init__.pyi
out/python/greeter/_kio_stub_0000.pyi
out/python/kio_static_v1/core.pyi
```

`__init__.pyi` is the public typed entry point. The `_kio_stub_*` files are
private supporting shards; host code imports `greeter`, never a shard.
The generated `kio_static_v1/` directory holds shared type definitions, with
additional files as the package needs them. Keep the complete output tree when
distributing typing support. Each build supplies everything its stubs need;
there is no extra package to install and no shared runtime to load. A dynamic
host can distribute just `greeter.py`. Names matching `kio_static_v[0-9]+`
(with one or more ASCII digits) belong to this generated support and cannot
be an explicit Python target namespace.

The emitted module targets Python 3.10+ and uses only the standard library.

### Generated documentation

Documented package functions expose their Kio prose through `__doc__`, `help()`,
and `pydoc`. Typed stubs also carry docstrings on host and package methods,
public newtype classes, and parameterized host-type classes. Host types that
are only generic parameters have no separate docstring. Your own host callable
objects are not modified. See
[boundary documentation](../../specs/backends/python.md#boundary-documentation).

## Loading the package

A Python host brings the package online in three steps.

### 1. Import the generated module

```python
import importlib.util

spec = importlib.util.spec_from_file_location("greeter_kio", "out/python/greeter.py")
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)
```

The module exposes `create_greeter` — the factory is branded from the module
stem (the package namespace, `greeter` here). Importing it does not call host
functions or run package code.

The module stem and factory component are distinct: namespace `my_pkg` emits
`my_pkg.py` with `create_myPkg`, not `create_my_pkg`. The fixed prefix remains
`create_`; exact `KioPkg_...` and `KioNs_...` brands keep their capitalization.
See [the namespace rules](../../specs/backends/python.md#output-layout).

### 2. Build the host record

The host record supplies one callable per `host fn` the bridged modules
declare and one declaration-keyed conversion pair per role-bearing host type.
Host functions are namespaced by module. A path without source `_` is
formed with `/` replaced by `_`, keeping lowercase components; a path containing
`_` uses `KioModule_...` after word casing and escaping remaining `_` as `_u`
and `/` as `_s`. Thus `foo/bar` and `foo_bar` use distinct `foo_bar` and
`KioModule_fooBar` attributes. Callable leaves use word casing
(`do_work` → `doWork`). For module `greeter`, the host capability lives under `greeter`:

```kio {variant=module}
module greeter;

host type String role(str);

host fn print(p0: String) -> .;
```

```python
import sys
import types

def write_stdout(s):
    sys.stdout.write(s)
    return None

host = types.SimpleNamespace(
    KioHostIn_greeter_String=lambda value: value,
    KioHostOut_greeter_String=lambda value: value,
    greeter=types.SimpleNamespace(print=write_stdout),
)
```

The two root-level adapter names contain the complete identity of
`greeter.String`. They convert the host-selected public value to and from the
runtime's private `str`. Identity lambdas are sufficient when the host selects
`str`; a host may instead use its own `String` class. Two Kio declarations
carrying `role(str)` still have different adapter names and may select
different Python classes.

`dict` records with the same namespaced keys work as well.

### 3. Instantiate and invoke

```python
pkg = mod.create_greeter(host)
pkg.greeter.KioModule_main.main()
```

The returned package surface is a namespace tree. A root module uses word
casing (`my_module` → `myModule`); nested modules use `KioModule_<exact-component>`,
and public newtype handles use `KioType_<exact-component>`. Exact components
apply word casing before escaping remaining underscores; outer underscore
runs survive word casing. An export `main` from module
`greeter/main` is reached as `pkg.greeter.KioModule_main.main`.

## Worked example

`greeter.pkg.kio`:

```kio {variant=package}
package greeter;

build {
  cache "out/.kio-cache/";

  target python {
    out "out/python/"
  }
}

bridge {
  greeter;
  greeter/**
}
```

`greeter.kio` declares the host capability:

```kio {file}
module greeter;

host type String role(str);
host fn print(p0: String) -> .;
```

`greeter/main.kio` imports it and exports `main`:

<!--kio {file}
package greeter;

build {
  cache "out/.kio-cache/";

  target python {
    out "out/python/";
  }
}

bridge {
  greeter;
  greeter/**;
}
-->

<!--kio {harness=greeter_python_main file placeholder="__SNIPPET__"}
module greeter/main;

import greeter(String, print);

__SNIPPET__
-->

```kio {@greeter_python_main placeholder={"module greeter/main;":"","import greeter(String, print);":""}}
module greeter/main;

import greeter(String, print);

pub fn main() { print("hello from kio\n") }
```

Python host:

```python
import importlib.util
import sys
import types

spec = importlib.util.spec_from_file_location("greeter_kio", "out/python/greeter.py")
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)

def write_stdout(s):
    sys.stdout.write(s)
    return None

host = types.SimpleNamespace(
    KioHostIn_greeter_String=lambda value: value,
    KioHostOut_greeter_String=lambda value: value,
    greeter=types.SimpleNamespace(print=write_stdout),
)

pkg = mod.create_greeter(host)
pkg.greeter.KioModule_main.main()
```

Running the host prints:

```text
hello from kio
```

## Type-checked hosting

Beside `out/python/greeter.py`, `kio build python` emits
`out/python/greeter/` — a typed-stub package declaring the package's surface (see
[`specs/backends/python.md` § Typed stub](../../specs/backends/python.md#typed-stub)).
A host that runs pyright or mypy checks its embedding against it: the factory,
the exported surface, and the host contract are all typed. Importing the module
with `out/python` on the path gives type checkers the stub package's view and
Python the sibling `greeter.py` runtime, with no extra setup.
Independently generated packages reuse compatible static definitions, so typed
callbacks can pass between them through their ordinary package imports.

The stub exposes `create_greeter`, the generic package handle type `Greeter`,
and the generic host protocol `GreeterHost`. A host selects one Python type for
each exact nullary Kio host declaration. Here `GreeterHost[str]` selects `str`
for `greeter.String`; the matching adapter pair is therefore identity. The
factory carries that same selection into `Greeter[str]`, so the host contract,
callbacks, exports, and newtype surfaces cannot drift apart:

```python
from __future__ import annotations

import sys
from typing import TYPE_CHECKING

from greeter import create_greeter

if TYPE_CHECKING:
    from greeter import GreeterHost  # pyright resolves the stub package

class GreeterCapabilities:
    def print(self, p0: str) -> None:
        sys.stdout.write(p0)

class Host:
    def __init__(self) -> None:
        self.greeter = GreeterCapabilities()

    def KioHostIn_greeter_String(self, value: str) -> str:
        return value

    def KioHostOut_greeter_String(self, value: str) -> str:
        return value

def run(host: GreeterHost[str]) -> None:
    pkg = create_greeter(host)
    pkg.greeter.KioModule_main.main()  # checked: nullary, returns None

run(Host())
```

pyright flags a wrong host shape (a missing `greeter.print` or exact adapter, a
mistyped argument) or a misused export (wrong arity, a non-existent module
path) at author time. The stub is not required — a dynamic host that runs no
checker loads the same `.py` unchanged.

The generated stub and runtime contain current declarations only. Removing a
host function remains runtime-compatible: an existing host may keep the extra
field because Python host records are structural. Python 3.10+'s standard
library has no type-stub deprecation marker, however, so Kio cannot retain an
old generated root, adapter, carrier, method, or support name while marking it
deprecated. If existing host source imports one of those removed generated
types or names it in an annotation, delete that import or replace the
annotation with the current live type. See the Python contract's
[history-only generated names caveat](../../specs/backends/python.md#history-only-generated-names-are-not-source-stable-on-python).

## FFI shapes at a glance

Keep shared tuple dictionary fields and nested Kio data unchanged while a retained
value or closure can still use them, even after a call returns. Construct a
changed tuple instead. A host-managed mutable `Box` may change through its
declared `get` and `set` host functions while the tuple keeps the same box.
See [shared data stays unchanged](../../specs/backends/README.md#10-shared-data-stays-unchanged).

Each nullary host type is an exact generic binding selected by the host. A
role-bearing binding adapts to a private runtime `str`, `bool`, `int`, or
`float`; the public value can be that primitive or a declaration-specific host
class. For `Box[T]` declared in module `api`, import
`KioHostType_api__Box` to store and recover your native value:
`KioHostType_api__Box[int].from_native(value)` constructs it, and
`KioHostType_api__Box[int].to_native(box)` reads it back. The generated type
`KioApply1[KioHostMk_api__Box, int]` preserves that exact application in
callable signatures; its type argument adds no runtime call argument.

The readable names are aliases of the exact declaration, so they add no
conversion or new class. Newtype annotations can similarly use
`KioNewtype_api__Wrapper` and its `KioNewtypeMk_api__Wrapper` constructor
index. Construct or inspect those values only through the package's public
newtype members. The [type-view reference](../../specs/backends/python.md#readable-type-views)
gives the parameter order and the escaping rule for affixed names.

When a package needs no runtime host but exposes a roleless exact type, select
that type through the expected handle annotation, for example
`pkg: Library[Token] = create_library()`. The annotation supplies the static
choice; it adds no runtime argument.

Unit is `None`. A product is `{"left": a, "right": b}`, and a sum is
`{"tag": "left", "value": a}` or `{"tag": "right", "value": b}`.
Longer products and sums nest on the right. These shapes stay the same when
you pass a value through a generic function or keep it inside a host-owned
container; Kio does not inspect that container.

Supply values matching the declared types and layouts, including callback
arguments and results and values kept in generic or opaque containers. Role
values must have the required range and shape; Kio does not defensively coerce
them. Typed stubs help check host code, but the runtime does not validate every
incoming value or promise fresh structural dictionaries. See
[host values satisfy their declared types](../../specs/backends/README.md#11-host-values-satisfy-their-declared-types)
and the [FFI surface](../../specs/backends/python.md#ffi-surface).

Reading a tuple or choice field must return the same value without side
effects. This field rule does not turn calls to declared mutable host-state
operations into passive reads.

When a generic result has an index absent from its arguments, select that
index with an expected type using the generated public types. For example,
after importing the generated sum arms as `Left` and `Right` and the host
constructor marker as `BoxMarker`, annotate a `putLeft(73)` result as
`KioApply1[BoxMarker, Left[int] | Right[str]]`. You can then assign that value
to an equivalent host-defined `TypedDict` union under the same `KioApply1`.
Using the independently defined union directly as the call's expected type
may not give Pyright enough information to infer the absent index.

Every explicit newtype is a sealed value with no public payload field. Use its
public constructor and projector methods instead of building a dictionary.
A constructor-only namespace can create that value, a
projector-only namespace can inspect a value received from Kio, and a namespace
with neither member public is empty. In every case, values from the public
member wrappers pass directly to ordinary exported functions that name the
same newtype.

For a newly written callback, use the generated module's `kio_lift` helper.
The callback receives one argument: `None` for Unit, or one product dictionary
for a product domain. In type annotations, `KioFn[Domain, Result]` is an alias
for `typing.Callable[[Domain], Result]`: one positional-only argument and its
result. Write a function or callable object, rather than subclassing `KioFn`.
A callback returned by Kio already has this interface;
pass it on directly. Named package methods and host-record methods keep their
source parameter lists, so a named Unit-only method remains a no-argument call.

A generic function value has one `advance()` for each type parameter. Advancing
selects that parameter and returns the next stage; it does not call the value
function. An untyped host supplies an ordinary class with an `advance` method
and wraps its instance with `kio_lift_stage`; no generated superclass is needed.
A typed host subclasses the readable source view for that argument, such as
`KioViewHost_api__applyPoly_Arg0Source` for `api.apply_poly`'s first argument.
Its `advance` returns the selected `KioFn[T, T]` (for an identity callback).
Python 3.10-compatible generic return annotations can require the narrow
`# pyright: ignore[reportInvalidTypeVarUse]` on that method; the returned
callable's input and result still retain the selected type.
The lift helpers do not force a stage or call your callback early. Hosts
implementing their own callable objects follow the same one-domain call or
`advance()` interface in the
[callable contract](../../specs/backends/python.md#ffi-surface).

For a structural annotation, use the readable argument/result view instead of
an encoded shape name: for example,
`KioViewCtor_api__Shadow__makeShadow_Arg0[BoxString]` describes the input to
`pkg.api.KioType_Shadow.makeShadow`. Returned or nested generic callbacks have local
`_StageNSource` views. The [type-view naming rule](../../specs/backends/python.md#readable-type-views)
explains the exact names and parameter order. These annotations are optional;
they do not add arguments to calls or change dynamic hosting.

See [`specs/backends/python.md`](../../specs/backends/python.md) for the full
boundary contract and [`specs/backends/README.md`](../../specs/backends/README.md)
for cross-cutting rules shared by every backend.
