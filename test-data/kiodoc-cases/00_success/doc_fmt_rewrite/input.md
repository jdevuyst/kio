# Doc fmt rewrite

`kio doc fmt --check` reports this file until the snippet bodies are
canonical.

```kio {harness=main placeholder="__INSERT_CODE_HERE__"}
bridge {
  fmtcase;
}

module fmtcase;

host type S role(str);
host fn print(p0: S) -> .;

__INSERT_CODE_HERE__
```

```kio {@main}
pub fn greet()-> .{print("hi\n")}
```

```kio {variant=package}
package fmtcase;
build{cache();}
```
