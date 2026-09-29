# Harness reference must open the attribute list

A fence's harness reference — `@NAME`, or the bare `@` inside a `///`
doc-comment — is its first attribute. Flags and key-value attributes
carry no order among themselves, but none of them may precede the
reference.

```kio {harness=main placeholder="__INSERT_CODE_HERE__"}
bridge {
  kiodoc;
}

module kiodoc;

host type S role(str);

__INSERT_CODE_HERE__
```

`check_exit_code=14` precedes `@main`, so the fence is a runner error:

```kio {check_exit_code=14 @main}
pub fn greet() -> S { "hi" }
```
