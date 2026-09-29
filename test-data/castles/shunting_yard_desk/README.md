# shunting_yard_desk

An integer desk calculator built on Dijkstra's shunting-yard algorithm. It
reads pre-tokenized infix expressions from stdin, converts each one to reverse
Polish notation with an operator stack that honours precedence and
associativity, evaluates the RPN on a value stack, and prints the tokens, the
queue, and the value — or the fault, with the token it failed at.

Nothing in it crashes. Every way the pipeline can go wrong is an ordinary sum
arm carrying a position, dispatched with `match!` exactly the way a success is.

## One lexeme per line

`input.stdin` supplies one pre-tokenized **lexeme** per line. A blank line ends
an expression and EOF ends the stream. `desk/lex.classify` maps each lexeme to
an operator, parenthesis, integer, or bad token before the resulting token
stream reaches the shunting-yard pass.

Because the operator spellings are tested first and `string_to_int` is asked
only afterwards, the lexemes `-` and `-3` classify directly as subtraction and
the integer minus three. Expression 6 in the fixture exercises that distinction.

## The operator table

Tightest-binding first:

| Operator | Precedence | Associativity | Meaning |
| --- | --- | --- | --- |
| `^` | 4 | right | exponentiation |
| `~` | 3 | prefix (unary) | negation |
| `*` `/` `%` | 2 | left | product, quotient, remainder |
| `+` `-` | 1 | left | sum, difference |

`(` and `)` group. `^` is not a host function — the host tier has no
exponentiation — so it is computed in Kio by repeated multiplication.

Two rules in the yard carry the whole precedence story, and both are the
places a naive implementation goes wrong:

- An incoming **binary** operator pops every pending operator that outranks it:
  strictly tighter, or *equally* tight and left-associative. A tie against a
  right-associative incoming operator pops nothing, which is the entire reason
  `2 ^ 3 ^ 2` means `2 ^ (3 ^ 2)`.
- An incoming **prefix** operator pops *nothing at all*. It has no left operand,
  so there is nothing for a pending operator to be tighter than; popping one
  here would strand it without its right operand. A yard that treats `~` like a
  binary operator turns `2 ^ ~ 3` into nonsense.

Arity is carried by the type rather than by a side condition: an operator is
either a `Binop` or a `Unop`, so no code has to handle "the unary case of a
binary application", and the yard turns on exactly the distinction the types
already make.

## The faults

Seven, each carrying the 1-based position of the token it failed at:

| Fault | Raised when |
| --- | --- |
| `bad-token` | a line is neither an operator, a parenthesis, nor an integer |
| `unmatched-close` | a `)` closes nothing |
| `unclosed-open` | a `(` is never closed |
| `missing-operand` | an operator finds too few operands on the value stack |
| `extra-operand` | evaluation ends with more than one value left |
| `divide-by-zero` | `/` or `%` is handed a zero divisor |
| `negative-exponent` | `^` is handed a negative exponent |

The last three are guards placed *before* the host call, not after it:
`div_i32` and `mod_i32` are asked for a quotient only once the divisor is known
to be non-zero, and the exponentiation loop runs only once the exponent is
known to be non-negative. The host arithmetic is never handed an argument it
cannot answer.

`extra-operand` names the topmost value left on the stack — for a trailing
operand, that is the trailing token itself. `missing-operand` also covers the
one expression that produces no value at all, `( )`, and quotes its last token.

## The fixture

Fifteen expressions: seven that evaluate, eight that fault. All the arithmetic
is ordinary, so the values check by eye.

| # | Expression | RPN | Result |
| --- | --- | --- | --- |
| 1 | `2 + 3 * 4` | `2 3 4 * +` | `14` |
| 2 | `2 ^ 3 ^ 2` | `2 3 2 ^ ^` | `512` |
| 3 | `~ 2 ^ 2` | `2 2 ^ ~` | `-4` |
| 4 | `( 1 + 2 ) * ( 8 - 3 )` | `1 2 + 8 3 - *` | `15` |
| 5 | `100 / 7 % 5` | `100 7 / 5 %` | `4` |
| 6 | `10 - -3` | `10 -3 -` | `13` |
| 7 | `~ 3 + 4` | `3 ~ 4 +` | `1` |
| 8 | `( 1 + 2` | — | `unclosed-open` at token 1 |
| 9 | `1 + 2 )` | — | `unmatched-close` at token 4 |
| 10 | `5 *` | `5 *` | `missing-operand` at token 2 |
| 11 | `( 1 + 2 ) 5` | `1 2 + 5` | `extra-operand` at token 6 |
| 12 | `8 / 0` | `8 0 /` | `divide-by-zero` at token 2 |
| 13 | `9 % 0` | `9 0 %` | `divide-by-zero` at token 2 |
| 14 | `2 ^ ~ 3` | `2 3 ~ ^` | `negative-exponent` at token 2 |
| 15 | `4 & 5` | — | `bad-token` at token 2 |

Two of the seven values exist to catch the mistakes a naive implementation
makes, and they are the reason to read the table rather than trust the code:

- **Expression 2 depends on `^` associating to the right.** `2 ^ 3 ^ 2` is
  `2 ^ (3 ^ 2)` = `2 ^ 9` = **512**. A yard that pops on a precedence tie
  regardless of associativity builds `(2 ^ 3) ^ 2` and prints `64`.
- **Expression 3 depends on unary minus binding looser than `^`.** `~ 2 ^ 2` is
  `~(2 ^ 2)` = **-4**. An implementation that gives `~` the higher precedence
  builds `(~2) ^ 2` and prints `4`.

Expression 7 is the other side of the same rule: `~` *does* outrank `+`, so it
pops before the `+` goes on the stack and `~ 3 + 4` is `(-3) + 4` = `1`, not
`-(3 + 4)`. Expression 14 exercises the prefix-pops-nothing rule — a `~` that
popped the pending `^` could not produce the queue `2 3 ~ ^` at all.

Expressions 12 and 13 are the same fault arm reached through the two different
guarded host calls, so both guards are exercised rather than only one.

## Output shape

The program owns all of stdout; the runner echoes nothing. Per expression: the
tokens as read back, the RPN queue, and the value or the fault. The `rpn` line
appears exactly when a queue was built — a parenthesis fault or a bad token
stops the conversion, and there is then no queue to echo.

```text
expr 2: 2 ^ 3 ^ 2
  rpn: 2 3 2 ^ ^
  value: 512
expr 12: 8 / 0
  rpn: 8 0 /
  error: divide-by-zero at token 2
expr 15: 4 & 5
  error: bad-token at token 2
```

A closing summary counts the expressions read, the ones that produced a value,
and the failures by kind — walked off the list of fault kinds rather than off
seven hand-maintained counters, so no arm can quietly lose its tally.

## The modules

`desk/num` builds the integer vocabulary the tier lacks (equality from the
antisymmetry of `<=`; exponentiation from repeated multiplication). `desk/list`
is the cons list every stack and queue here is an instance of — the tier has no
arrays. `desk/op` is the operator table, `desk/token` the lexical vocabulary,
`desk/lex` the lexeme-to-token classifier, and `desk/step` the RPN instruction
the yard emits and the evaluator executes — deliberately narrower than a token,
so the evaluator has no impossible arm to answer for. `desk/outcome` holds the
faults, `desk/yard` the conversion, `desk/eval` the RPN machine, `desk/render`
the transcript text, and `testapi/main` the stdin loop and the summary.

What this adds to the corpus: the corpus's first operator-precedence parser —
Dijkstra's shunting yard converting infix to RPN with right-associative
exponentiation and unary minus, an RPN evaluator over a value stack, and every
error arm exercised as a value rather than a crash. Where the existing text
castles scan and reformat, this one *parses*: it stresses a precedence-and-
associativity decision inside a `rec(loop)` stack walk, an arity distinction
carried in the type of the operator sum, four separate loop-driven stack
traversals over a Kio-implemented cons list, guarded partial host arithmetic
(`div_i32`, `mod_i32`) that never sees a zero divisor, and a seven-arm fault
sum every one of whose arms the fixture fires.
