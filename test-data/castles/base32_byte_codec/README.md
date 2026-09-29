# base32_byte_codec

A base32 encoder and companion decoder built out of nothing but integer
arithmetic. The program encodes five in-source byte payloads to canonical
RFC 4648 base32, decodes the glyphs straight back, checks the recovered
bytes against the ones it started with, accepts a sixth canonical received
transmission, and refuses a seventh transmission that arrived damaged.

Payloads are lists of byte *values* — integers in `[0, 256)` — because
the host env this castle declares has no way to look inside a string.
That constraint is the point: bytes are numbers here, and the whole codec
is arithmetic.

## The encoding scheme

A payload is a stream of bits, not a stream of bytes: each byte pushes
eight fresh bits into an accumulator, and every time five or more bits are
pending the top five are drawn off as one glyph, spelled through the
32-entry alphabet `A`-`Z` (values 0-25) and `2`-`7` (values 26-31). Eight
and five share no factor, so group boundaries drift through the payload
until the bits run out; whatever is left over becomes the high bits of one
last glyph, and `=` pads the transmission up to a whole eight-glyph
quantum.

There is no shift and no mask at this host boundary, so `base32/bits`
builds both out of arithmetic: a left shift is a multiply by a power of
two, a right shift is a divide, and a mask is a remainder. The alphabet
(`base32/alphabet`) is an association list walked with the host `loop`,
and both directions of the mapping read from that one list, so they use
the same glyph-to-value correspondence.

## The packet fixtures

Seven fixtures, all in source — there is no `input.stdin`.

| # | Fixture | Why |
| --- | --- | --- |
| 1 | payload, 5 bytes (`Kio!` and a newline) | Five bytes is exactly one 40-bit quantum, so it encodes with **no padding**. |
| 2 | payload, 7 bytes (`0`, `255`, `128`, and single set bits walking down) | Edges of the byte range; seven bytes leave **four** glyphs of the quantum unfilled. |
| 3 | payload, 11 bytes (`hello world`) | Eleven bytes overrun two quanta by one byte — the **widest padding** the scheme produces, six glyphs. |
| 4 | payload, 3 bytes (`foo`) | Three bytes produce five data glyphs and **three** padding glyphs. |
| 5 | payload, 4 bytes (`foob`) | Four bytes produce seven data glyphs and **one** padding glyph. |
| 6 | received transmission, 8 glyphs (`MZXW6===`) | Canonical base32 for `foo`, received without the original payload. |
| 7 | received transmission, 8 glyphs (`JNUW61IK`) | Packet 1's own transmission with one transcription slip. |

Packet 7 is the corruption case. Its sixth glyph should be the letter `I`
but arrived as the digit `1` — and RFC 4648 leaves `1` out of the
alphabet precisely because the two look alike. The decoder cannot turn a
glyph it does not know into bits, so it refuses the whole transmission and
names the position it stopped at rather than quietly recovering the wrong
bytes.

The five lengths are deliberately chosen so that every padding width the
encoder can emit — zero, one, three, four, and six glyphs — is exercised.

The decoder is the encoder's companion, not a general RFC 4648 validator.
For round trips it consumes the encoder's canonical output; for the damaged
transmission it rejects the first glyph outside the alphabet. It does not
validate `=` placement or count, nor the unused low bits of a final data
glyph.

## Output shape

One block per packet, then a tally. The program owns every line; nothing
is echoed.

```text
packet <n>: payload, <k> bytes
  bytes: <the payload, in decimal>
  encoded: <the base32 transmission, padding included>
  verdict: OK, round trip verified, <k> bytes recovered

packet <n>: received transmission, <k> glyphs
  received: <the glyphs as they arrived>
  verdict: OK, received transmission decoded, <k> bytes recovered

or:

packet <n>: received transmission, <k> glyphs
  received: <the glyphs as they arrived>
  verdict: REJECT, glyph <p> is outside the alphabet

summary: packets ok <a>, packets rejected <b>, bytes recovered <c>
```

A payload's `verdict` is `OK` only when the bytes that came back out of
the decoder are the same bytes, in the same order, that went into the
encoder; the round trip is checked, not assumed. `bytes recovered` counts
the bytes the decoder handed back across every accepted packet.

What this adds to the corpus: the corpus's first binary-encoding codec —
bit packing via integer arithmetic, alphabet table lookup, round-trip
verification, and corruption rejection. It is the first castle to pack
bits with arithmetic alone (shifts as multiply and divide, masks as
remainder, an accumulator carrying a bit count across a fold whose group
boundaries never line up with its input boundaries), the first to look a
table up in both directions over one association list walked with the
host `loop`, and the first to pair an encoder with its decoder and then
*check* the round trip rather than trust it. Corruption rejection is the
fourth piece: a decode result modelled as a label sum of a recovered
payload or a refusal position and dispatched with `match!`, so a glyph
outside the alphabet is a value the program reports rather than an error
it has no way to express.
