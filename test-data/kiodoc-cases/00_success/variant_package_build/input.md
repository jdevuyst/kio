# variant=package with build block

A `kio` fence carrying `variant=package` validates the whole
package file, including its optional `build { ... }` block.

```kio {variant=package}
package pkg;

build {
  cache "out/.kio-cache/";

  target js {
    out "out/js/";
  }
}
```
