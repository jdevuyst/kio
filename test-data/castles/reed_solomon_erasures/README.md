# Reed-Solomon erasure recovery over F17

This workbench encodes a message as a polynomial over the prime field F17,
evaluates it at distinct coordinates, and recovers the message and erased
symbols from a surviving subset. Coefficients are stored constant-first:
`[3, 5, 2]` means `3 + 5*x + 2*x^2`, with every operation modulo 17.

Encoding uses Horner evaluation. Recovery builds the Lagrange interpolation
polynomial from the first `k` surviving samples, multiplying its linear factors
and accumulating coefficient lists. The decoder receives only `k`, the
codeword coordinates, and the surviving `(coordinate, value)` pairs. It has no
access to the original message. A separate direct-power evaluator checks all
surviving samples, including any surplus ones, and reconstructs every erased
symbol. The workbench compares the recovered coefficients and complete
codeword with the original encoder and deliberately alters one recovered
symbol to demonstrate rejection by the independent evaluation check.

`input.stdin` contains whole-line ASCII tokens. A frame consists of:

1. A name, message dimension `k`, and codeword length `n`.
2. The `k` message coefficients, one integer per line.
3. The `n` evaluation coordinates, one integer per line.
4. A surviving-sample count, followed by that many coordinate/value pairs,
   with each integer on its own line.

`END` terminates the input. The framing bounds are `1 <= k <= 4`,
`1 <= n <= 8`, and zero through eight surviving samples. Valid codes require
`k <= n`, distinct coordinates in `0..16`, and coefficients and received
values in `0..16`. Surviving coordinates must be distinct members of the
codeword coordinates. At least `k` samples are required. Field operations
reduce after each step, so their integer intermediates are at most 256.
There is no random seed.

The successful fixtures use reordered nonconsecutive survivors, a cubic
message with one surplus survivor and wraparound field arithmetic, and a
constant polynomial. Negative frames exercise too few survivors, repeated
surviving coordinates, an out-of-field coordinate, and a contradictory
surplus sample. Framing errors stop input; domain rejections report the reason
and continue to the next frame. Every case is a normal successful process run.

Stdout lists the message, coordinates, received pairs, encoded and repaired
codewords, recovered coefficient list, erased pairs, and verification results.
Pair notation `x:y` means evaluation coordinate `x` carries field value `y`.
This program performs known-position erasure recovery over F17. It does not
use GF256 or correct unknown symbol errors. A surplus consistency failure is
detection only; exactly `k` incorrect samples can interpolate a different
polynomial and are not authenticated by the decoder.

The package composes the reusable List and elaborator libraries through the
exact `testapi-compute-list-elab` host interface. Field arithmetic,
polynomials, symbol encoding, interpolation and validation, input framing,
and reporting live in separate modules.

What this adds to the corpus: a finite-field coding workflow that reconstructs
missing information from a nontrivial surviving subset, with coefficient-list
polynomial multiplication, modular inversion, Lagrange basis accumulation,
and independently evaluated evidence. The erasure and surplus-consistency
paths exercise different data and control flow from numerical row reduction
or standalone arithmetic expressions.
