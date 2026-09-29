# Priority Window Scheduler

This castle evaluates three contiguous maintenance windows and chooses the one
with the highest priority score. The window facts are embedded in Kio source;
there is no stdin fixture and no dependency.

Stdout prints each candidate window, the selected window, and the number of
rejected alternatives. Scores are computed from priority impact minus risk and
handoff penalties.

What this adds to the corpus: a dynamic-planner-shaped scheduler using the
`testapi-arith-collection` host surface. It exercises record-shaped scoring,
candidate comparison, nested arithmetic, and report composition.
