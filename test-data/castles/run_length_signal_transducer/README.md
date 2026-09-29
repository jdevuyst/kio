# Run-length signal transducer

This streaming codec reads one signed 32-bit integer per line until EOF.
It accumulates equal samples into packets of at most four samples. A fifth
equal sample begins another packet; a changed sample closes the current
packet. EOF flushes the pending packet, including a final singleton.

The fixture contains negative samples, zero, a run exactly at the cap, and
nine consecutive sevens that become packets of lengths four, four and one.
It has 18 samples, four value transitions and two cap-induced splits. A
non-integer input line produces a readable input error.

Stdout lists each packet with its value, length and boundary reason, then
compares each decoded sample with the original input at the same position.
The final roundtrip result requires equal values and equal list lengths.
Encoding consumes input incrementally; the retained input list is used only
as the independent roundtrip reference. Decoding uses the shared List
library's replication, append and fold operations.

What this adds to the corpus: a capped streaming run-length state machine
composed with a separate list decoder and a sample-by-sample verifier.
Cap boundaries and value transitions have different meanings even when
adjacent encoded packets carry the same sample.
