# semaphore_signal_card

A flag-semaphore reference card. The program renders the fixed message
`SEMAPHORE` as a printed card: one row per letter giving the two arm bearings, a
crossed-arms marker, and a mnemonic, followed by a footer of aggregate facts
about the whole message.

The castle runs on the `testapi-print` protocol, whose entire host surface is a
single function, `print`. There is no arithmetic, no string comparison or
concatenation, no host loop, and no array. Everything above `print` - the record
model, the boolean algebra, the folds over the message, the rendering - is
ordinary in-language Kio.

## What it models

Arm bearings are the **signaller's own** arms, placed on the eight compass
points: `S` is the arm down at the side (rest), `N` is straight up, and `W` / `E`
are horizontal. Letters are built in circles: one arm parks while the other
sweeps.

The card's alphabet covers ten letters - A, E, H, L, M, O, P, R, S, T - of which
the message uses eight. Each letter is a labeled record product carrying its
glyph, its left and right arm bearings, a short mnemonic, and three `Bool` facts:

- `crossed` - an arm reaches across the mid-line (the left arm at an easterly
  bearing, or the right arm at a westerly one). Of the ten letters, only `M` and
  `S` do.
- `high` - at least one arm sits above the horizontal, at `NW`, `N`, or `NE`.
- `vowel` - the letter is a vowel.

The three facts are **recorded per letter, not derived** from the bearing
strings. This host has no string comparison, so a letter cannot inspect its own
arms; the facts are part of the letter's data.

A letter's **shape class** is the pair `(crossed, high)`, printed as two
characters: `X` or `-` for crossed, then `H` or `-` for high. Two letters in the
same class throw a similar silhouette, which is what makes them easy to confuse
when read at speed.

## How it is organized

| module | what it holds |
| --- | --- |
| `alphabet` | the `Card` labeled record product and the ten letter constructors |
| `logic` | `not` / `and` / `or` / `xor` / `same` over `Bool`, written with `if` / `else` |
| `message` | the fixed nine-card product spelling `SEMAPHORE`, and the aggregate facts as folds over its slots, returned as a `Facts` record |
| `render` | the print-only rendering: the header, one row per card, the shape-class line, the footer |
| `testapi/main` | `main`: destructure the message, print the header, the nine rows, and the footer |
| `testapi`, `testapi/io` | the host surface - `Bool`, `String`, and `print` |

Records cross every one of those boundaries: `Card` is built in `alphabet`, read
in `message` and `render`, and threaded through `main`; `Facts` is built in
`message` and read in `render`.

Because the host supplies no loop, the message is a fixed product of nine cards
and each aggregate is a fold written out over its slots. Because the host
supplies no string concatenation, a rendered row is a run of `print` calls -
`print` adds no newline, so each row ends by printing `"\n"` itself.

## Input shape

There is no `input.stdin`. The message and the alphabet are compiled into the
program.

## Output shape

A header naming the card and the message, then one row per letter of
`SEMAPHORE`:

```text
letter | left / right | crossed | mnemonic
```

The crossed marker is `[X]` when an arm reaches across the body and `[ ]`
otherwise. The footer prints the shape class of each letter in message order,
then the three aggregate facts, each rendered `yes` or `no` by the program's own
bool-to-word function:

- **contains a vowel** - the `or`-fold of `vowel` over the message. `SEMAPHORE`
  carries E, A, O, E, so `yes`.
- **even arm crossings** - the negation of the `xor`-fold of `crossed`. The
  message crosses twice, at `S` and `M`, so the parity is even: `yes`.
- **neighbours distinct** - the `and`-fold, over the eight adjacent pairs, of
  "these two letters differ in shape class". `SEMAPHORE` has the run A, P, H, O
  all in class `--`, so `no`. The shape-class line above the facts shows the run
  directly.

What this adds to the corpus: the first castle on the print-only tier - an
entire multi-module program whose host surface is a single `print`, with every
other thing in-language. No arithmetic, no comparisons, no string helpers, no
host loop, no arrays, and no dependency, so no elaborator library and therefore
no sums: the whole program is built from labeled record products, `Bool` with
`if` / `else`, and function composition across modules. It is the corpus's
minimal-host extreme, and it pins that a structured program with real derived
logic - a boolean algebra, three folds over a fixed sequence, records crossing
five module boundaries - compiles and runs against a near-empty host.
