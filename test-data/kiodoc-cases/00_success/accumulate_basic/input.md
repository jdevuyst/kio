# Accumulating harness — basic

A harness declared with the `accumulate` bare flag aggregates
every `{@NAME}` member into one program. Members can refer to
each other's declarations across the document.

```kio {harness=running placeholder="__INSERT_CODE_HERE__" accumulate}
bridge {
  kiodoc;
}

module kiodoc;

host type S role(str);
host fn print(p0: S) -> .;

__INSERT_CODE_HERE__
```

First member introduces a helper.

```kio {@running}
fn greet(name: S) -> . { print(name) }
```

The second uses the helper from the first.

```kio {@running}
pub fn main() -> . { greet("world\n") }
```
