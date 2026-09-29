# markov_ballad_weaver

`markov_ballad_weaver` learns a weighted bigram Markov chain from a small
ballad corpus supplied on stdin, then uses a fixture-seeded pseudo-random
generator to weave deterministic verses. The run prints enough evidence to
audit the result: the training phrases, learned transition counts, generated
lines, and every weighted draw in the first verse.

## Model and generation

The model is built entirely in Kio. Association lists preserve the first-seen
order of words, while each tally records how often a start word, end word, or
successor occurred. A generated line first draws from the weighted start list,
then repeatedly draws from the current word's weighted successor list. It
stops when it reaches a word observed at the end of a training phrase, reaches
the fixture's word cap, or has no learned successor.

Randomness is also Kio-owned. The package uses the overflow-safe recurrence

```text
next = (20077 * state + 12345) mod 32749
```

and threads one state through every line. The runner supplies no random value;
the first line of `input.stdin` is the sole seed.

## Input shape

`input.stdin` is ASCII and line-oriented:

1. a base-10 PRNG seed,
2. the number of verses,
3. the number of lines per verse,
4. the maximum words per generated line,
5. then the training corpus, one word per line, with a blank line between
   phrases and a final `END` sentinel.

The one-word-per-line corpus is deliberate. The `testapi-compute` host surface
can compare and concatenate whole strings but does not provide string slicing,
so the fixture supplies the training corpus as already-tokenized words.

The checked-in fixture uses seed `95`, asks for two three-line verses with an
eight-word cap, and trains on eleven phrases: 54 tokens over 27 distinct
words. It includes repeated transitions, branching transitions, and the cycle
`silver -> moon -> follows -> silver`; the generated run therefore exercises
both natural end-word stops and a word-cap stop.

## Reading stdout

The output is self-contained; the runner does not echo stdin. Its sections are:

- the seed, recurrence, first five states, and requested dimensions;
- the normalized corpus and its token and vocabulary counts;
- the complete model, where `word*count` is a weight;
- the two generated verses;
- the first verse's draw trace; and
- summary counts over all generated lines.

In the trace, a band such as `moon[0,3) song[3,4)` is the half-open interval
owned by each successor. The printed PRNG state is reduced modulo the list's
total weight to produce the roll, so every selected word can be checked from
the fixture and the recurrence without trusting the generated verse alone.

## Package shape

| Module | Responsibility |
| --- | --- |
| `ballad/fixture` | reads the numeric header and blank-delimited phrase corpus |
| `ballad/list` | generic cons lists and loop-driven folds |
| `ballad/model` | weighted starts, ends, and successor rows |
| `ballad/prng` | deterministic overflow-safe recurrence |
| `ballad/weave` | weighted draws, stop reasons, and recorded trace steps |
| `ballad/stats` | generated-word and stop-reason summaries |
| `ballad/report` | corpus, model, verse, trace, and statistics rendering |
| `testapi/main` | fixture-to-report orchestration and host entry point |

## What this adds to the corpus

This is the corpus's first learned stochastic text generator: a fixture-fed
weighted Markov model rather than a fixed transition table, with a Kio-owned
PRNG state threaded across lines and an auditable cumulative-band trace. It
combines stdin parsing, nested generic lists, association-list updates,
label-product rows, label-sum stop reasons, weighted selection, cyclic
`rec(loop)` walks, and a multi-section report in one roughly thousand-line
project. The combination differs from the existing seeded replay castles:
the seed drives choices in a model learned during the same run, and stdout
exposes enough intermediate structure to verify those choices independently.
