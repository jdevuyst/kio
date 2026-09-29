# expression_formatter

A medium structured text scanner, parser, and formatter for a tiny
arithmetic/config expression. The program owns one fixed source string,
classifies its lexemes into a token stream, checks the expression shape, and
prints a normalized report.

## What it models

The source fixture is embedded in the scanner:

```text
total=base+(tax*3)
```

The scanner emits nine tokens with kind, lexeme, span, and nesting depth. The
parser classifies the token stream as an assignment whose right-hand side is an
addition with a parenthesized multiplication, then the renderer prints a
normalized expression and a token table.

## Input shape

There is no `input.stdin`. The fixture is the source literal in the scanner.

## Output shape

Stdout is a self-contained report:

- the raw source expression,
- the normalized expression,
- the parsed shape and validation status,
- aggregate token/depth/binding counts,
- and one formatted line per token.

The runner does not echo anything; every byte in `expected.stdout` comes from
the Kio program.

## What this adds to the corpus

This castle extends the structured text scanner family beyond the compact
`gate_text_scanner` shape. It still uses a fixed ASCII fixture and the
`testapi-text` runner protocol, but it adds a richer token record, explicit
parser state, precedence/nesting classification, and a separate formatting
pipeline that renders a normalized expression plus a token table. The compiler
stress comes from composed label records, cross-module imports, string
slicing, string-length classification, and a testapi-conformed full text host
surface.
