# Nested support import closure

```kio {}
mod example;

import utilities/entry {fn run}

fn result() { run() }
```

```kio {check_exit_code=11}
mod rejected;

import utilities/entry {fn run}

fn broken() { let x = ; () }
```
