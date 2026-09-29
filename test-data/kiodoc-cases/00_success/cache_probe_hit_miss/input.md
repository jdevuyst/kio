# Doc cache hit / miss probe fixture

Two independent snippets. The per-snippet doc cache keys each by its
own content, so editing one snippet's body must MISS only that snippet
while the other keeps HITting.

```kio {}
bridge {
  kiodoc;
}

module kiodoc;

host type T role(str);

pub fn alpha() -> T { "alpha" }
```

```kio {}
bridge {
  kiodoc;
}

module kiodoc;

host type T role(str);

pub fn beta() -> T { "beta" }
```
