# example-0

The reference contributed test case: a minimal Kio program that prints
`Hello, world!` and exits `0`. It's maintainer-owned (the `example-0`
name, not a real `<github-username>-<issue-number>`), and it stays in the
corpus for two reasons:

- it's the template to copy when contributing your own case — see the
  contract in [`../README.md`](../README.md);
- it keeps the contrib harness exercised on every CI pass, even when no
  real contribution is open, so a break in the contrib build/run path
  surfaces immediately rather than on the next contributor's PR.

`main` calls a host `print` over the `testapi-print` protocol (named in
`run.args`); `workdir/` holds only `.kio` sources, and the greeting is
pinned in `expected.stdout`.
