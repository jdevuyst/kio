# Self-ref on a `.md` fence

The bare `@` names the snippet's *surrounding module* as its harness.
That is a `///` doc-comment form: a `.md` fence has no surrounding
module, so `{@}` is a runner error here. A `.md` snippet references a
declared harness with `{@NAME}`, or is standalone with `{}`.

```kio {@}
module kiodoc;

pub fn greet() -> . { () }
```
