# Hidden harness

The harness lives inside an HTML comment so GitHub readers don't
see it. The snippet that follows references it the same way it
would reference a visible harness.

<!--kio {harness=main placeholder="__INSERT_CODE_HERE__"}
bridge {
  kiodoc;
}

module kiodoc;

host type S role(str);

__INSERT_CODE_HERE__
-->

```kio {@main}
pub fn first_char() -> S { "x" }
```
