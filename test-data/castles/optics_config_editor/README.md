# optics_config_editor

This castle models a small hosted configuration editor. The program builds a
nested service configuration, applies a fixed patch plan, and prints a compact
before/after summary plus a score delta.

The patch plan uses the optics POC package as a dependency. Lenses update nested
fields such as retry count, timeout, rollout percentage, and revision. Prisms
transition the mode from stable to degraded and then update the degraded
severity. An iso converted to a lens edits an audit limit inside a left-nested
limit bundle.

The case reads no stdin and has no fixture seed. `expected.stdout` is the full
program transcript: the original config summary, the patched config summary,
and a score/check line.

What this adds to the corpus: a dependency-integrated configuration-edit
workflow using records, labels, nested products, sums, lenses, prisms, and isos
against the `testapi-arith-collection` host surface, without relying on stdin or
raw intrinsic calls.
