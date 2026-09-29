# File-backed harness with ordinary import

A document-scoped file can provide a root module producer for a
file-backed module harness.

<!--kio {file}
package pkg;

bridge {
  pkg/**;
  utils;
  utils/**;
}
-->

<!--kio {file}
module utils;
-->

<!--kio {file}
module utils/list_ops;

pub fn fold[A](x: A) -> A { x }
pub fn map[A](x: A) -> A { x }
-->

<!--kio {harness=consumer file placeholder="__SNIPPET__"}
module pkg/main;

__SNIPPET__

fn use_fold[A](x: A) -> A { fold(x) }
-->

```kio {@consumer}
import utils/list_ops(fold, map);
import utils/list_ops as l;
```
