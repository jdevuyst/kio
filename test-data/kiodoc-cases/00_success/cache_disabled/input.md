# Cache disabled via `cache ();`

A package whose build block opts out of caching (`cache ();`)
must still let `kio doc` run every snippet uncached, without
erroring. The on-disk cache directory is never created.

```kio {}
bridge {
  kiodoc;
}

module kiodoc;

host type S role(str);
host fn print(p0: S) -> .;

pub fn greet() -> . { print("hi\n") }
```
