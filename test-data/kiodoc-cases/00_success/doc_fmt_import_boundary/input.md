# Imports across a snippet boundary

<!--kio {file}
module host;

host type String role(str);
-->

<!--kio {file}
module names;

import host(String);

pub type Name = String;
-->

<!--kio {harness=consumer file placeholder="__SNIPPET__"}
module consumer;

__SNIPPET__
-->

```kio {@consumer}
import names(Name);
import host(String);

fn show(value: Name) -> String { value }
```
