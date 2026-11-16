# Root modules also belong to one implicit package

<!--kio {file}
mod utils;

pub fn identity[A](x: A) -> A { x }
-->

<!--kio {harness=consumer file placeholder="__SNIPPET__"}
mod consumer;

import utils {fn identity}

__SNIPPET__
-->

```kio {@consumer}
fn example[A](x: A) -> A { identity(x) }
```
