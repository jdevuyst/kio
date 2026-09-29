# A `.md` snippet `kio check` rejects

The snippet declares no `check_exit_code`, so it asserts the default
`check_exit_code=0` — it must typecheck. It does not: `greet` returns
an `I` where its signature promises an `S`. The runner reports the
mismatch, the `kio check` diagnostic that produced it, and the source
it assembled to reach it.

```kio {harness=main placeholder="__INSERT_CODE_HERE__"}
bridge {
  kiodoc;
}

module kiodoc;

host type S role(str);
host type I role(i32);

__INSERT_CODE_HERE__
```

```kio {@main}
pub fn greet() -> S { 42 }
```
