# polynomial_list_lab

A computer-algebra workbench over fixed polynomials whose coefficients use the host's
`I32` type. It builds five fixture polynomials, runs the polynomial operations on
them, runs guarded division cases, and then — the point of the exercise — checks five
algebraic laws against the fixture results at runtime and prints a pass/fail verdict for
each.

The polynomials are held in the [`list`](../../poc/list/) POC's `List`, so the
dependency does the real work rather than decorating the program: a polynomial *is*
a coefficient list, and every operation is a walk, fold, or rebuild of that list.
The [`elab`](../../poc/elab/) POC supplies `match!` and `widen_sum!`.

The program reads no input. It takes no seed and ships no `input.stdin`.

The host supplies the arithmetic for `I32`. Every coefficient and intermediate value
in this run stays within the signed-32 literal range. The eleven printed checks are
finite examples over those fixtures. They do not prove behavior for arbitrary inputs,
arbitrary-precision integers, or host overflow.

## The representation and its normal form

A polynomial is its **coefficient vector, low degree first**: the element at index
`i` is the coefficient of `x^i`, so the head of the list is the constant term.
`3x^2 - 2x + 5` is `[5, -2, 3]`.

The **normal form trims trailing zero coefficients** — the ones at the far end of
the vector, which carry the high degrees. Two consequences make the rest of the
program simple:

- The zero polynomial has exactly one spelling, the empty vector. `[0, 0, 0]`,
  `[]`, and `p - p` are all the same value.
- `degree` is well defined: it is `length - 1`, and the highest-index coefficient is
  guaranteed non-zero. The zero polynomial gets degree `-1`, which keeps `degree`
  total and lets `deg(remainder) < deg(divisor)` be the single stopping test in long
  division without a special case for a remainder that has vanished.

`poly/core` enforces this. The `Poly` newtype's constructor and projector are
private, so `of_coeffs` — which trims — is the only door in. The invariant is
structural, not a convention anyone has to remember.

Polynomial equality is coefficient-vector equality, walked directly in `poly/core`.
It is deliberately **not** defined as "is `a - b` the zero polynomial": the laws
below judge the arithmetic, so the equality they are judged with must not be built
out of the arithmetic under test. A subtraction that always returned zero would
otherwise make every law pass.

## The operations

| operation | module | how |
| --- | --- | --- |
| `add`, `sub` | `poly/arith` | one coefficient-wise walk parameterized by the host arithmetic operation; the shorter vector's missing coefficients read as zero |
| `scale(p, k)` | `poly/arith` | `map` over the vector; scaling by 0 collapses to the zero polynomial because the normalizer trims the all-zero vector |
| `shift(p, k)` | `poly/arith` | for `k >= 0`, `p * x^k` — `k` zero coefficients pushed under the vector |
| `mul` | `poly/arith` | convolution, accumulated as a running sum of shifted, scaled copies of the right operand |
| `eval(p, x)` | `poly/calculus` | Horner's rule, as a `foldr` of `c + x * acc` over the vector |
| `derivative` | `poly/calculus` | drop the constant coefficient, multiply each survivor by its original degree |
| `divmod` | `poly/division` | school-book long division, guarded (below) |
| `show` | `poly/render` | ordinary notation — `3x^2 - 2x + 5` |

Rendering is fiddlier than it looks and the fixtures are chosen to exercise it: the
sign travels with the separator (a bare `-` only on the leading term, an infix
` + ` / ` - ` after), zero coefficients are omitted entirely (`[-4, 0, 1]` is
`x^2 - 4`), a unit coefficient loses its `1` above degree 0 but keeps it at degree 0
(`-x^2 + 4`, but `... + 1`), and the zero polynomial is `0`.

## The coefficient-division choice

The host tier this castle runs under supplies addition, subtraction, multiplication
and the integer comparisons — **but no division and no modulo**. Long division needs
to divide the running remainder's leading coefficient by the divisor's, so that
division had to come from somewhere.

**The choice: implement exact division for the fixtures in Kio, by repeated subtraction.**
For the fixture-sized coefficients, `poly/integers.exact_div(a, b)` counts how many
whole `b`s come out of `a`, works on magnitudes and reapplies the sign afterwards,
and returns `()` when anything is left over — or when `b` is zero. There is no
rounding or truncation in the program.

The alternative would have been to use only monic fixture divisors, where coefficient
division is division by 1. Exact division also covers the checked non-monic case whose
leading coefficients divide evenly. The fixtures exercise both shapes: `p / d` uses a
monic divisor, while `s / e` is the case a monic-only workbench could not express.

So `divmod(p, divisor)` returns a sum, dispatched with `match!`:

- `divided (quotient, remainder)` — for the checked cases, the identity
  `p = quotient * divisor + remainder` with `deg(remainder) < deg(divisor)` holds.
- `not_divisible reason` — the division was refused, with the reason. An integer
  coefficient step whose leading coefficients do not divide exactly has no result in
  this workbench; its host boundary supplies neither reciprocals nor rounding.

Both arms are exercised. `p / e` is the refusal.

## The algebraic laws

Every law is checked against the fixture polynomials on every run, and each check
prints its verdict and the value it actually produced. A failed instance exposes a
disagreement in the composed implementation rather than a tolerance to widen.

| law | claim | what a failure catches |
| --- | --- | --- |
| additive inverse | `(p + q) - q == p` | a coefficient-wise walk that mis-pads the shorter vector. `add` and `sub` must agree about where a missing operand's implicit zeros are; if they disagree, the round trip drops or invents a coefficient. |
| degree of product | `deg(p * q) == deg p + deg q` (non-zero `p`, `q`) | a convolution that drops its last partial product, or a normalizer that trims one zero too many. Both shorten the vector; the fixture coefficients keep the expected leading product in range and non-zero. |
| division identity | `quotient * divisor + remainder == p` | errors in long division. The quotient and remainder are re-multiplied and compared against what went in, so the division is judged against multiplication and addition rather than against itself — a wrong `exact_div` sign, a wrong shift, or a mis-cancelled leading term surfaces here. |
| product rule | `(p * q)' == p' * q + p * q'` | the one law that ties the derivative to multiplication. An off-by-one in the degree counter — differentiating to `sum (i+1) c_i x^(i-1)`, or shifting the vector the wrong way — survives the derivative-only checks in this fixture set. |
| eval homomorphism | `eval(p * q, x) == eval(p, x) * eval(q, x)` | a transposed index in Horner or in the convolution. Horner folds the coefficient vector from one end and convolution builds it from the other; if either reads the vector backwards, the two sides of this equation stop agreeing. Checked at `x = -1`, `2`, `3` because at `x = 2` the product vanishes and would not exercise a nonzero result. |

Within this fixture set, starting the derivative's degree counter at 0 instead of 1
fails exactly the two product-rule instances and nothing else; replacing Horner's
multiply with an add fails exactly the three homomorphism instances.

## The fixtures

| name | polynomial | vector | why it is here |
| --- | --- | --- | --- |
| `p` | `3x^2 - 2x + 5` | `[5, -2, 3]` | the general case |
| `q` | `x^2 - 4` | `[-4, 0, 1]` | an interior zero coefficient the renderer must drop, and a unit leading coefficient it must not print |
| `d` | `x - 2` | `[-2, 1]` | monic — divides `p` with a non-zero remainder |
| `e` | `2x + 1` | `[1, 2]` | not monic — its leading 2 does not divide `p`'s leading 3, which is the refusal |
| `s` | `6x^2 + 7x + 2` | `[2, 7, 6]` | `(2x + 1)(3x + 2)` — the checked division by the non-monic `e` succeeds exactly |

`p / d` gives quotient `3x + 4` and remainder `13`, and `13` is `p(2)` — the
remainder theorem, visible in the transcript.

## Reading the output

Six sections, none of which needs the source to be understood:

1. **fixtures** — each fixture rendered, with its degree and its raw coefficient
   vector (rendered by the list library's own `to_string`).
2. **normal form** — the trimming demonstrated: a padded vector normalizes back,
   and `p - p` and `scale(q, 0)` both land on the canonical zero at degree `-1`.
3. **operations** — every operation applied to the fixtures.
4. **evaluation** — `p`, `q` and `p * q` evaluated at three points.
5. **long division** — the two `divided` cases and the one `not_divisible` case,
   with its reason.
6. **algebraic laws** — one line per law instance: verdict, law, claim, and the
   value the check produced. Then the tally.

## Layout

```text
workdir/
  polynomial_list_lab.pkg.kio   package file: build targets and the host bridge
  list.dep.kio, elab.dep.kio    the two path dependencies
  list_host.kio                 supplies the `list` package's own host requirements
  testapi.kio, testapi/         the host boundary and `main`
  poly/core.kio                 the representation, the normal form, equality
  poly/integers.kio             abs, negate, exact division by repeated subtraction
  poly/arith.kio                add, sub, scale, shift, mul
  poly/calculus.kio             eval (Horner), derivative
  poly/division.kio             divmod and its guarded result sum
  poly/render.kio               polynomial notation and fragment-list joining
  poly/fixtures.kio             the five fixture polynomials
  poly/laws.kio                 the five laws and their instances
  poly/report.kio               the printed transcript
  list/, elab/                  the materialized dependency trees (not owned source)
```

`list_host` is what lets the `list` package run under this castle's host. The library
declares its own host requirements (`add_i32`, `loop`, `string_concat`, …) as `host`
items of its `list` module; re-rooted into a consumer, those would surface as extra,
unsatisfiable requirements on the consumer's boundary. `list.dep.kio` carries a
`rehost list/list to list_host;` clause, and `list_host` forwards each one to the
matching `testapi` capability.

What this adds to the corpus: a computer-algebra workbench —
polynomials as coefficient lists over the `list` POC, with convolution, Horner
evaluation, derivative, guarded long division, and five algebraic laws checked at
runtime against fixed inputs. It uses a POC collection as the *carrier of a
mathematical structure* rather than as a container of records: the list is the
polynomial, the normal form is a property of the list's shape, and the degree is its
length. It also derives a fixture-scale arithmetic primitive the host tier does not
supply — exact division, built in Kio from repeated subtraction.
