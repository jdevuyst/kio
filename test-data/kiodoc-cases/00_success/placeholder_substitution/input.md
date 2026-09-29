# Placeholder substitution

A harness declares its insertion marker explicitly. A snippet can
show placeholder text while validating replacement text.

```kio {harness=main placeholder="__SNIPPET__"}
bridge {
  kiodoc;
}

module kiodoc;

host type String role(str);

fn dummy_code() -> String { "ok\n" }

__SNIPPET__
```

```kio {@main placeholder={"...":"dummy_code()","???":"dummy_code()"}}
fn first() -> String { ... }
fn second() -> String { ??? }
```
