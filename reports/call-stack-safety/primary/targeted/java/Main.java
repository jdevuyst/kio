import bench.Bench;
import bench.BenchHost;
import bench.Shapes;
import java.util.ArrayList;
import java.util.List;
import java.util.Locale;
import java.util.Set;

public final class Main {
  private record Config(String arm, String mode, int depth, int repeat, int seed,
                        int warmup, String purpose) {}

  private static final class Host implements BenchHost<Integer> {
    final boolean witness;
    final List<Integer> trace = new ArrayList<>();
    Host(boolean witness) { this.witness = witness; }

    @Override public Integer KioHostBinding_bench__I32_fromBody(Integer value) { return value; }
    @Override public Integer KioHostBinding_bench__I32_toBody(Integer value) { return value; }
    @Override public Integer bench__step(Integer value, Integer label) {
      if (witness) trace.add(label);
      return value + label + 1;
    }
  }

  private static final class State {
    Bench<Integer> pkg;
    Shapes.Fn1<Integer, Integer> invoke;
    final List<Object> roots = new ArrayList<>();
    long packageNs;
    long constructNs;
  }

  private static void require(boolean ok, String message) {
    if (!ok) throw new IllegalArgumentException(message);
  }

  private static int integer(String text, int low, int high) {
    int value = Integer.parseInt(text);
    require(low <= value && value <= high, "out-of-range integer");
    return value;
  }

  private static Config config(String[] args) {
    require(args.length == 7, "usage: Main ARM MODE DEPTH REPEAT SEED WARMUP PURPOSE");
    require(Set.of("B", "T", "C", "S", "P").contains(args[0]), "unknown arm");
    require(Set.of("direct", "native", "host-loop", "tail", "pending", "callback-tail", "callback-pending").contains(args[1]), "unknown mode");
    require(Set.of("time", "witness").contains(args[6]), "unknown purpose");
    Config c = new Config(args[0], args[1], integer(args[2], 0, 10000),
        integer(args[3], 1, 1000000), integer(args[4], 0, 65535),
        integer(args[5], 0, 1000000), args[6]);
    require(!Set.of("direct", "native").contains(c.mode()) || c.depth() == 0, "direct/native require depth zero");
    require(!c.purpose().equals("witness") || (c.depth() <= 8 && c.repeat() == 1 && c.warmup() == 0), "invalid witness bounds");
    return c;
  }

  private static State construct(Config c, Host host) {
    State state = new State();
    long start = System.nanoTime();
    state.pkg = Bench.create(host);
    state.packageNs = System.nanoTime() - start;
    start = System.nanoTime();
    switch (c.mode()) {
      case "tail", "pending" -> {
        var link = state.pkg.bench.seed();
        state.roots.add(link);
        for (int label = 1; label <= c.depth(); label++) {
          link = c.mode().equals("tail") ? state.pkg.bench.tail(link, label) : state.pkg.bench.pending(link, label);
          state.roots.add(link);
        }
        final var end = link;
        state.invoke = value -> state.pkg.bench.invoke(end, value);
      }
      case "callback-tail", "callback-pending" -> {
        var link = state.pkg.bench.callbackSeed();
        state.roots.add(link);
        for (int label = 1; label <= c.depth(); label++) {
          link = c.mode().equals("callback-tail") ? state.pkg.bench.callbackTail(link, label) : state.pkg.bench.callbackPending(link, label);
          state.roots.add(link);
        }
        // Keep the generated carrier itself; a forwarding lambda is a native boundary.
        state.invoke = link;
      }
      case "native" -> {
        Shapes.Fn1<Integer, Integer> nativeBody = value -> host.bench__step(value, 0);
        state.invoke = value -> state.pkg.bench.callNative(nativeBody, value);
      }
      case "direct" -> state.invoke = state.pkg.bench::direct;
      case "host-loop" -> state.invoke = value -> {
        int result = value;
        for (int label = c.depth(); label >= 0; label--) result = host.bench__step(result, label);
        return result;
      };
      default -> throw new IllegalArgumentException("unknown mode");
    }
    state.constructNs = System.nanoTime() - start;
    require(host.trace.isEmpty(), "construction executed a host body");
    return state;
  }

  public static void main(String[] args) {
    Config c = config(args);
    Host host = new Host(c.purpose().equals("witness"));
    State state = construct(c, host);
    int delta = (c.depth() + 1) * (c.depth() + 2) / 2;
    long expected = 0;
    for (int i = 0; i < c.repeat(); i++) expected += (c.seed() + i * 17) % 65536 + delta;
    long start = System.nanoTime();
    for (int i = 0; i < c.warmup(); i++) {
      int value = (c.seed() + i * 17) % 65536;
      require(state.invoke.apply(value) == value + delta, "warmup checksum mismatch");
    }
    long warmupNs = System.nanoTime() - start;
    start = System.nanoTime();
    long checksum = 0;
    for (int i = 0; i < c.repeat(); i++) checksum += state.invoke.apply((c.seed() + i * 17) % 65536);
    long invokeNs = System.nanoTime() - start;
    require(checksum == expected, "measured checksum mismatch");
    if (host.witness) {
      require(host.trace.size() == c.depth() + 1, "wrong effect count");
      for (int i = 0; i <= c.depth(); i++) {
        int wanted = Set.of("pending", "callback-pending").contains(c.mode()) ? i : c.depth() - i;
        require(host.trace.get(i) == wanted, "wrong effect order");
      }
    }
    start = System.nanoTime();
    state.invoke = null;
    for (int i = state.roots.size() - 1; i >= 0; i--) state.roots.remove(i);
    state.pkg = null;
    long releaseNs = System.nanoTime() - start;
    long totalNs = state.packageNs + state.constructNs + warmupNs + invokeNs + releaseNs;
    System.out.printf(Locale.ROOT,
        "{\"schema\":\"kio-targeted-driver-v1\",\"backend\":\"java\",\"arm\":\"%s\",\"mode\":\"%s\","
        + "\"depth\":%d,\"repeat\":%d,\"seed\":%d,\"warmup\":%d,\"purpose\":\"%s\","
        + "\"checksum\":%d,\"expected\":%d,\"trace\":%s,\"package_ns\":%d,\"construct_ns\":%d,"
        + "\"warmup_ns\":%d,\"invoke_ns\":%d,\"release_roots_ns\":%d,\"total_phases_ns\":%d,"
        + "\"release_limit\":\"roots released in reverse order; no forced GC or live-heap claim\"}%n",
        c.arm(), c.mode(), c.depth(), c.repeat(), c.seed(), c.warmup(), c.purpose(),
        checksum, expected, host.trace, state.packageNs, state.constructNs, warmupNs, invokeNs, releaseNs, totalNs);
  }
}
