# Packet Frame Decoder

This castle is a structured text scanner for a tiny packet-frame protocol.
The frame dataset is compiled into the Kio source; the program does not read
stdin and the castle intentionally omits `input.stdin`.

Each frame has the fixed shape:

```text
PF|<id>|<kind>|<seq>|<payload>|<checksum>
```

The decoder slices the fixed fields, inspects byte codes at key offsets,
checks the declared checksum against the expected checksum for the known
fixture frame, and classifies the result as accepted, checksum-failed, or
magic-failed. The report prints one line per frame plus a deterministic
summary.

What this adds to the corpus: `packet_frame_decoder` is a medium structured
text scanner/parser using the testapi-conformed text tier. It stresses
namespaced host declarations, string length/slice/code-point inspection,
multi-module composition, and deterministic reporting over compiled-in fixture
data.
