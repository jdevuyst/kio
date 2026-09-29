# Snippet declaring stderr

The `stderr` pairing rule works the same as `stdout`. A snippet
can also assert a check-time failure category via
`check_exit_code=N`, and pair it with a `{stderr}` output fence
to pin the expected diagnostic text. The runner enforces the
attribute pairing.

```kio {harness=main placeholder="__INSERT_CODE_HERE__"}
bridge {
  kiodoc;
}

module kiodoc;

host type S role(str);
host type I role(i32);

__INSERT_CODE_HERE__
```

```kio {@main check_exit_code=14 stderr}
pub fn bad() -> S { 42 }
```

The package fails to typecheck with:

```text {stderr}
type mismatch: expected `S`, found `I`
```
