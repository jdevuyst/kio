# command_transcript_linter

This castle scans a fixed ASCII command transcript and reports malformed
records. The transcript is embedded in `workdir/transcript.kio`; there is no
stdin fixture and no external dependency.

The scanner treats the transcript as six fixed-span records. It checks that
known four-letter opcodes have a colon separator, that retry records use the
five-letter `RETRY` opcode with its own separator position, and that retry
payloads do not start with an unsafe shell command.

Stdout prints the transcript size, one line for each detected issue, a boolean
retry-safety summary, and fixed accepted/rejected totals. The expected result is
one missing separator, one unsafe retry, one malformed opcode, three accepted
records, and three rejected records.

What this adds to the corpus: a structured text scanner/parser that uses only
the `testapi-text` host surface over a fixed fixture string. It exercises
module composition, fixed-span string slicing, string equality, formatted
counts, boolean rendering, and side-effectful printing without stdin or a host
loop function.
