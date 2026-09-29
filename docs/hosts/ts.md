# Hosting Kio in TypeScript

**Host API stability:** `evolving`

Status policy: [Host API stability](../../specs/backends/README.md#host-api-stability).

This guide walks through compiling a Kio package to TypeScript and calling into it from a TypeScript host with full type-checking. The authoritative contract is [`specs/backends/ts.md`](../../specs/backends/ts.md); this guide is the narrative companion.

> **Known host-source compatibility caveat.** When removed host signatures
> depend on incompatible epochs of one exact nominal declaration, the `.d.ts`
> omits those signatures and their conflicting type closure. Existing host
> source must delete or rewrite the affected optional properties and any
> import or annotation that names the superseded generated type. See the
> TypeScript contract's
> [incompatible-retained-epochs section](../../specs/backends/ts.md#incompatible-retained-declaration-epochs-are-not-source-stable-on-typescript).

The TypeScript backend is the **typed-skin companion of the JavaScript backend**: `kio build ts` writes the JS backend's `<ns>.js` byte-identical plus a generated `<ns>.d.ts` declaration file that types the FFI boundary (`<ns>` is the artifact namespace — the package name unless a `namespace` target key overrides it). The runtime artifact is the same one a JS host runs (see [Hosting Kio in JavaScript](js.md)); the `.d.ts` adds the types — the standard way a typed npm package ships type annotations for plain JavaScript.

## The build target

A package emits to TypeScript by declaring a `ts` target in `<pkg>.pkg.kio`:

```kio {variant=package}
package greeter;

build {
  cache "out/.kio-cache/";

  target ts {
    out "out/ts/"
  }
}

bridge {
  greeter;
  greeter/**
}
```

`kio build` or `kio build ts` writes the package surface under the target output directory:

```text
out/ts/greeter.js     # the runtime ES module — byte-identical to target=js
out/ts/greeter.d.ts   # the typed FFI skin
```

The `greeter.js` is the JavaScript backend's output verbatim — the body is
shared with `target=js` and is drift-proof. The `greeter.d.ts` is the only
artifact `ts` adds over `js`: it exports `GreeterHostTypes`,
`GreeterHost<B>`, `Greeter<B>`, `GreeterTypes<B>`, the structural
`GreeterTypeLambda` / `GreeterApply` / `GreeterBind` protocol, and the
`createGreeter` factory. Those names are branded from the package namespace
`greeter` (the default; a `namespace` target key overrides it, renaming the
files and the brand together). The runtime targets ECMAScript 2020 (the JS
backend's floor); the `.d.ts` targets TypeScript 5.0+ and type-checks under
`--strict`.

## Loading the package

A TypeScript host brings the package online in three steps — the same steps a JS host follows, with types added.

### 1. Import the factory

```ts
import { createGreeter, type Greeter, type GreeterApply, type GreeterHost, type GreeterHostTypes, type GreeterTypeLambda } from './out/ts/greeter.js';
```

The single runtime export is `createGreeter`; every other imported name is a
type. TypeScript resolves the sibling `greeter.d.ts` for `greeter.js`
automatically. Importing the module does nothing observable; the package only
comes online when you call the factory.

The artifact stem retains the effective namespace: `my_pkg.js` exports
`createMyPkg`. The public brand, including the exact `KioPkg_...` and
`KioNs_...` cases, follows [the JS namespace rules](../../specs/backends/js.md#output-layout).

### 2. Build the host record

The host record is typed by `GreeterHost<B>`: one member per `host fn` the
bridged modules declare, **nested by module**. A path without source `_` is
formed with `/` replaced by `_`, keeping lowercase components; a path containing
`_` uses `KioModule_...` after word casing and escaping remaining `_` as `_u`
and `/` as `_s`. Thus `foo/bar` and `foo_bar` use distinct `foo_bar` and
`KioModule_fooBar` properties. Callable leaves also use word casing
(`do_work` → `doWork`). A
`host fn print` in module `greeter` is supplied as the readonly callable
property `greeter.print`:

```kio {variant=module}
// greeter.kio
module greeter;

host type String role(str);

host fn print(s: String) -> .;
```

```ts
const host: GreeterHost = {
  greeter: {
    print: (s) => { process.stdout.write(s); return null; },
  },
};
```

TypeScript checks the supplied record at compile time, including generic and
rank-N parameter and return relations. A `host type` needs no runtime property
— types remain erased from the JavaScript value — and a `role(...)` annotation
fixes the TypeScript primitive for an atomic type (here `String` → `string`).
Function-valued properties preserve strict parameter variance under
`--strictFunctionTypes`.

### Selecting native host types

Every roleless `host type` is selected once in an interface extending
`GreeterHostTypes`. The outer property applies word casing per module component
while keeping `/`: module `api/data_store` uses the quoted key `"api/dataStore"`.
The inner property is the word-cased declaration name (`Native_token` →
`NativeToken`). These type-selection keys differ from runtime host-record keys.
A nullary declaration
selects a native type directly. A positive-arity declaration selects a
structural type-lambda witness:

```ts
type NativeBox<A> = { readonly value: A };

interface BoxK extends GreeterTypeLambda<readonly [unknown]> {
  readonly type: NativeBox<this["arguments"][0]>;
}

interface AppTypes extends GreeterHostTypes {
  readonly app: {
    readonly Token: URL;
    readonly Box: BoxK;
  };
}

type BoxedText = GreeterApply<BoxK, readonly [string]>;
const boxedText: BoxedText = { value: "hello" };
```

For a nested module the outer key stays readable and exact, for example
`readonly "app/model": { ... }`; this type-only key is independent of the
mangled runtime host-record key. Role types never need selection. Annotating a
host as `GreeterHost<AppTypes>` lets `createGreeter(host)` infer the same
`AppTypes` for the returned handle. A package with no host functions takes no
runtime argument; if it still has roleless types, write
`createGreeter<AppTypes>()` to select them.

The generated facade validates every slot. Missing or partial selections,
`any`, `unknown`, `never`, wrong constructor arity, and constructors without a
concrete result produce branded, uninhabited diagnostic types instead of
silently weakening the boundary.

After a host function is removed, the declaration file keeps its exact old
function type as an optional property marked `@deprecated` whenever its frozen
type closure is representable alongside the other declaration epochs. This
matters for object literals: the old function's parameters and result remain
contextually typed under `--strict`, while a new host simply leaves it out. If
every function under an old module path has been removed, that outer property
is optional and deprecated too. A module that still has a live function
remains required, with only its removed members optional.

Any host-type selection, carrier, alias, or helper declaration kept solely for
that old signature is also deprecated. When retained declaration epochs are
compatible, old binding maps may continue to select those types; new maps need
not include them. If current source uses the same
declaration again, it is live, required where applicable, and not deprecated.
Compatibility declarations add no factory requirement, package entry,
JavaScript validation, or dispatch path; the generated `.js` remains the
current JavaScript backend output byte for byte.

Two incompatible frozen declarations cannot share one exact generated type
path. In that case the declaration file omits every affected retained method
and type rather than widening either signature or requiring a current host to
select history. An old host deletes or rewrites those optional properties and
replaces imports or annotations that name the superseded generated type. See
the caveat banner above.

### 3. Instantiate and invoke

```ts
const pkg = createGreeter(host);
pkg.greeter.KioModule_main.main();
```

`createGreeter(host)` returns a value typed by `Greeter<B>`; `B` is inferred
from an annotated host when the package has roleless types. Its runtime
behavior is the JavaScript backend's. A root module uses word casing;
nested modules use `KioModule_<exact-component>`, and public
newtype handles use `KioType_<exact-component>`. Exact components word-case
first, then escape remaining underscores as `_u`; outer underscore runs survive
word casing. An export `main` from module
`greeter/main` is therefore reached as
`pkg.greeter.KioModule_main.main`.

A public `newtype` adds a typed namespace containing only its public
constructor and projector. A nonexistential newtype with both operations
public uses the visible `{TypeName: payload}` shape unless its payload has an
uncut payload-visible recursive cycle. Member-hidden and existential newtypes,
and those uncut cycles, use a declaration-private invariant nominal carrier
parameterized by the host selection and visible generic arguments. TypeScript
can infer and shuttle that carrier, while host code can only use the public
constructor/projector operations. An existential projector exposes its hidden
type only inside the exact generic CPS continuation. A newtype with no public
members has an empty namespace type.

An opaque or one-member carrier is already atomic at the JS boundary and cuts
the recursion walk. An enclosing visible wrapper such as
`Root : Hidden(Root)` therefore remains the host-constructible structural
`{ Root: Hidden<Root> }` shape. Its declaration uses a private structural
interface only to give the recursive reference a finite name; that interface
is not a nominal brand.

## Types at the boundary

The `.d.ts` types the **JS shapes** values cross in — the runtime is the JS
backend's, so the declarations describe exactly the plain-object / primitive
shapes the `.js` produces while preserving Kio's type relations.

- **Atomic role types** map to TypeScript primitives: `str` → `string`, `bool` → `boolean`, the `i8`…`i32` / `u8`…`u32` / float roles → `number`, and the wide-integer roles `i64` / `i128` / `u64` / `u128` → `bigint` (the `.js` carries those as `BigInt`).
- **Products** `(A & B)` are objects keyed by the right-spine slot: `{ _0: shape(A); _1: shape(B) }` (or by newtype name where the slot resolves to one).
- **Sums** `(A | B)` are discriminated unions `{ _0: shape(A) } | { _1: shape(B) }`, exactly one property set — TypeScript narrows each branch by which property is present.
- **Newtypes** follow the visible/nominal rule above. Generic parameters remain
  visible in either representation. Existentials and uncut payload-visible
  recursive cycles stay nominal; recursion through an atomic hidden carrier
  can leave an enclosing public wrapper structural.
- **Unit** is `null`, **bottom** is `never`, and **function values** are
  TypeScript function types.
- **Ordinary Kio type variables** are TypeScript generics. A public
  `pub fn id[T](x: T) -> T` is `<T>(p0: T) => T`, and callbacks, returns, and
  nested `forall`s retain their own generic function layers.
- **Roleless host types** use the selected `B["<module>"]["<name>"]` entry.
  Applications of positive-arity host types and higher-kinded binders use
  `GreeterApply`; non-saturating prefixes use `GreeterBind`.

`GreeterTypeLambda`, `GreeterApply`, and recursive `GreeterBind` are structural
and have no arity cap. Independently authored witnesses and partial
applications work across generated packages. The protocol diagnoses a bare
base witness, wrong arity, and `any` / `unknown` / `never` results instead of
leaking a universal type. Runtime type arguments remain erased: these exact
relations add no JavaScript arguments, wrappers, or casts.

`GreeterTypes<B>` provides a readable declaration-keyed catalogue of selected
host types, fixed role types, and reachable public Kio newtype constructors for
generic host-side composition.

Catalog and type-selection paths word-case each module component while
retaining `/`, and word-case the type leaf. Named product/sum object keys also
word-case source components, retaining `/` and `.` for qualified keys
(`"dataApi/model.NativeToken"`); positional keys such as `_0` stay unchanged.
The nominal type brand's opaque identity is not derived from these display keys.

See [`specs/backends/ts.md` § FFI surface](../../specs/backends/ts.md#ffi-surface) for the full mapping.

## Worked example

`greeter.pkg.kio`:

```kio {variant=package}
package greeter;

build {
  cache "out/.kio-cache/";

  target ts {
    out "out/ts/"
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
host fn print(s: String) -> .;
```

`greeter/main.kio` imports it and exports `main`:

<!--kio {file}
package greeter;

build {
  cache "out/.kio-cache/";

  target ts {
    out "out/ts/";
  }
}

bridge {
  greeter;
  greeter/**;
}
-->

<!--kio {harness=greeter_ts_main file placeholder="__SNIPPET__"}
module greeter/main;

import greeter(String, print);

__SNIPPET__
-->

```kio {@greeter_ts_main placeholder={"module greeter/main;":"","import greeter(String, print);":""}}
module greeter/main;

import greeter(String, print);

pub fn main() -> . { print("hello from kio\n"(String)) }
```

Build:

```sh
kio build ts
```

`kio build ts` writes `out/ts/greeter.js` (byte-identical to `target=js`) and `out/ts/greeter.d.ts`. The `.d.ts` declares (shape illustrative):

```ts
export interface GreeterHostTypes {}

export type GreeterHost<
  B extends GreeterHostTypes = GreeterHostTypes,
> = {
  readonly greeter: {
    readonly print: (p0: string) => null;
  };
};

export type Greeter<
  B extends GreeterHostTypes = GreeterHostTypes,
> = {
  greeter: {
    KioModule_main: {
      main: () => null;
    };
  };
};

export function createGreeter<B extends GreeterHostTypes = GreeterHostTypes>(
  host: GreeterHost<B>,
): Greeter<B>;
```

A Node host runs it:

```ts
import { createGreeter, type GreeterHost } from './out/ts/greeter.js';

const host: GreeterHost = {
  greeter: {
    print: (s) => { process.stdout.write(s); return null; },
  },
};

const pkg = createGreeter(host);
pkg.greeter.KioModule_main.main();
// → writes "hello from kio\n" to stdout
```

## Where this leads

- [`specs/backends/ts.md`](../../specs/backends/ts.md) — the full backend contract.
- [Hosting Kio in JavaScript](js.md) — the runtime `.js` contract the TypeScript backend shares.
- [`specs/backends/README.md`](../../specs/backends/README.md) — cross-cutting properties shared by every backend.
- [Getting started with the Kio tooling](../tutorials/tooling.md) — build and check a package before loading it in a host.
- [`specs/cli.md`](../../specs/cli.md) — the exact `kio build` command contract.
