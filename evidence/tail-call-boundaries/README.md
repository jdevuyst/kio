# Callback boundary tail-call probes

These reproductions preserve historical callback-integration experiments. They
exercise transfers between independent generated artifacts or through a public
native callback view. The recorded results apply to the producer/runtime
identities below, not automatically to the current
[backend specifications](../../specs/backends/README.md) or implementation.

The Haskell specimens retain their original `CONTRACT` and `BUG` comments,
markers, expected outputs and source bytes. Those comments overstate the
authority for the proposed stack bound; this explanation supersedes that
interpretation. Their `expected.exit` and `expected.stdout` files describe the
proposal's desired result, not the measured outcome. Neither fixture was a
landed regression test for an existing promised stack bound. They reside outside
automatic corpus discovery so their unsettled assertions remain inspectable and
executable without becoming an implicit public contract.

## JavaScript: independent artifact identity

[The value-chain reproduction](reproductions/js/value-chain.mjs) and
[the type-chain reproduction](reproductions/js/type-chain.mjs) retain the two
original 50,000-link blocks and assertions verbatim. Each alternates between
two independently instantiated runtime copies in one JavaScript realm. The
value probe also checks cursor reuse, repeated invocation and the ordinary
callable surface. The type probe checks inert construction, one executing type
stage, its receiver and a separately invoked function result.

The original assertion blocks have SHA-256
`32cd46e1841621d1895c21547a8336a7ee18b1d2d878d5b2fe62fa8c55da5788`
(value) and
`2ff863e9d6bb874e1c59fba37f0f0fefe0bd8988920d477b8218e2fad4129f73`
(type), including their final newline.

[The loader](reproductions/js/runtime.mjs) reads the repository's
[JavaScript runtime](../../kio-rs/src/backends/js/value_runtime.js) for each
invocation. It does not retain a copied historical runtime. In the recorded
runtime, an artifact-local `WeakMap` recognized only its own generated entries;
an unknown foreign callback received an ordinary native call, retaining a frame
when the callback entered its own driver. TypeScript shared that runtime; these
probes did not independently check its static facade.

With Node 24.21.0 and its default stack configuration, both probes exited 1 with
`RangeError: Maximum call stack size exceeded`. The runtime SHA-256 for those
observations is
`611ff6f9db33d9594439bec8ed4d086b4fddb6e91b860fae213a49380603ed9b`.
The failures establish an independent-artifact transfer boundary. They do not
establish failure of a same-artifact chain, and their discovery grants no
authority for global state, a shared executable runtime dependency, or metadata
reads from arbitrary host callbacks.

The current cooperative callable API requires explicit lifting of native
callbacks. These original blocks contain unlifted native leaves and the old
type-stage construction recipe (`poly(guest(...))`), so running them against
the current runtime is not an equivalent stack-overflow test. Use a checkout
whose runtime matches the identity above, then run each independently so the
first failure does not prevent the second observation:

```sh
mise exec -- node evidence/tail-call-boundaries/reproductions/js/value-chain.mjs
mise exec -- node evidence/tail-call-boundaries/reproductions/js/type-chain.mjs
```

## Haskell: public callback conversion

[The scalar/value fixture](reproductions/haskell/general_tail_calls/) constructs
100,001 captured links through exported functions, alternating two package
instances from one artifact. Every link becomes a public native callable before
the host passes it back into the next exported call. Repeated invocation,
different type applications and asymmetric owner-specific effect counts make
both premature execution and dropped work observable.

[The function-result fixture](reproductions/haskell/general_tail_function_result/)
uses the same public round trip with a stage of type
`forall a. IO (IO ())`. It separately checks the type-stage effects and invocation
of the returned Unit-domain action, first at depth 3 and then at depth 100,001.
The public callable-result conversion can retain pending frames even though the
Kio source returns its predecessor's type-stage result directly.

The retained GHC 9.10.3 observations use an explicitly forced `+RTS -K1M` stack
limit:

| Specimen | Optimization | Recorded outcome |
| --- | --- | --- |
| Scalar/value chains | `-O0`, `-O2` | Both repeated chains pass after a prototype identity-result conversion shortcut. |
| Function-result stages | `-O0` | The depth-3 prefix passes; the depth-100,001 stage overflows before returning its callable result. |
| Function-result stages | `-O2` | Not reached by the fail-fast specimen script after the `-O0` failure. |

The scalar/value pass belongs to a prototype compiler with SHA-256
`6f2455f1c9bbf176eb0967e8ac23bcbc43113021a8c5a123d7997329d5f24955`
and Haskell emitter SHA-256
`9d6a9fed2d7b5cd2e7036d1e3829be2ca056d71492155b878bd2a2b32a184c5a`.
It is not acceptance evidence for a different emitter or for removal of strict
value-demand operations. The function-result failure's recorded output begins
`O0: shallow function-result stages`, followed by `Stack space overflow`.
These results concern generated public conversions under `IO`; they make no
operational stack claim for arbitrary host-selected monads.

A failure under the forced 1 MiB limit does not demonstrate exhaustion under
GHC's default RTS settings. No default-limit exhaustion is established by these
records. Changing a native callable representation, dropping required demand,
or moving execution into the returned action changes a separate semantic or
public-interface question; the stack observation does not settle it.

The original hosts also predate explicit Unit domains in first-class Haskell
arrows. Their `IO (IO ())` signatures are historical, not current host examples;
a current-compiler type error is not a reproduction of the recorded overflow.
To run either original specimen with a compatible historical compiler, set
`TMPDIR` to an existing writable temporary root and run from the repository root:

```sh
export KIO_BIN="$PWD/kio-rs/target/debug/kio"
export KIO_TARGET=haskell
(
  cd evidence/tail-call-boundaries/reproductions/haskell/general_tail_calls
  mise exec -- sh run.sh
)
(
  cd evidence/tail-call-boundaries/reproductions/haskell/general_tail_function_result
  mise exec -- sh run.sh
)
```

Each script builds private output, compiles its fixed host at `-O0` and `-O2`,
and invokes the bounded chain with `-K1M`. A failing optimization stops that
script; it must be reported rather than counted as coverage of the remaining
mode. The [specimen hashes](specimens.sha256) bind all sixteen original Haskell
files, including the empty markers and original expectation metadata.
