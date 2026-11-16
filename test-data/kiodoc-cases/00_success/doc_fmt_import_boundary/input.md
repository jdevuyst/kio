# Imports across a snippet boundary

<!--kio {file}
mod host;

host type String role(str);
-->

<!--kio {file}
mod names;

import host {type String}

pub type Name = String;
-->

<!--kio {harness=consumer file placeholder="__SNIPPET__"}
mod consumer;

__SNIPPET__
-->

```kio {@consumer}
import names {type Name}
import host {type String}

fn show(value: Name) -> String { value }
```
