# Nested support import closure

```kio {}
module example;

import utilities/entry(run);

fn result() -> . { run() }
```

```kio {check_exit_code=11}
module rejected;

import utilities/entry(run);

fn broken() -> . { let x = ; () }
```
