# Standalone snippet

A snippet marked `{}` is a complete Kio program — an optional
`bridge { … }` glob, a module header, optional module-level `host`
declarations, and a module body.

```kio {}
bridge {
  kiodoc;
}

module kiodoc;

host type S role(str);
host fn print(p0: S) -> .;

pub fn greet() -> . { print("hi\n") }
```
