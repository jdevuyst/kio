# Snippet with check_exit_code

`check_exit_code=N` asserts that `kio check` exits with code N
(per specs/exit-codes.md). The runner exits 0 if the actual exit
matches the declared one, even when the underlying snippet was
designed to *fail* check.

An expected failure is a validation *success*, so the rejected
snippet's own diagnostic is not printed: this case pins stderr
byte-exactly (`expected.stderr`, empty) rather than ignoring it.

```kio {harness=main placeholder="__INSERT_CODE_HERE__"}
bridge {
  kiodoc;
}

module kiodoc;

host type S role(str);
host type I role(i32);

__INSERT_CODE_HERE__
```

```kio {@main check_exit_code=14}
pub fn bad() -> S { 42 }
```
