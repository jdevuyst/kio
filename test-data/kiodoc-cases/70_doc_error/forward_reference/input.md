# Forward reference to harness

A snippet references `main` before any `harness=main` declaration
has been seen.

```kio {@main}
pub fn greet() -> . { () }
```

```kio {harness=main placeholder="__INSERT_CODE_HERE__"}
module kiodoc/main;
__INSERT_CODE_HERE__
```
