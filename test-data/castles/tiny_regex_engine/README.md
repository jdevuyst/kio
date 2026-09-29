# tiny_regex_engine

`tiny_regex_engine` is a medium fixed-fixture text-pattern interpreter. It
checks a small regex-like pattern language over ASCII candidate strings and
prints one diagnostic line per run. The supported fixture forms are literal
text, `.` wildcard, `?` optional character, `^` anchor, and a small
alternation group.

The castle uses the `testapi-text` runner protocol. It declares the protocol's
canonical text host surface under `testapi/*`: printing, integer and boolean
formatting, string concatenation, string equality, string length,
half-open slicing, and byte-at inspection. There is no `input.stdin`; all
patterns and candidates are checked-in fixtures in the Kio source.

Stdout is a compact transcript. Each case names the pattern and candidate,
then reports either a matched span plus details, or a miss reason with the
length or byte that drove the decision. The final line is a static summary of
the fixture batch.

What this adds to the corpus: this castle is a small interpreter-style text
engine rather than a formatter or scanner. It stresses imported structural
dispatch, label-shaped result values, byte-level string inspection, fixed
window matching, and a testapi-conformed host boundary across the JS, TS, Rust,
and Go backends.
