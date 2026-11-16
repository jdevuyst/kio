# File-backed harness

<!--kio {file}
mod broken;
fn value(x: ()) { x }
-->

<!--kio {harness=sample file placeholder="__INSERT__"}
mod sample;

__INSERT__
-->

```kio {@sample}
fn ready( ) {()}
```
