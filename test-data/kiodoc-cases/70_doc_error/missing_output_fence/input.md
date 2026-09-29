# Missing output fence

A snippet declaring `{stdout}` must be followed by a paired
`{stdout}` output fence. Here the output fence is absent.

```kio {harness=main placeholder="__INSERT_CODE_HERE__"}
module kiodoc/main;
__INSERT_CODE_HERE__
```

```kio {@main stdout}
pub fn greet() -> . { () }
```
