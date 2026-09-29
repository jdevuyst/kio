# Snippet declaring stdout

A snippet declaring `{stdout}` must be followed by a paired
`{stdout}` output fence. The runner enforces the pairing and does
not compare the output text against actual program output.

```kio {harness=main placeholder="__INSERT_CODE_HERE__"}
bridge {
  kiodoc;
}

module kiodoc;

host type S role(str);
host fn print(p0: S) -> .;

__INSERT_CODE_HERE__
```

```kio {@main stdout}
pub fn greet() -> . { print("hi\n") }
```

Producing:

```text {stdout}
hi
```
