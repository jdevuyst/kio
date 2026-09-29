# Harness basic

A visible harness wraps a snippet. The harness body contains
`__INSERT_CODE_HERE__` where the snippet body drops in.

```kio {harness=main placeholder="__INSERT_CODE_HERE__"}
bridge {
  kiodoc;
}

module kiodoc;

host type S role(str);
host fn print(p0: S) -> .;

__INSERT_CODE_HERE__
```

```kio {@main}
pub fn greet() -> . { print("hi\n") }
```
