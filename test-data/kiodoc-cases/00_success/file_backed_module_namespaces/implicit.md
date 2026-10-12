# Different module namespaces in one implicit package

<!--kio {file}
module utils/list_ops;

pub fn identity[A](x: A) -> A { x }
-->

<!--kio {harness=consumer file placeholder="__SNIPPET__"}
module app/main;

import utils/list_ops(identity);

__SNIPPET__
-->

```kio {@consumer}
fn example[A](x: A) -> A { identity(x) }
```
