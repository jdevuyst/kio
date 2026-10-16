# File-backed harness

<!--kio {file}
module broken;
fn value(x: ()) { x }
-->

<!--kio {harness=sample file placeholder="__INSERT__"}
module sample;

__INSERT__
-->

```kio {@sample}
fn ready( ) {()}
```
