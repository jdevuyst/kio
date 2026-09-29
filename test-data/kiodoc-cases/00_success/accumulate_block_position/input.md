# Accumulating harness — block position

A harness can wrap `__INSERT_CODE_HERE__` inside a function body
so members contribute block contents rather than top-level forms.
Combined with `accumulate`, the chapter-by-chapter narrative
pattern: each member adds one more line to one growing function.

```kio {harness=block placeholder="__INSERT_CODE_HERE__" accumulate}
bridge {
  kiodoc;
}

module kiodoc;

host type S role(str);
host fn print(p0: S) -> .;

pub fn main() -> . {
__INSERT_CODE_HERE__
}
```

First, a let binding.

```kio {@block}
let greeting = "hello\n";
```

Then a final expression to flush the block.

```kio {@block}
print(greeting)
```
