# variant=package

A `kio` fence carrying `variant=package` routes its body through
the `*.pkg.kio` parser rather than the regular-module parser.

```kio {variant=package}
package pkg;

bridge {
  pkg;
}
```
