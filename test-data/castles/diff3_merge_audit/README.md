# diff3_merge_audit

A three-way line merge, diff3-style. The program reads three versions of
one config-like document from stdin — the common ancestor and the two
edited copies — reconciles them into a single merged document, and prints
an audit of every decision it made.

## What it models

- **Two alignments** (`diff3/lcs`). Each edited copy is aligned against the
  base with a line-level longest-common-subsequence dynamic program. The
  table is solved in the suffix orientation — `L(i, j)` is the LCS length of
  `base[i..]` against `other[j..]` — so the traceback runs forward and emits
  aligned line pairs in document order. This tier has no host array type, so
  the table is a list of rows and a row is a list of cells; a row reads the
  row below it and its own next column, which is why rows are built from the
  last one upward and each row from its right edge inward.
- **Anchors** (`diff3/merge`). A base line is an anchor when *both*
  alignments kept it. Anchors are the only lines the merge can trust, and
  the text between two consecutive anchors is a chunk that one side, both
  sides, or neither side rewrote.
- **A verdict per region.** A maximal run of anchors that advances all three
  cursors in lockstep is a `stable` region. Every chunk between runs is
  judged in diff3's order: a side that still equals the base did not touch
  the chunk, so the other side's text wins outright (`ours-changed` /
  `theirs-changed`); two identical rewrites are `both-same`; two genuinely
  different rewrites of the same base text are a `conflict`.
- **Emission** (`diff3/render`). Each region contributes its winning text to
  the merged document; a conflict contributes all three texts, fenced by
  marker lines.

Because a chunk spans everything between two anchors, two edits with no
surviving line between them land in the same chunk and are judged together.
That is the algorithm's decision, not an approximation of it: adjacent
rewrites are one conflict, not two.

## Input shape

`input.stdin` is a document, not a command stream. A line that is exactly
`#base`, `#ours`, or `#theirs` opens a section, and every following line
belongs to that section verbatim until the next marker:

```text
#base
<the common ancestor, one line per line>
#ours
<our edited copy>
#theirs
<their edited copy>
```

Markers are matched as whole lines, so ordinary document text may begin with
`#` — the fixture's `# service config` and `# end` lines are content, not
markers. A line ahead of the first marker belongs to no section and is
dropped.

The checked-in fixture is a twelve-line service config edited on both sides.
Between them the three sections exercise every verdict the merge can reach:

| base line | ours | theirs | verdict |
| --- | --- | --- | --- |
| `workers = 4` | `workers = 8` | unchanged | ours-changed |
| `retries = 2` | *deleted* | unchanged | ours-changed |
| `log_level = info` | `= debug` | `= debug` | both-same |
| `tls = disabled` | `= required` | `= optional` | **conflict** |
| `queue_depth = 16` | unchanged | `queue_depth = 64` | theirs-changed |

Everything else is untouched on both sides and merges as stable text. Each
edited line is separated from the next by at least one line both sides kept,
so every edit lands in its own chunk.

## Output shape

The program owns all output; the runner echoes nothing. The header repeats
the three section sizes so the report stands on its own, then two sections
follow.

`--- merged ---` is the merged document. A conflict region prints as three
fenced blocks, each marker introducing the block below it. The marker lines
carry a two-space indent: they remain recognizable as conflict sentinels while
staying ordinary rendered document text.

```text
  <<<<<<< ours
<our text>
  ||||||| base
<the base text neither side agreed on>
  >>>>>>> theirs
<their text>
```

`--- audit ---` reports what the merge did, in numbers a reader can check
against the fixture by hand:

```text
lcs base/ours     length of the base-vs-ours longest common subsequence
lcs base/theirs   the same against theirs
anchors           base lines both alignments kept
regions           regions the document decomposed into
stable            regions no side touched
ours-changed      regions only we rewrote (a rewrite or a deletion)
theirs-changed    regions only they rewrote
both-same         regions both sides rewrote the same way
conflict          regions the merge could not reconcile
lines in          lines read across the three sections
lines out         lines in the merged document, marker lines included
marker lines      three per conflict region
status            merged, or conflicted when any region conflicted
```

The two LCS lengths differ (8 and 9) because ours deletes a line that theirs
keeps, so the two alignments are genuinely independent solves rather than
one result reported twice.

## What this adds to the corpus

The first castle whose stdin fixture is a *document* rather than a command
stream or a settings header — the input is the data being computed over, and
its three sections are recovered by whole-line marker recognition. It is also
the corpus's first diff/merge algorithm: longest-common-subsequence dynamic
programming over cons lists in a tier with no host array, run twice, with the
two alignments intersected into anchors and three-way conflict detection over
the chunks between them.
