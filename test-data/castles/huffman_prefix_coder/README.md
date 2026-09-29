# huffman_prefix_coder

Builds a Huffman prefix code for a fixed message, uses it, and reports the
construction and its checks.

The program counts letter frequencies, merges the two lightest nodes over and
over until a single tree is left, reads a codeword off the root path to each
leaf, encodes the message, decodes it back through the tree, and reports its
46-bit cost against a fixed-width baseline. The round trip checks the table and
tree agree for this message; the Kraft equality checks the codeword lengths
account for the complete leaf tree.

There is no `input.stdin`. The message and the alphabet are fixtures in the Kio
source; stdout is the whole transcript.

## The message

The message is the palindrome **"a man a plan a canal panama"** with the spaces
dropped — 21 letters over a 6-glyph alphabet. `huffman/message` builds it word by
word, so the source still reads as the sentence it encodes:

```text
a  man  a  plan  a  canal  panama     ->  amanaplanacanalpanama
```

Each glyph is a small integer. The symbol codes are the alphabet's indices,
assigned in glyph order, and they are part of the coder's contract rather than a
display detail — the tie-break rule below reads a leaf's ordering key straight
off the symbol code.

| code | 0 | 1 | 2 | 3 | 4 | 5 |
| --- | --- | --- | --- | --- | --- | --- |
| glyph | `a` | `c` | `l` | `m` | `n` | `p` |

## The frequency table

| glyph | `a` | `c` | `l` | `m` | `n` | `p` |
| --- | --- | --- | --- | --- | --- | --- |
| count | 10 | 1 | 2 | 2 | 4 | 2 |

Three glyphs occur exactly twice. That is the point of this message: the coder
cannot get through the first two merges without breaking a tie, and a later merge
ties a leaf against an internal node of the same weight. **With ties, a Huffman
tree is not unique.** A different valid tie-break can produce different
codewords and a different bit string while still round-tripping, satisfying
Kraft equality, and attaining the same cost. The exact build log and code table
pin this implementation's choice.

## The tie-break rule

There is no host priority queue, so `huffman/tree` keeps the node list sorted and
re-inserts each merged node at its place. The order is **ascending
`(weight, key)`**, where:

- a **leaf's** key is its **symbol code** (0-5);
- the **k-th merged node's** key is `alphabet_size + k` — so 6, 7, 8, … in
  creation order.

Three consequences, and the run exercises all three:

- equal-weight **leaves** order by symbol code — the initial queue puts `l(2)`
  before `m(2)` before `p(2)`;
- an equal-weight **leaf precedes an internal node**, because every leaf key sits
  below every internal key — merge 3 takes leaf `n(4)` over the equally heavy
  `*7(4)` built one merge earlier;
- equal-weight **internal nodes** order by creation.

Each merge removes the two front nodes. The **first** taken becomes the **left**
child and is therefore reached by bit **0**; the second becomes the right child,
reached by bit **1**.

## Reading the output

**The initial queue and the build steps.** A leaf prints as its glyph, an
internal node as `*` and its ordering key; the number in parentheses is always
the node's weight. So `3  *6(3) + n(4) -> *8(7)` reads: merge 3 took the node
keyed 6 (weight 3) and the leaf `n` (weight 4), and produced the node keyed 8,
weight 7. The two weights taken and the new weight are the merge.

**The code table.** One row per glyph, in symbol order so it reads against the
frequency table: the glyph, its count, its codeword length, and the codeword.
Every codeword is the path from the root to that leaf. Together with the exact
build log, this table pins the chosen tie-break.

**The compression report.** The baseline is what a fixed-width code would have
cost — every symbol spending `ceil(log2(6)) = 3` bits whatever its frequency — so
21 symbols cost 63 bits. The Huffman code spends 46. The host tier has no floats
and no division, so the saving is an integer permille and the per-symbol cost is
in milli-bits, both produced by a long division (`huffman/report`) written out in
adds, subtracts, and compares.

**The two checks.** *Round trip*: the decoder never consults the code table — it
walks the tree one bit at a time and emits a symbol whenever it lands on a leaf —
so agreeing on all 21 symbols checks that the table-derived encoder and the
tree-derived decoder agree for this fixture. The root-to-leaf construction
itself establishes prefix-freeness: no distinct leaf path can be a prefix of
another. *Kraft*: summing `2^-length` over the recorded codeword lengths must
land exactly on 1. Scaled by `2^longest` to stay in integers, the six terms must
sum to exactly 16, checking that the lengths account for the complete leaf tree.
Kraft equality does not independently establish the tie-break, prefix-freeness,
or optimality; another valid tie-break can pass both checks. The build log and
code table, rather than either verdict, pin the selected tree.

## Package shape

`testapi/main` is the entry point and the orchestration; the domain lives under
`huffman/`:

| module | what it holds |
| --- | --- |
| `huffman/list` | the cons list every sequence here is an instantiation of — the tier has no host array |
| `huffman/text` | string plumbing; the tier has no `string_len`, so every column width is derived arithmetically |
| `huffman/message` | the alphabet, the glyph table, and the message |
| `huffman/freq` | the frequency tally, one walk over the message |
| `huffman/tree` | the node sum, the pinned order, and the greedy merge |
| `huffman/code` | code assignment by tree walk, and the code table |
| `huffman/codec` | encode, decode, and the round-trip comparison |
| `huffman/report` | long division, `ceil(log2 n)`, the Kraft sum |
| `huffman/render` | the transcript |

What this adds to the corpus: the corpus's first Huffman prefix-code
construction — greedy tree building with an exact build log and code table that
pin the tie-break, code assignment by tree walk, a round-trip decode, a
Kraft-equality length-accounting check, and a fixed 46-bit cost for the message.
