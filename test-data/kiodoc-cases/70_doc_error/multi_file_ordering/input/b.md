# File B — second by sort order

Same shape as a.md: two `check_exit_code` mismatches. With four
total snippets across two files, this fixture exercises both
the cross-file sort and the within-file sort.

```kio {harness=h placeholder="__INSERT_CODE_HERE__"}
bridge {
  kiodoc;
}

module kiodoc;

host type S role(str);

__INSERT_CODE_HERE__
```

```kio {@h check_exit_code=21}
pub fn b1() -> S { "third" }
```

```kio {@h check_exit_code=22}
pub fn b2() -> S { "fourth" }
```
