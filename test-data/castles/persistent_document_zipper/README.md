# Persistent document zipper

A command-driven editor for an ordered multiway document tree. Each node has
a title and an immutable list of children. A zipper holds the focused subtree
and a breadcrumb for each ancestor: its title, preceding siblings in reverse
order, and following siblings in document order. Parent navigation rebuilds
that one ancestor; root reconstruction repeats this through all breadcrumbs.

The editor starts with the empty `document` root. `input.stdin` contains one
command per line. `rename`, `append`, `before`, `after`, and `snapshot` consume
the next entire line as a nonempty name, including any spaces. Other commands
are `child` (first child), `parent`, `prev`, `next`, `root`, `delete`, `undo`,
`where`, `show`, and `snapshots`. End of input prints the current document and
every retained snapshot. There is no random seed.

`append` adds a last child and retains focus. `before` and `after` insert an
immediate sibling and retain focus. `delete` removes the complete focused
subtree and focuses its parent; the root cannot be deleted or given siblings.
Each successful edit saves the preceding zipper. `undo` restores that entire
zipper, including its focus; navigation, reporting, snapshots, and rejected
commands do not add undo entries. Snapshot names are unique and retain a
reconstructed root independently of undo history.

Stdout echoes commands and their operands, reports focus paths or domain
errors, and prints indented documents with node counts, undo depth, and the
number of snapshots. The fixture builds three document sections, edits a
grandchild, inserts on both sides of an interior sibling, deletes subtrees at
different positions, empties a parent, and undoes edits after navigation. The
`draft`, `review`, and `published` snapshots are rendered after subsequent
changes to demonstrate persistence and ordered ancestor reconstruction. Root
boundaries, missing children and siblings, duplicate snapshot names, empty
names, unknown commands, and a missing final operand are ordinary reports.

The package adopts the persistent `list` library for children, breadcrumbs,
history, snapshots, and traversal worklists. Its list host requirements are
rehosted through an ordinary adapter; imported elaborators provide control
flow and structural sums. The standard `testapi-compute-list-elab` protocol
supplies input, output, strings, integers, and iteration.

What this adds to the corpus: a persistent multiway tree editor centered on
local navigation and reconstruction from zipper contexts. Retained roots,
undo cursors, and a separate preorder worklist combine nested recursive data,
polymorphic collections, higher-order edit application, and line-oriented
command replay in a coherent stateful workflow.
