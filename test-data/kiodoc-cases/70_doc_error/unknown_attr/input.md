# Unknown attribute

Unknown attributes are runner errors — they typically signal a
typo (e.g. `stout` for `stdout`).

```kio {harness=main placeholder="__INSERT_CODE_HERE__"}
mod kiodoc/main;
__INSERT_CODE_HERE__
```

```kio {@main stout}
pub fn greet() { () }
```
