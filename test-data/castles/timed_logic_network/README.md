# Timed logic network

This discrete-event simulator follows Boolean transitions through a small
acyclic circuit. A source reaches an XOR gate both directly and through a
delayed buffer. The two paths reconverge, so their different arrival times
create transient pulses even though the settled XOR result is always zero.
Two downstream buffers expose how transport and inertial delays treat those
pulses differently.

| Output | Logic | Delay | Model |
| --- | --- | ---: | --- |
| slow | A | 3 | transport |
| H | A XOR slow | 1 | transport |
| T | H | 2 | transport |
| I | H | 2 | inertial |

All wires and gate ideal values initially equal zero. Delays are positive
integer simulation times. The circuit is fixed; its gate records, pending
events, wire states, and ideal values are persistent lists. A sorted list
priority queue drives the simulation until it empties, without host timers.

## Input and event order

`input.stdin` contains pairs of whole lines: an integer time, then the source
A's value (`0` or `1`). Times must be nondecreasing and range from 1 through
30. At most 16 pairs are accepted. EOF between pairs finishes input; empty
input is also valid. Invalid times, values, missing fields, or excessive
input produce an input-error report. There is no seed.

Events are ordered by time, then source events before gate-output events,
then their unique insertion serial. Source serials follow fixture order.
Affected gates are evaluated in the table's order, so gates scheduling at
the same time receive serials in that order. An assignment equal to the
wire's present value produces no edge and triggers no downstream evaluation.
There is no simultaneous snapshot or timestamp-wide batch update: each edge
takes effect before the next event at that time.

These are this simulator's explicit scheduling conventions. An opposite
input change at an inertial commit's exact due time cancels that commit only
if the input event is processed first under the ordering above. If the commit
has already executed, the opposite input schedules a later edge. In
particular, source events precede pending gate commits, while two gate events
at the same time follow their insertion serials.

## Delay semantics

Each gate remembers its last ideal Boolean result separately from its current
output wire. A changed input re-evaluates the gate; an unchanged ideal result
does nothing. A transport gate schedules every ideal-result change at
`current time + delay`, retaining all pending transitions. This includes
returning to the current output value while an opposite transition is pending.

An inertial gate cancels its pending output event whenever its ideal result
changes. If the new ideal result differs from the current output, it schedules
a replacement after its delay; otherwise it schedules nothing. Thus a pulse
shorter than the delay is suppressed. Cancellation removes pending events from
the queue and reports the canceled value and due time.

The fixture raises A at time 2, lowers it at 3, raises it at 10, and lowers it
at 16. H has one-unit pulses over `[3,4)` and `[6,7)`: T reproduces these after
two units, while I cancels its pending rises at times 4 and 7. The longer H
pulses over `[11,14)` and `[17,20)` pass both buffers. At time 13 the earlier
scheduled slow edge precedes T and I. At time 16 the source edge precedes
the pending T and I edges.

Stdout begins with the network and initial state, then records every actual
wire transition and inertial cancellation. The summary counts edges across
all five wires and canceled pending events. The last processed event settles
the fixture at time 22, after 28 edges and two cancellations; every wire ends
at zero.

## What this adds to the corpus

This castle composes a list priority queue, delayed event propagation through
a reconvergent circuit, separate ideal/output state, and destructive logical
cancellation implemented with persistent-list filtering. Its observable result
is a transient waveform and deterministic tie ordering, rather than a static
truth table or a direct replay of input values.
