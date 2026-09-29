# symbolic_units_reducer

`symbolic_units_reducer` models a small symbolic unit expression language. The
program builds a fixed recursive expression tree from meters, seconds,
kilograms, products, quotients, and powers, then reduces each expression to
exponent totals for the three base units. Products add exponents, quotients
subtract them, and powers multiply them.

The castle reads no input. Its `run.args` selects the `testapi-arith-collection`
runner protocol, so stdout is produced entirely by the Kio program. Each case
prints the source expression, the reduced exponent summary, a weighted score,
and whether the expression reduced to a dimensionless unit.

What this adds to the corpus: this is a medium symbolic evaluator with a
recursive label-grounded term tree, recursive sum dispatch, a path dependency
on the elaborator POC for `match!` and spine widening, and a numeric reduction
pass over a fixed expression set.
