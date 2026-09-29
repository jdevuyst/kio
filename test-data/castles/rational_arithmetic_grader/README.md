# rational_arithmetic_grader

`rational_arithmetic_grader` does exact arithmetic on fractions and uses it to
mark homework. Nine exercises each pair a question — an expression over rational
literals — with the fraction one student handed in for it. The program evaluates
each question exactly, compares the submission against the answer, and prints a
transcript and a class summary.

The castle reads no input. Its `run.args` selects the `testapi-bare-collection`
runner protocol, so stdout is produced entirely by the Kio program.

## The number tower

A **`Fraction`** (`rational/core`) is a numerator and a denominator exactly as
somebody wrote them down. Nothing about it is canonical: the denominator may be
negative or zero, and the pair need not be in lowest terms.

A **`Rational`** is a `Fraction` in canonical form — lowest terms, denominator
strictly positive. It is a `newtype` whose constructor is *private*, so `reduce`
is the only door into the type and no caller anywhere can mint a `Rational` that
breaks the invariant. `reduce` refuses a zero denominator, moves the sign onto
the numerator, and divides both parts by their greatest common divisor, which
`gcd` computes by the Euclidean algorithm over the two magnitudes. Keeping the
algorithm on non-negative operands is deliberate: whatever the host chooses to
do with `mod` of a negative number never reaches it.

The invariant then pays for itself twice. Equality is component-wise, because
two canonical pairs agree exactly when they denote the same number. And ordering
is plain cross-multiplication, because both denominators are positive and so
multiplying the comparison through by them cannot flip it.

`reduce` is also the *only* place the arithmetic is partial. `rational/arith`
divides by multiplying by the reciprocal — the same operation with the divisor's
two parts swapped — so a zero divisor puts its zero in the denominator, where
`reduce` already knows to report it. A zero-denominator literal and a division by
zero are therefore one fault and not two, which is right: a fraction bar *is*
division.

## The expression language

`rational/expr` declares `Expr = Lit | Add | Sub | Mul | Div`, a label-generated
sum whose four operator nodes each hold two `Expr` children. A `Lit` holds its
fraction *as written*; reduction is the evaluator's job.

`eval` walks the tree in a `rec(loop)` group and returns `Eval = Rational |
Division_by_zero` — a value, never a crash. One combinator threads both children
of a node into the operation and stops at the first fault, so a division by zero
found arbitrarily deep in a question rides out through every node above it
without any of them having to look for it.

## The exercises and the four verdicts

Each exercise (`rational/fixture`) is a question and a submitted `Fraction`.
`rational/grader` evaluates the question and marks the submission:

- **exact** — the submission is the right number *and* is already in lowest
  terms.
- **equivalent** — the submission is the right number but was not reduced. It is
  accepted, and the note says so: a student who wrote `2/4` where `1/2` was
  wanted, or who left the sign on the denominator as `2/-5`, is right.
- **wrong** — the submission is a different number. The note says whether it came
  out above or below the correct value. A submission with a zero denominator is
  not a number at all, so it lands here too.
- **undefined** — the *question* has no answer, because it divides by zero. This
  is a verdict on the exercise, not on the student, so it counts against neither.

The nine exercises reach all four verdicts and every corner of the reduction
logic: a gcd of 8 that the evaluator itself must find (`1/4 + 1/4` cross-
multiplies to `8/16`), a gcd of 12, a negative result whose sign must land on the
numerator, a submission whose sign must be lifted *off* the denominator, a
quotient that comes out whole and must print as `7` rather than `7/1`, a
zero-divisor question whose fault has to climb out through both of the
evaluator's short-circuit paths, and a submission that is not a number.

## Output shape

One block per exercise, then the summary:

```text
exercise <n>: <the question, fully parenthesized>
  correct: <the exact value, or `undefined`>
  submitted: <the student's fraction, exactly as written>
  verdict: <exact | equivalent | wrong | undefined> - <why>
```

The question is printed with every operator node parenthesized, so the grader
never has to defend a precedence table and the printed question is unambiguous
about the tree the evaluator walked. The submission is printed verbatim — `2/4`
stays `2/4` and `2/-5` keeps its negative denominator — because a transcript has
to show what the student actually put down, not what it reduces to. A canonical
value with denominator 1 prints as a bare integer.

The summary counts the four verdicts and states the class total under the
rubric: a fully reduced correct answer scores two points, an accepted but
unreduced one scores one, a wrong one scores none, and an undefined exercise
leaves the denominator entirely — a broken question is not allowed to count
against the class.

What this adds to the corpus: the corpus's first exact-rational number tower —
a Euclidean `gcd`, a canonical-form invariant a private newtype constructor makes
unbreakable, sign canonicalization on both the evaluator's and the student's
side, and reduction by an exact divisor. It is the corpus's first program in
which division by zero is an ordinary error *value* that propagates out of a
nested expression through a short-circuiting combinator rather than a crash or a
sentinel, and its first use of an equivalence relation (rather than equality) as
a grading judgement — accepting `2/4` for `1/2` and saying so. It is also the
first castle on the unsuffixed-arithmetic `testapi-bare-collection` tier, where
`add` / `sub` / `mul` / `div` / `mod` carry no `_i32` suffix while the
comparisons do, and the first to define its own `fn add` / `sub` / `mul` / `div`
over a domain type alongside those same-named host functions.
