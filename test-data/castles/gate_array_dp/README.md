# gate_array_dp

This castle models maintenance-window planning for an access gate over a fixed
eight-day horizon. Each day has a traffic risk cost and a maintenance crew cost.
The planner fills a dynamic-programming table over `(day, gate-age)` states,
where deferring maintenance increases the age up to a cap and performing
maintenance resets the age to zero.

There is no stdin fixture. The fixture data is compiled into the program as two
mutable host arrays: daily traffic risk and maintenance cost.

Stdout prints a compact summary: the dimensions and constants, the final minimum
score, a few checkpoint cells from the DP table, and a small text/boolean sanity
line. The numbers are intended to make the result understandable without
inspecting the source.

What this adds to the corpus: this is a medium DP/data-structure-heavy array
workflow. It stresses mutable host arrays, repeated `array_get` / `array_set`
updates, nested `rec(loop)` walks, numeric roles, string formatting, and
testapi-array namespacing without relying on stdin, parser logic, or a simple
sort/knapsack skeleton.
