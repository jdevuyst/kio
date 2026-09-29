# Dependency injection

Dependency injection in Kio is not a framework feature. It is an ordinary
programming pattern: take the capabilities a function needs as an argument,
bundle those capabilities with `&`, and build the bundle at the edge of the
package.

That fits Kio especially well because product types are structural. A
dependency bundle can be positional when it is small and local, labeled when the
names matter, and row-shaped when a helper should accept "at least these
capabilities" without caring what else the caller carries.

Assumed setup: a module whose host supplies `String`, `print`, and `string_concat`.
Snippets thread together through one accumulating harness.

<!--kio {harness=di placeholder="__INSERT_CODE_HERE__" accumulate}
bridge {
  kiodoc;
}

module kiodoc;

import spine_elaborators(narrow_prod, one_prod);

host type String role(str);
host fn print(p0: String) -> .;
host fn string_concat(p0: String, p1: String) -> String;

__INSERT_CODE_HERE__
-->

## Start with plain functions

A dependency is usually just a function value. If a service needs a logger and
a configuration loader, write those two function types down and pass them in:

```kio {@di}
type Raw_deps = (String -> .) & (. -> String);

fn raw_log(deps: Raw_deps) -> String -> . { deps.>one_prod!(String -> .) }

fn raw_config(deps: Raw_deps) -> . -> String { deps.>one_prod!(. -> String) }

fn start_raw(deps: Raw_deps) -> . { raw_log(deps)(raw_config(deps)()) }
```

This is enough for small private code. The bundle is a product, and
`one_prod!` projects the dependency whose type you ask for. There is no ambient
lookup and no hidden container: the caller chooses the implementation by
choosing the value it passes.

The tradeoff is ambiguity. `Raw_deps` works because the two function types are
different. If two dependencies share a type, or if this bundle crosses a module
boundary, give the slots names.

## Name public bundles with labels

Labels turn a product into a record-shaped dependency bundle. The type is still a
product under the hood, but each slot has a stable field name:

```kio {@di}
labels App_deps = { log: String -> ., config: . -> String };

fn app_log(deps: App_deps) -> String -> . { deps.?{log} }

fn app_config(deps: App_deps) -> . -> String { deps.?{config} }

fn start(deps: App_deps) -> . { app_log(deps)(app_config(deps)()) }
```

`App_deps` is the product `Log & Config`. `deps.?{log}` finds the visible
`Log` slot and unwraps the function payload.

This is the shape to prefer for API-facing dependency bundles:

- readers see names, not just function types;
- two dependencies may have the same function type without becoming ambiguous;
- helper functions can ask for a named subset of a larger bundle.

## Build the bundle at the edge

The `host` declarations are the package boundary. A module can bring those host
capabilities into scope with an `import`, wrap them in a bundle once, and pass that
bundle through the rest of the program:

```kio {@di}
fn default_config() -> String { "production\n" }

fn production_deps() -> App_deps { {log = print, config = default_config} }

pub fn main() -> . { start(production_deps()) }
```

Tests, examples, or alternate hosts build a different `App_deps` value. The
business code does not change; only the bundle construction changes.

```kio {@di}
fn quiet_log(_s: String) -> . { () }

fn test_config() -> String { "test\n" }

fn test_deps() -> App_deps { {log = quiet_log, config = test_config} }
```

This is the core DI move in Kio: dependencies are ordinary values, and swapping
an implementation is ordinary value substitution.

## Ask for only what a helper needs

A helper does not have to name the whole application bundle. Because products
compose structurally, the helper can accept any bundle that contains the slot it
uses:

```kio {@di}
fn log_line[Rest](deps: Log & Rest, msg: String) -> . { deps.?{log}(string_concat(msg, "\n")) }

fn call_log_line(deps: App_deps) -> . { log_line(deps, "ready") }
```

Read `(Log & rest)` as "a `log` dependency plus whatever else the caller is
carrying." `call_log_line` can pass the full `App_deps`; `log_line` projects out
the single capability it needs.

This keeps dependency parameters narrow without making callers rebuild tiny
one-off bundles. It is the same row-typed-record pattern described in
[Structural products and row types](products.md), applied to capability passing.

## Split bundles by boundary

Use one bundle per meaningful boundary, not one global environment for the
whole package. A parser, a storage layer, and a notification workflow usually
need different capabilities. Separate bundles make those boundaries visible:

```kio {@di}
labels Parse_deps = { report_error: String -> . };

labels Store_deps = { save: String -> ., load: . -> String };

labels Notify_deps = { send: String -> ., template: . -> String };
```

When a workflow really does need several bundles, compose them with `&` and let
helpers project their slice:

```kio {@di}
type Workflow_deps = Parse_deps & Store_deps & Notify_deps;
```

The important rule is that the bundle should describe the caller-callee
contract. If a function only logs, take `(Log & rest)`. If a module owns a
coherent subsystem, define a named `labels ...` bundle for that subsystem. If a
single private helper needs two distinct functions, a small positional product
is fine.

## What not to inject

Do not pass a dependency bundle just because a value is "global" in another
language. Prefer a normal argument for ordinary data:

```kio {@di}
fn render_user(name: String) -> String { name }
```

Reach for a dependency bundle when the caller should choose an implementation:
I/O, logging, clocks, randomness, storage, parsing tables, host adapters, or an
algorithm strategy. Keep data as data.

Also avoid turning every function into `.(deps, ...)`. Inject at the boundary
where the choice matters, then pass narrower products to the helpers that need
them. Product types make that cheap; using them everywhere still makes APIs
harder to read.

## Where this leads

- [Structural products and row types](products.md) explains the `(Field & rest)` pattern directly.
- [Using libraries](using-libraries.md) points to the product projection helpers used here.
- [Package files and bridges](pkg.md) shows the
  package-level version of the same idea: adapting a root module's host
  environment at the package edge with `bridge`.
