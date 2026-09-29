# gate_text_scanner

A compact structured text scanner over a fixed ASCII command script. The
program does not read stdin; it owns a checked-in script string inside
`scanner.kio`, slices known command and token positions with the `testapi-text`
string helpers, classifies each line, and prints a transcript.

## What it models

The script has six command-like lines:

```text
OPEN gate
SET mode=A
PING 007
BAD ??
LOCK gate
SET mode=B
```

The scanner is deliberately small because the selected protocol provides no
loop, stdin, or integer arithmetic surface. Instead of a general recursive
tokenizer, the castle uses fixed offsets into one command script and small
helpers for command words, payload slices, code values, and category strings.

## Input shape

There is no `input.stdin`. The fixture is the fixed script literal compiled
into the program.

## Output shape

The first line prints the script's byte-length classification. The next six
lines print one token/category result per command line. The final line prints a
checksum/count summary computed by composing the per-line category fragments.

## What this adds to the corpus

This castle adds the structured text scanner/parser family: no-stdin execution,
the `testapi-text` host surface, repeated `string_slice` / `string_eq` /
`string_len` use, fixed-position token inspection, and a checksum-like summary
assembled from classified command categories. It complements stdin-driven
replay-style castles by exercising text parsing through a fixed script fixture
and no recursive host loop.
