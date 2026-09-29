# Intervening fence between snippet and output

A snippet declaring `{stdout}` must be followed by the matching
output fence *before any other fence appears*.

```kio {harness=main placeholder="__INSERT_CODE_HERE__"}
module kiodoc/main;
__INSERT_CODE_HERE__
```

```kio {@main stdout}
pub fn greet() -> . { () }
```

Some intervening `kio {ignore}` fence breaks the pairing.

```kio {ignore}
not part of the program above
```

```text {stdout}
hello
```
