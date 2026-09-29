# turnstile_event_replay

This castle models a hosted access gate by replaying one ASCII event per input
line. The gate starts locked. `coin` unlocks one paid entry, `push` tries to
enter, `maint-open` holds the gate open for maintenance, and `maint-close`
returns it to locked service. A line that parses as a signed base-10 integer
is treated as a badge ID; IDs 1001 through 1003 are accepted and unlock one
entry. Any other line is reported as an unknown event.

`input.stdin` is the replay log. The program consumes the fixture until EOF
with `read_ascii_line`, prints one transcript line per event, and then prints
final counters: processed lines, entries, pushes, forced pushes, accepted
coins, accepted and denied badges, maintenance events, unknown events, final
gate mode, and whether a paid or badge unlock is still pending.

What this adds to the corpus: this is a stdin replay state machine with
fallible whole-line parsing, maintenance-mode control flow, labeled state, and
recursive `rec(loop)` input consumption over the `testapi-compute` host
surface. It stresses host I/O, text equality, string-to-integer parsing,
integer arithmetic and comparison, sum dispatch, and multi-backend emission
without relying on a case-specific host parser.
