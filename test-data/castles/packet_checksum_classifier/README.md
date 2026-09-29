# Packet Checksum Classifier

This castle classifies four fixed-width packet frames embedded in
`workdir/packet_checksum_classifier/source.kio`. It does not read stdin and has
no package dependencies.

The scanner treats every frame as `PREFIX:ID:CHECKSUM`, then the rules module
classifies each one as valid, warn, reject, or invalid-prefix. Stdout prints the
fixture size, the per-frame classification, and a summary count.

What this adds to the corpus: a compact packet-parser-shaped text scanner
using the `testapi-text` protocol. It exercises fixed-span slicing, string
equality, module composition, and readable reporting without relying on stdin
or a host loop.
