# File A — first by sort order

Two snippets, each with a `check_exit_code` mismatch (the body
typechecks cleanly so `kio check` exits 0, but the directives
declare nonzero expected exits). The validator emits two
deterministic diagnostics in source order.

```kio {harness=h placeholder="__INSERT_CODE_HERE__"}
bridge {
  kiodoc;
}

module kiodoc;

host type S role(str);

__INSERT_CODE_HERE__
```

```kio {@h check_exit_code=11}
pub fn a1() -> S { "first" }
```

```kio {@h check_exit_code=12}
pub fn a2() -> S { "second" }
```
