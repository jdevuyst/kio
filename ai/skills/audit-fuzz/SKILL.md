---
name: audit-fuzz
description: Run the parser / lexer fuzz harness and triage any new crashes into proposed regression goldens
allowed-tools: Read, Grep, Glob, Bash
---

# Fuzz audit

Drive [`reports/fuzz.sh`](../../../reports/fuzz.sh) against the parser and lexer entry points (libFuzzer via cargo-fuzz), then triage any inputs that crash. A surviving crash names a parser or lexer bug: well-formed input must produce a `Module` / token vector, ill-formed input must produce a structured `Error` per [`specs/exit-codes.md`](../../../specs/exit-codes.md), neither should panic, loop, or stack-overflow.

Pre-req: nightly Rust toolchain (installed by the pinned Rust postinstall) and `cargo-fuzz` (`sh ci/impl-toolchain.sh install-report-tools`). The fuzz crate at [`kio-rs/fuzz/`](../../../kio-rs/fuzz/) is the libFuzzer entry-point package; see [`ai/topics/repo-layout.md`](../../topics/repo-layout.md).

## 1. Snapshot prior findings

`kio-rs/fuzz/artifacts/<target>/` accumulates crashing inputs across runs. Record the set of files there *before* running so step 3's diff distinguishes new findings from already-known ones:

```sh
snap=$(mktemp -d)
find kio-rs/fuzz/artifacts -type f 2>/dev/null | sort > "$snap/before.txt"
```

(A `mktemp`-owned directory keeps parallel sessions from clobbering each other's snapshots.)

## 2. Run the harness

Invoke the script with its default budget (180s per target) unless the user asked for longer:

```sh
sh reports/fuzz.sh
```

A non-zero exit from `reports/fuzz.sh` means a real harness failure (cargo-fuzz can't build, nightly missing, etc.) — surface it and stop. A crash detection during fuzzing exits 0 from the script; the crash lands as a file under `kio-rs/fuzz/artifacts/<target>/`.

To target a single fuzz target or extend the budget for local triage, use the script's flags: `sh reports/fuzz.sh --target=parse --timeout=600`.

## 3. Diff to find new findings

```sh
find kio-rs/fuzz/artifacts -type f 2>/dev/null | sort > "$snap/after.txt"
diff "$snap/before.txt" "$snap/after.txt"
```

Any file in the "after" set that wasn't in "before" is a new finding from this run. Files that were already there are previously-known and don't need re-triage unless the user asks.

## 4. Classify each new finding

For each new crashing input:

1. **Read the input bytes.** `xxd kio-rs/fuzz/artifacts/<target>/<file>` or equivalent. Most libFuzzer findings are short (sub-256-byte) byte sequences that violate some lexer / parser invariant.
2. **Reproduce locally.** Pipe the bytes through `kio` to see what category of failure manifests:
   - `cat kio-rs/fuzz/artifacts/<target>/<file> | kio check -` (if `kio check` accepts stdin in your build) — or write the bytes to a temp `*.kio` file and run `kio check <path>`.
   - Note the exit code and stderr category. Map to a `specs/exit-codes.md` bucket if the panic was caught and turned into a structured error; otherwise the panic itself is the bug.
3. **Decide the category:**
   - **Panic / abort / stack-overflow** — parser or lexer bug. Always a finding; propose a regression golden under `test-data/goldens/<NN_category>/` where `<NN_>` matches the exit code the parser *should* have produced for this input.
   - **Hang / OOM** — termination bug, often grammar-level (left-recursion mishandling, ambiguous lookahead). Same treatment.
   - **Wrong exit code** — input was rejected, but the structured error landed in the wrong category (e.g. parse error reported as internal error). File the mismatch.
   - **Already covered** — the input maps to a `*.kio` source equivalent of an existing golden. Note but don't propose a duplicate.

## 5. Propose follow-ups

For each new finding that isn't a duplicate, produce a golden-case proposal:

- **Case name**: short and descriptive of the construct, not the input bytes (e.g., `parse_unclosed_string_at_eof`, not `crash_42`).
- **Bucket**: the exit-code category the corrected parser should produce (e.g. `test-data/goldens/11_parse_error/` for parse failures).
- **Files**:
  - `<case>/workdir/main.kio` — the input (or a hand-cleaned equivalent if the raw bytes are non-UTF-8 or non-printable noise that hides the underlying construct).
  - `<case>/run.sh` — typically `cd workdir && exec "$KIO_BIN" check` or `cd workdir && exec "$KIO_BIN" build "$KIO_TARGET"` depending on which phase crashes.
  - `<case>/expected.exit` — the target exit code.
  - `<case>/expected.stderr.ignore` if stderr text isn't part of the contract.
- **Fix landing**: the golden should be added *along with* the parser/lexer fix that makes it pass. Don't land the golden alone — it'd be a failing test from day one.

## 6. Run the fuzz suite once more if you fixed anything

If a fix-it directive accompanies this audit and code changes landed, rerun the fuzz with a longer budget (e.g. `sh reports/fuzz.sh --timeout=600`) to verify the fix and surface any next-deepest finding.

## How to report

Group findings into:

1. **New crashes from this run** — name each, classify by category (panic / hang / wrong exit code / duplicate), and propose the golden-case shape (name, bucket, expected status).
2. **Harness regressions** — if `reports/fuzz.sh` itself failed (cargo-fuzz install drift, kio-rs/fuzz build error, etc.), name the symptom and likely cause.
3. **Stale artifacts** — files under `kio-rs/fuzz/artifacts/` that correspond to *already-fixed* bugs. Propose cleaning them up so future runs' diff is accurate.

For each crashing input, cite the artifact path and the proposed golden bucket.

**Default: report only.** If invoked with a fix-it directive, follow [`ai/topics/audit-fix-mode.md`](../../topics/audit-fix-mode.md) — for each finding, add the regression golden and either fix the underlying parser/lexer bug (if scope permits) or open a follow-up note describing the fix needed.
