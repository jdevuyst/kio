---
name: audit-refl-intrinsic-constructors
description: Verify reflected intrinsic term constructors stay in 1-1 name correspondence with __intrinsics__ and remain separate from core term constructors
allowed-tools: Read, Grep, Bash
---

# Reflection Intrinsic Constructor Audit

This audit keeps the `__refl__` term-construction surface tidy:

- core-language term constructors use `term_*`;
- intrinsic-call term constructors use `__intrinsic_*__`;
- every `__intrinsics__` value has exactly one reflected intrinsic constructor named by stripping the leading/trailing double underscores from the intrinsic name and wrapping the result as `__intrinsic_<name>__` (so `__pair__` → `__intrinsic_pair__`);
- no legacy intrinsic-shaped `term_*` constructor is reintroduced.

## 1. Run The Parity Check

From the repo root, run:

```bash
python3 - <<'PY'
from pathlib import Path
import re
import sys

root = Path(".")
resolve = (root / "kio-rs/src/pass/resolve.rs").read_text()
synth = (root / "kio-rs/src/pass/typecheck_core/synth.rs").read_text()
eval_rs = (root / "kio-rs/src/comptime.rs").read_text()

match = re.search(r"pub const PRIME_INTRINSICS: &\[&str\] = &\[(.*?)\];", resolve, re.S)
if not match:
    print("could not find PRIME_INTRINSICS in kio-rs/src/pass/resolve.rs")
    sys.exit(1)

intrinsics = re.findall(r'"(__[A-Za-z0-9_]+__)"', match.group(1))
expected = {
    name: "__intrinsic_" + name.removeprefix("__").removesuffix("__") + "__"
    for name in intrinsics
}

scheme_names = set(re.findall(r'"(__intrinsic_[A-Za-z0-9_]+__)"', synth))
eval_names = set(re.findall(r'"(__intrinsic_[A-Za-z0-9_]+__)"', eval_rs))
expected_names = set(expected.values())

errors = []
for intrinsic, refl_name in expected.items():
    if refl_name not in scheme_names:
        errors.append(f"{intrinsic} is missing reflection scheme {refl_name}")
    if refl_name not in eval_names:
        errors.append(f"{intrinsic} is missing evaluator arm {refl_name}")

extra_scheme = sorted(scheme_names - expected_names)
extra_eval = sorted(eval_names - expected_names)
if extra_scheme:
    errors.append("extra __intrinsic_*__ reflection scheme(s): " + ", ".join(extra_scheme))
if extra_eval:
    errors.append("extra __intrinsic_*__ evaluator arm(s): " + ", ".join(extra_eval))

legacy = sorted(
    set(
        re.findall(
            r'"(term_(?:left|right|either|product|fst|snd|absurd|if_then_else|mk_[A-Za-z0-9_]+))"',
            synth + "\n" + eval_rs,
        )
    )
)
if legacy:
    errors.append("legacy intrinsic-shaped term_* constructor(s): " + ", ".join(legacy))

if errors:
    print("\n".join(errors))
    sys.exit(1)

for intrinsic, refl_name in expected.items():
    print(f"{intrinsic} -> {refl_name}")
PY
```

Findings:

- **Missing reflected constructor** — an intrinsic in `PRIME_INTRINSICS` lacks its `__intrinsic_*__` scheme or evaluator arm.
- **Extra reflected intrinsic constructor** — an `__intrinsic_*__` constructor exists without a matching `__intrinsics__` value.
- **Legacy constructor name** — a reflected intrinsic call is exposed as `term_*` rather than `__intrinsic_*__`.

## 2. Check Elaborator POCs

Run:

```bash
grep -R -nE '\bterm_(left|right|either|product|fst|snd|absurd|if_then_else|mk_[A-Za-z0-9_]+)\b' test-data/poc/elab/ || true
grep -R -nE '__intrinsic_(left|right|either|pair|fst|snd|if_then_else|absurd)__' test-data/poc/elab/
```

Every first-command hit is a finding unless it is part of an unrelated local helper name rather than a `__refl__` import/call. The second command should show intrinsic-call constructors only under the `__intrinsic_*__` names.

## 3. Check The Spec

Read `specs/language.md` around the user-defined elaborator hygiene paragraph. It must state:

- generated elaborator terms are hygienic;
- callers are not required to import `__intrinsics__` for intrinsic calls emitted by an elaborator;
- reflected term constructors are split into core `term_*` constructors and intrinsic-call `__intrinsic_*__` constructors;
- the listed `__intrinsic_*__` names mirror the `__intrinsics__` value names.

## 4. Report

Group findings by file and rule. For each, cite the missing, extra, or wrongly named constructor and whether the mismatch appears in the scheme, evaluator, POC corpus, or spec.

**Default: report only.** If invoked with a fix-it directive, follow [`ai/topics/audit-fix-mode.md`](../../topics/audit-fix-mode.md). Fixes should update the scheme, evaluator, POCs/goldens, and `specs/language.md` together, then run focused user-elaborator checks.
