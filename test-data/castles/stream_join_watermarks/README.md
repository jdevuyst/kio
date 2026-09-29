# Stream join with watermarks

This program joins two keyed event streams by event time. A left event at
time `t` matches every right event with the same key and a time in the closed
interval `[t - 2, t + 3]`. Each input occurrence has an integer ID unique
within its side of a session. IDs distinguish observations: two occurrences
with identical keys and timestamps still produce separate pairs. The join
does not deduplicate observations.

Both watermarks start at zero. A side's watermark is a monotone lower bound
on that side's admissible event times: an arrival with `time < watermark`
is rejected, while equality is accepted. A decreasing watermark is rejected;
repeating a watermark is harmless. Timestamps, IDs, and watermarks in the
fixture are small nonnegative integers, so interval arithmetic fits I32.

The engine keeps two immutable event lists. An accepted arrival probes the
other side and emits one pair for every matching retained occurrence. It
then joins its own side's state. After arrivals and watermark updates, a
left event expires exactly when `right_watermark > left.time + 3`; a right
event expires when `left_watermark > right.time + 2`. Equality retains the
event because a boundary match remains admissible. An arriving event can
produce matches and immediately expire: it is still on time for its own
side, but the opposite watermark rules out any further matches.

Every matching pair of admitted occurrences is emitted when its second
member arrives. The first member cannot have expired before that arrival:
expiry would put the second member below its side's watermark. No later
step revisits an already emitted pair. Thus changing arrival order preserves
the pair multiset whenever the same occurrences remain on time. Watermarks
never retract previously emitted pairs. The report retains a pair audit list
even after the join's event state expires.

## Input and output

`input.stdin` is a line-oriented command stream. Each command and each field
occupies a complete line; keys and session/checkpoint names are strings, and
numeric fields are decimal integers. There is no fixture seed.

| Command | Following lines | Effect |
| --- | --- | --- |
| `begin` | session name | Reset event state, counters, and watermarks. |
| `left`, `right` | occurrence ID, key, time | Admit or reject an event. |
| `watermark-left`, `watermark-right` | watermark | Advance one side and evict expired state. |
| `checkpoint` | checkpoint name | Report both watermarks, retained/evicted counts, and emitted count. |
| `end` | none | Report counters and canonical pairs; compare with the preceding session. |

Sessions are delimited by `begin` and `end`; event IDs must be unique within
each side. Missing or malformed fields and unknown commands print an input
error and stop replay. EOF ends replay. Rejected late events and regressing
watermarks are domain results and do not fail execution.

The fixture replays the same admitted events in two substantially different
arrival orders. It includes both interval boundaries, nonmatching keys, an
event one tick outside a window, duplicate observations, watermark equality,
out-of-order timestamps, rejected watermark regression, repeated watermarks,
late events, and immediate expiry after matching. The final watermark pair
drains both event lists. Checkpoints print left/right counts; pair `L/R`
identifies the left and right occurrence IDs. The emitted and distinct pair
counts establish multiplicity without silently deduplicating the result.
The canonical pair lists and computed arrival-order comparison establish
that both replays yield the same 20 distinct pairs.

## What this adds to the corpus

This castle composes event-time interval joining, independent watermark
frontiers, incremental cross-stream probing, state eviction, and multiset
auditing. Its reusable List dependency carries event and pair state, while
imported elaborators drive command decoding and recursion. The execution
shape is a two-input temporal relation with separate progress frontiers;
its output depends on cross-stream pair multiplicity and exact inclusive
boundaries, beyond a single-stream rolling aggregate or command journal.
