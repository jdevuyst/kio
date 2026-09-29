# `scratchpad/`

Trigger: editing under `scratchpad/`, or referencing scratchpad content from a checked-in file.

`scratchpad/` is a gitignored, local-only design-notes directory. Holds **pre-settled** material — proposals under discussion, deferred items, motivation that doesn't belong in a public spec. `specs/` is what's settled; `scratchpad/` is the rest.

- One file per thread, named descriptively (`<topic>.md`).
- When a thread settles, migrate its content into the appropriate `specs/` page in the same change and delete the scratchpad file. Don't leave parallel sources.
- If `scratchpad/` isn't present in your working copy, that just means no local notes exist. Never invent files based on assumed content.
- **Nothing checked into the repo may link to or reference `scratchpad/`** — not `README.md`, not `ROADMAP.md`, not `specs/`, not `docs/`, not `test-data/`, not source comments, not commit messages. The directory is gitignored, so any such reference is broken for external readers (clones, GitHub) and leaks internal naming. If a checked-in file would benefit from naming a scratchpad doc, the right move is to migrate the content out of `scratchpad/` first. This is the scratchpad-specific case of the general no-leak rule — see [`no-leak.md`](no-leak.md) for the principle and the other leak classes.
