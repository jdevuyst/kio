# check_exit_code mismatch

The snippet asserts `check_exit_code=14`, but the body is valid
Kio and typechecks cleanly (exit 0). The runner fails because the
actual exit does not match the declared one.

```kio {harness=main placeholder="__INSERT_CODE_HERE__"}
bridge {
  kiodoc;
}

module kiodoc;

host type S role(str);

__INSERT_CODE_HERE__
```

```kio {@main check_exit_code=14}
pub fn greet() -> S { "hi" }
```
