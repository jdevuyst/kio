# REPL fence opaque

`kio-repl` fences are opaque prose — the runner does not engage
them.

```kio-repl
>>> 1 + 1
2
>>> let x = 5
>>> x * 2
10
```

This is also a place to validate that ordinary `text` fences with
no attributes pass through quietly.

```text
some plain text
```
