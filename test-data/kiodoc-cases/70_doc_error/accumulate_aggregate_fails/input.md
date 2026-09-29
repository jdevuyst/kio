# Accumulating harness — aggregate fails

The aggregate of these members declares the same function twice,
which the typechecker rejects.

```kio {harness=dup placeholder="__INSERT_CODE_HERE__" accumulate}
bridge {
  kiodoc;
}

module kiodoc;

host type S role(str);

__INSERT_CODE_HERE__
```

First definition:

```kio {@dup}
fn helper() -> . { () }
```

Second definition of the same name:

```kio {@dup}
fn helper() -> . { () }
```
