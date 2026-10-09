# Writing Kio with an AI agent

Use this workflow when writing Kio in any project. Follow the project's own
conventions; the preferences here are defaults, not additional language rules.

## Finding the documentation

The installed compiler carries documentation matching its version. Read it
without a source checkout or network connection:

```sh
kio manual cat docs/README.md
kio manual tree
kio manual cat KIO-FOR-AGENTS.md
```

[The documentation catalog](docs/README.md) routes to the language and tooling
tutorials, topic guides, and host integration guides. Start there for ordinary
authoring. [The specifications](specs/) give authoritative rules for precise
questions; for example:

```sh
kio manual cat specs/language.md
```

Relative links in these files describe the bundled tree, not the consuming
project's current directory. Use `kio manual cat` with the resolved bundled path,
or export the tree for browsing and searching with local tools. Run
`kio manual export --help` for archive formats and extraction commands.

## Writing and checking

- Learn the package layout from [the package guide](docs/guides/pkg.md), and
  use [the tooling tutorial](docs/tutorials/tooling.md) for the development loop.
  Check changes with `kio check`, format with `kio fmt`, and verify laws with
  `kio test` as appropriate to the project.
- Prefer surface syntax in ordinary programs. Use the tutorials and guides
  for tuples, labels, rows, branching, recursion, and imported elaborators;
  raw intrinsics belong in investigations of the explicit core.
- Default to qualified imports (`import path/to/module as p;`) and calls
  through the alias. Selective imports are useful for elaborators, operators,
  and names intentionally forming the module's local vocabulary. See
  [the language tutorial](docs/tutorials/language.md) and
  [UFCS spelling choices](docs/guides/ufcs.md#choosing-a-spelling).
- Export deliberately: keep helpers private, adding `pub` when another module
  or a host-facing package surface needs the item.
- Reuse a library's public API when shared nominal identity matters. Use
  [the library adoption guide](docs/guides/using-libraries.md) for dependencies;
  do not clone a public abstraction merely to avoid understanding its API.
- When source behavior is surprising, inspect the explicit Kio' output using
  [the debugging guide](docs/guides/debugging-with-kio-prime.md). Keep ordinary
  source readable rather than copying lowered intrinsic trees back into it.

## Resolving uncertainty

Consult guides first and specifications for exact rules. If they contradict
each other, report the discrepancy with the relevant paths and compiler version;
the specifications define the contract. Also report ordinary language or idiom
guidance that is only discoverable in a specification or these instructions,
when the public guides should explain it.

Reproduce an alleged compiler limitation with a small example before inheriting
a workaround. Report genuine defects instead of presenting their workarounds
as Kio idioms. Distinguish a language requirement from a style preference, and
avoid changing unrelated project conventions while fixing an example.
