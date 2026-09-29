# sensor_alarm_window

`sensor_alarm_window` scans a fixed set of in-source sensor log records. Each
record is a fixed-width ASCII line:

```text
HH:MM|ID|STATE|TMP=nnn|CO=nnn
```

The program parses fields with the runner's text primitives, classifies alert
runs as alarm windows, and prints a compact incident summary. It does not read
stdin; all log records are fixture strings in the Kio source.

Stdout reports the number of records, the fixed line length observed by the
scanner, the two detected alert windows, the alert sample count, and whether
the records matched the expected layout.

What this adds to the corpus: this is a no-stdin structured text
scanner/parser over fixed in-source telemetry, using the `testapi-text`
protocol's string length, slicing, byte-probe, equality, formatting, and print
surface without arrays or case-specific host parsing helpers.
