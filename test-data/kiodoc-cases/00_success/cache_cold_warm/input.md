# Cold / warm determinism fixture

Exercises the per-snippet cache. The first `kio doc` invocation
populates the cache; the second is expected to hit every entry
and produce byte-identical output (an empty stderr / stdout, in
this case).

```kio {harness=main placeholder="__INSERT_CODE_HERE__"}
bridge {
  kiodoc;
}

module kiodoc;

host type S role(str);
host fn print(p0: S) -> .;

__INSERT_CODE_HERE__
```

```kio {@main}
pub fn greet() -> . { print("hi\n") }
```

```kio {@main}
pub fn farewell() -> . { print("bye\n") }
```

```kio {}
bridge {
  kiodoc;
}

module kiodoc;

host type T role(str);

pub fn hello() -> T { "hello" }
```
