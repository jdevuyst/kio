# variant=package — bad body

A `kio` fence with `variant=package` whose body doesn't parse
as a package file fails the snippet's expected `check_exit_code`
of 0.

```kio {variant=package}
fn this_is_not_valid_in_a_package_file() -> . { () }
```
