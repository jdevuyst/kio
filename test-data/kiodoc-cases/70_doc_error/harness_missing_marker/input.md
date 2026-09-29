# Harness missing the substitution marker

A harness body must contain the literal token
`__INSERT_CODE_HERE__`. A harness without it is a runner error.

```kio {harness=main placeholder="__INSERT_CODE_HERE__"}
module kiodoc/main;
// no substitution marker — runner refuses to register this harness
```
