# The no-leak rule

Pointer: read when writing or reviewing any checked-in **public** file other than a hand-written `docs/blog/*.md` post body. Every governed file is published — clones, GitHub, outside readers — and must read as if written for an outsider who has never seen this machine, this session, or this user. Session/machine/user-specific detail that creeps into a tracked file is *leaked context*: noise to an outside reader, and often broken (a path they don't have) or stale (a date that meant "today" the day it was written).

The repo already states three specific cases of this rule:

- **`scratchpad/` references** — [`scratchpad.md`](scratchpad.md): nothing checked in may name the gitignored notes directory.
- **`ROADMAP` hygiene** — [`roadmap.md`](roadmap.md): no bookkeeping phrasing, no session-relative framing.
- **Comment discipline** — [`comments.md`](comments.md): no task/fix/caller context, no historical residue.

This page is the general principle those three specialize, plus the full catalogue of leak classes and the scope the rule governs.

## Scope

The rule governs the **public project artifacts** — the files a clone publishes and an outside reader consumes — with one explicit exception: hand-written `docs/blog/*.md` post bodies are outside this rule. Their authorial context may be personal, session-relative, or process-oriented, and they may link to any tracked repository path. Their separate stability rule requires GitHub file and directory links to pin a full commit ID rather than a moving branch or tag.

- `specs/`
- `docs/` except `docs/blog/*.md`
- `README.md`, `ROADMAP.md`, `INSTALL.md`
- `kio-rs/` source and its comments (and any future compiler crate)
- `ci/`
- `test-data/`
- commit messages

The **agent-guidance layer** may discuss local workflow mechanics when that is its subject matter, but it is not a dumping ground for incidental session or machine detail. Prefer portable descriptions ("the configured shared cache", "sibling worktrees") over concrete local paths, host-specific values, or session state. Put concrete local configuration in untracked local files.

- `AGENTS.md`
- `ai/topics/` (e.g. [`local-tools.md`](local-tools.md), [`local-ci.md`](local-ci.md), and [`local-performance.md`](local-performance.md) describe local workflow mechanics)
- `ai/skills/`
- `.gitignore`

These files are instructions *to an agent operating in this environment*, not artifacts published to outside readers, so they may describe agent workflow. They still should not name concrete local paths, machine specs, user-specific preferences, or session state unless the file's subject cannot be explained without that detail.

## Leak classes

Each class below is context that belongs to the session/machine/user, not to the published artifact. In a public file, each is a finding.

- **Scratchpad paths** — `scratchpad/<anything>`. Gitignored, so broken for any external reader, and leaks internal thread naming. The dedicated case: [`scratchpad.md`](scratchpad.md).
- **Machine / box specs** — core counts, RAM, CPU arch, "on a small box", "this 8-core machine". Properties of the developer's hardware, irrelevant to the artifact. (Performance characteristics that are a *property of the code or algorithm* — "O(n) in the module count" — are fine; the leak is naming *this* box.)
- **Worktree / filesystem paths** — `/tmp/…`, absolute paths into a developer's checkout, any sibling-worktree path (whatever parent-directory convention a maintainer uses). The reader's filesystem looks nothing like this.
- **User identity and private rationale** — the user's name or email in prose; the *why-the-user-wanted-it* (cost, business pressure, personal preference, "to save money", "because the user dislikes X"). Describe what changed and the technical reason, never the user's private motive. The comment-discipline case names the comment-specific form ([`comments.md`](comments.md): no task/fix/caller context); this generalizes it to every public artifact and to private *motive*, not just task context.
- **Agent-orchestration mechanics** — "we ran N agents", "the integration agent merged", agent/session IDs, fan-out narration, "in parallel across worktrees". The artifact records *what the code does*, not *how the work was coordinated*. Orchestration belongs in the agent-guidance layer (for local running, [`local-performance.md`](local-performance.md)), not in a spec, comment, or commit message.
- **Agent-guidance links from public docs** — outside the exempt blog post bodies, public reader-facing files must not point readers into `AGENTS.md`, `ai/topics/`, or `ai/skills/`. Those files are for agents and maintainers operating inside this repo, not part of the public documentation surface. If the target material is genuinely user-facing, move or summarize it in `specs/`, `docs/`, `README.md`, `TESTING.md`, or `CONTRIBUTING.md`; otherwise omit the link.
- **Session-relative dates and framing** — "today", "this week", "currently", "as of this session", "recently", "now we". A published file is read at an unknown future time; a session-relative anchor is stale the moment the session ends. State the durable fact (a version, a concrete date when one is genuinely needed, an unconditional present-tense description), not the relative one. The `ROADMAP` case ([`roadmap.md`](roadmap.md)) names the in-flight / bookkeeping form of this.
- **Machine-state diagnoses phrased as guidance** — "X isn't on PATH here, so export …", a fixed remedy for one box's filesystem layout, an env-var recipe that assumes one machine's cache location. The durable form is symptom-conditional ("if mise-provisioned tools aren't found in non-interactive shells, check that the shims directory is on that shell's PATH") or a pointer to the owning tooling doc; the machine-state instance belongs in the session report or untracked local notes, never in the repo.
- **Maintainer-preference mandates** — one person's workflow choice stated as a universal rule: "always develop in sibling worktrees", "never run more than one build", an alias or personal global skill assumed present. Different maintainers on different machines have different workflows; tracked guidance states repo facts and conditional advice, not one workflow as law.

## The portability filter

AGENTS.md § Universal rules — "Tooling friction feeds back into the instructions" requires every proposed instruction update to pass this filter; it is also the day-to-day test for anything written into the agent-guidance layer:

1. **Fresh-clone litmus** — the sentence must be true and useful on a fresh clone on any machine. "`ci/all.sh`'s baseline broad selector is spelled `SAMPLE_IMPL`" passes: a repo fact, verifiable from the tree. "mise isn't on PATH, so export the shims directory first" fails: a diagnosis of one machine's shell setup.
2. **Symptom-conditional phrasing** — portable environment guidance is keyed on symptoms, not setups: "if X fails with Y, check Z", never an imperative that assumes a particular machine's state.
3. **Route the rest** — what fails the filter still gets recorded, just not here: surface it in the session report for the maintainer, or keep it in untracked local notes. When a whole class of environment issue recurs, its generic symptom-conditional form can land in `INSTALL.md` or [`local-tools.md`](local-tools.md); [`audit-install`](../skills/audit-install/SKILL.md) and [`audit-no-leak`](../skills/audit-no-leak/SKILL.md) keep those honest.

## Judgment

A few words overlap with legitimate technical vocabulary — "now" in a state-machine description, "parallel" in a concurrency spec, a `/tmp` path in a doc that genuinely documents temp-file behavior, "today" inside a quoted example. The rule targets *leaked session/machine/user context*, not the words themselves. Classify per hit: is this naming the developer's environment / session / motive (leak), or describing the artifact's own subject matter (fine)?

[`audit-no-leak`](../skills/audit-no-leak/SKILL.md) sweeps the governed public artifacts for these classes, excluding blog post bodies, and classifies each hit; on the agent-guidance layer it applies the narrower portability checks (identity, machine-state, and preference-mandate classes) rather than the full public-artifact bar.
