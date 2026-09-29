# `ROADMAP.md`

Trigger: editing `ROADMAP.md`.

`ROADMAP.md` is the public index of in-flight design threads. Audience: a casual passer-by, not the people doing the work. It is a contract about *intent*, not about design or scheduling.

- **Keep it consistent with `specs/`.** When a thread lands in a spec, remove or rewrite its roadmap entry in the same change. The roadmap must not list as "in flight" anything already settled.
- **Don't leak unsettled specifics.** Sketch direction; don't pre-commit to syntax, keywords, algorithms, or guarantees still under discussion. Speculative details belong in `scratchpad/`. Don't link to `scratchpad/` (gitignored, broken externally).
- **No bookkeeping.** Drop phrases like "currently deferred," "not an immediate priority," "open question is X." Omit speculative threads rather than annotating their status. Order of the file conveys rough priority.
- **Adding an entry needs its own authorization.** ROADMAP is a public commitment about intent; adding a thread is a publication decision, not a derivation from work happening elsewhere. Don't promote a scratchpad note, an audit recommendation, or "this seems like a thread that should exist" into a ROADMAP entry without an explicit go-ahead. Scratchpad notes are unrestricted; promotion to ROADMAP is not.

Errors of omission are cheap; errors of premature commitment or visible bookkeeping are expensive.
