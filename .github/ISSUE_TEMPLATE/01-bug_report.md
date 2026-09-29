---
name: Bug report
about: Report a bug in the Kio compiler, CLI, or spec.
labels: bug
---

## What's broken

<!-- What happens, and what you expected instead. -->

## Reproducer

<!-- The ideal reproducer is a minimal `.kio` file that should compile or run but doesn't — paste the source inline, with the command you ran and the target (`js`, `rust`, …). That shape often lets a maintainer turn your report into a regression test almost verbatim — the fastest path from report to fix.

Even better: submit the reproducer as a pull request. Add it under `test-data/contrib/<your-github-username>-<this-issue-number>/` with a `KNOWN_FAILING` marker (it's expected to fail, so it won't break CI) and link this issue with `Closes #N`. When the bug is fixed the marker is removed and your case becomes a passing test. See CONTRIBUTING.md § Bug reports and test-data/contrib/README.md. -->

## Version

<!-- `kio --version` (or commit hash). OS and Rust toolchain if built from source. -->
