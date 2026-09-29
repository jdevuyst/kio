# float_weather_index

`float_weather_index` computes a small weather comfort index from a fixed
four-reading day. Each reading combines temperature, humidity, wind, and a
site correction term into a raw F64 score; a weighted smoothing pass then
runs across the day.

This is a no-stdin castle. The program prints the raw reading scores, the
final smoothed index, and two small derived summaries. The output is intended
to be read as a deterministic numeric report; there is no fixture input or
runtime seed.

What this adds to the corpus: this is a no-stdin floating-point simulation
using the `testapi-float` protocol. It stresses F64 role values, namespaced
host arithmetic and formatting, multi-module composition, and backend emission
for add/sub/mul-only numeric code without depending on a POC package.
