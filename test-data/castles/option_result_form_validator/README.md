# Option/result form validator

This castle models validation for a fixed application form. The form has
required email, age, country, and terms fields plus an optional nickname.
Field lookup returns option values from the option POC package, and each
validator returns a result value from the result POC package. The workflow
accumulates successes and errors into a readable report for two checked-in
submissions.

The program reads no stdin. All form data is built inside the package, and
stdout is the complete validation transcript: one accepted submission, one
rejected submission, per-field messages, pass/fail counts, and a boolean
accepted flag.

What this adds to the corpus: this is a dependency-integrated application
workflow rather than a standalone algorithm. It combines path dependencies,
optional field lookup, result-style validation, records/labels, host arithmetic
and boolean formatting, and multi-backend runner execution under the
`testapi-arith-collection` protocol.
