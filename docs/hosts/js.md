# Hosting Kio in JavaScript

**Host API stability:** `evolving`

Status policy: [Host API stability](../../specs/backends/README.md#host-api-stability).

This guide walks through compiling a Kio package to JavaScript and calling into it from a JS host. The authoritative contract is [`specs/backends/js.md`](../../specs/backends/js.md); this guide is the narrative companion.

## The build target

A package emits to JavaScript by declaring a `js` target in `<pkg>.pkg.kio`:

```kio {variant=package}
package greeter;

build {
  cache "out/.kio-cache/";

  target js {
    out "out/js/"
  }
}

bridge {
  greeter;
  greeter/**
}
```

`kio build` or `kio build js` writes the package surface under the target output directory, for example:

```text
out/js/greeter.js
```

The emitted code targets ECMAScript 2020: ES modules plus `BigInt` literals for wide integers.

### Generated documentation

Kio docs appear as JSDoc on exported function and newtype properties. The
factory also lists documented host requirements and package functions by their
property paths. Erased host types have no separate declaration. At-signs and
comment terminators use visible backslash escapes instead of becoming JSDoc
instructions. See [boundary documentation](../../specs/backends/js.md#boundary-documentation).

## Loading the package

A JS host brings the package online in three steps.

### 1. Import the factory

```js
import { createGreeter } from './out/js/greeter.js';
```

The single named export is `createGreeter` — the factory, its name branded from the package namespace `greeter` (the default; a `namespace` target key would override it, and the emitted file would be named to match). Importing the module does nothing observable; the package only comes online when you call the factory.

Artifact names and public brands are distinct: namespace `my_pkg` keeps
`my_pkg.js` but exports `createMyPkg`. Affixed source names and explicit
non-source namespaces have distinct `KioPkg_...` and `KioNs_...` brands;
see [the namespace rules](../../specs/backends/js.md#output-layout).

### 2. Build the host record

The host record is a plain JS object that supplies one callable per `host fn`
the bridged modules declare. The record is **namespaced by module**. A path
without source `_` replaces `/` with `_`, keeping lowercase components;
a path containing `_` uses `KioModule_...` after word casing and escaping
remaining `_` as `_u` and `/` as `_s`. Thus `foo/bar` and `foo_bar` use the
distinct keys `foo_bar` and `KioModule_fooBar`. Callable leaves also use
word casing (`do_work` → `doWork`). A `host fn print` in module `greeter`
is supplied as `greeter.print`:

```kio {variant=module}
// greeter.kio
module greeter;

host type String role(str);

host fn print(p0: String) -> .;
```

```js
const host = {
  greeter: {
    print: (s) => { process.stdout.write(s); return null; },
  },
};
```

`host type` declarations need no runtime property; types are erased. A `role(...)` annotation admits the corresponding Kio literal syntax. The JavaScript backend represents host types carrying these roles with the JS value shapes listed below.

Named exported functions, host functions, and public newtype members follow
their Kio source parameters. A parameter `cfg: Left & Right` is one JS object
argument; separate parameter groups are combined into one native call. A
direct Unit domain is nullary, while a retained generic parameter specialized
to Unit remains an explicit `null` argument.

Callbacks and returned functions instead take one complete domain value. A
callback of type `A & B -> R` receives `{_0: a, _1: b}`, and a callback of type
`. -> R` is called as `callback(null)`. Use `liftGreeterCallable(body)` to turn
a host function into such a value. Polymorphic values expose one `.advance()`
step per type parameter; for example, an identity value of type `[A] A -> A`
is used as `identity.advance()(value)`. `liftGreeterPoly({advance() { ... }})`
constructs a host-supplied polymorphic value. These helpers are named after
the package namespace, just like `createGreeter`.

### 3. Instantiate and invoke

```js
const pkg = createGreeter(host);
pkg.greeter.KioModule_main.main();
```

`createGreeter(host)` validates the record, freezes an internal copy, evaluates
the package modules, and returns the package's exposed surface as a plain
object. A root module uses word casing (`my_module` → `myModule`); nested modules use
`KioModule_<exact-component>`, and public newtype handles use
`KioType_<exact-component>`. Exact components apply word casing before
escaping remaining underscores (`Native_token` → `KioType_NativeToken`).
Leading and trailing underscores are preserved by word casing. An export `main` from module `greeter/main` is
therefore reached as `pkg.greeter.KioModule_main.main`.

A public `newtype` adds a namespace containing only its public constructor
and projector. When both are public, the newtype is nonexistential, and its
payload-visible graph has no uncut cycle, those methods and public functions use
the same `{TypeName: payload}` boundary object. Otherwise,
the value is instead an opaque, package-produced handle: it can be retained
and passed back, but has no payload property and cannot be forged. A
constructor-only namespace can mint handles; a projector-only namespace can
inspect handles produced by the package. If neither member is public, the
namespace is empty and values can only be obtained from and shuttled through
other public functions. An opaque child cuts payload traversal, so a visible
wrapper can contain that child even when its hidden payload refers back to the
wrapper's type.

Newtype wrapper keys use word casing: `Native_token` gives `NativeToken`.
Products and sums always use binary `_0`/`_1` keys, retaining each child as a
complete value. Thus `A & (B & C)` is `{_0: a, _1: {_0: b, _1: c}}`; a newtype
child retains its own wrapper rather than becoming a product field name.

Concrete structural inputs and outputs are copied at their known type
boundaries. Mutating a returned pair's structural shells does not change the
corresponding shells retained by the package. Atomic referenced children are
not copied. Generic `T` and opaque host-constructor values pass through by
reference instead: the package does not inspect an unknown value to guess
its structure. Roleless host references likewise remain host-owned.

## Worked example

`greeter.pkg.kio`:

```kio {variant=package}
package greeter;

build {
  cache "out/.kio-cache/";

  target js {
    out "out/js/"
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

  target js {
    out "out/js/";
  }
}

bridge {
  greeter;
  greeter/**;
}
-->

<!--kio {harness=greeter_js_main file placeholder="__SNIPPET__"}
module greeter/main;

import greeter(String, print);

__SNIPPET__
-->

```kio {@greeter_js_main placeholder={"module greeter/main;":"","import greeter(String, print);":""}}
module greeter/main;

import greeter(String, print);

pub fn main() { print("hello from kio\n") }
```

Build and run:

```sh
kio build js
```

```js
import { createGreeter } from './out/js/greeter.js';

const pkg = createGreeter({
  greeter: {
    print: (s) => process.stdout.write(s),
  },
});

pkg.greeter.KioModule_main.main();
```

## Where this leads

- [`specs/backends/js.md`](../../specs/backends/js.md) — the full backend contract.
- [`specs/backends/README.md`](../../specs/backends/README.md) — cross-cutting properties shared by every backend.
- [Getting started with the Kio tooling](../tutorials/tooling.md) — build and check a package before loading it in a host.
- [`specs/cli.md`](../../specs/cli.md) — the exact `kio build` command contract.
