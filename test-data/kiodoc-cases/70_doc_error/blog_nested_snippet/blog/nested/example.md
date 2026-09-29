# A nested blog example

This example promises a string but returns an integer.

<!--kio {harness=example placeholder="__INSERT_CODE_HERE__"}
bridge {
  kiodoc;
}

module kiodoc;

host type S role(str);
host type I role(i32);

__INSERT_CODE_HERE__
-->

```kio {@example}
pub fn greet() -> S { 42 }
```
