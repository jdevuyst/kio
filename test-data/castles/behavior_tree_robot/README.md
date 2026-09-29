# Behavior-tree delivery robot

A small robot controller interprets a nested behavior tree once per world
tick. The same interpreter handles timed actions, remembered sequences,
reactive priority selectors, and three decorators. Persistent lists hold
children, per-node progress, the visitation set, and the event journal.

The controller has this shape, with children ordered from left to right:

```text
selector
  hazard guard -> brake (2 ticks)
  once -> sequence
    selector
      sequence
        invert -> blocked condition
        short route (3 ticks)
      detour (2 ticks)
    collect (2 ticks)
    deliver (2 ticks)
```

`input.stdin` contains two whole-line fields per tick: `clear` or `hazard`,
then `open` or `blocked`. End of input between ticks ends the simulation.
An unknown field or incomplete pair produces an input-error report. There is
no seed or implicit world evolution; each pair describes that tick's sensors.

## Tick semantics

An action consumes one unit of work per visit. It returns `Running` until
its duration is reached, then returns `Success` and resets its progress.
The blocked condition returns `Success` for a blocked door and `Failure`
for an open door. Inversion swaps those terminal results and preserves
`Running`.

A sequence remembers the index of its running child. On the next visit it
resumes that child, so earlier successful children are not re-executed.
Child success immediately advances within the same tick; failure ends the
sequence and resets its cursor. Finishing every child also resets the cursor.
A selector starts at its first child on every visit, trying the next child
only after failure. Its first running or successful child wins that tick.

The hazard guard rechecks the current sensor every tick. It fails while clear
and otherwise ticks its child. Thus safety can preempt a running delivery.
After the tree returns, an unfinished action that was not visited is canceled:
its stored work is discarded and reported. Unvisited sequence cursors are
also discarded. This applies both to priority preemption and a guard becoming
false. Completed actions have no pending work to cancel. The `once` decorator
remembers its child's first success permanently; its latch survives absence
from the selected branch.

The fixture resumes the short route, interrupts it after two work units,
briefly clears the hazard, then raises it again. The short route restarts at
one work unit, the clear tick cancels unfinished braking, and the renewed
hazard restarts braking at one work unit. It finishes braking and then closes
the door. The route selector consequently
falls back to the detour. Collection and delivery each span two ticks, and
the last tick verifies that completed delivery is latched. Resetting the
interrupted sequence matters: it must re-evaluate the now-blocked shortcut.

Stdout reports the sensor pair and root status for each tick, followed by
condition, inversion, work, cancellation, and latch events. The final totals
count successful timed actions, canceled unfinished actions, and all work
units including discarded work. This fixture completes four timed actions,
cancels three, and consumes twelve work units over eleven ticks.

## What this adds to the corpus

This is a compositional control-tree interpreter with persistent execution
memory and priority interruption. Nested tree structure determines which
actions execute and resume; input supplies world observations rather than
action commands. It combines non-tail recursive evaluation, list-backed
memory, post-traversal cancellation, terminal-status decorators, and a durable
completion latch through the ordinary compute host protocol.
