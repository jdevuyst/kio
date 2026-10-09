import grouped.Grouped;
import grouped.GroupedHost;
import grouped.Shapes;
import java.util.ArrayList;
import java.util.List;
import java.util.function.BiFunction;
import java.util.function.Function;

public final class SourceGroups {
  private static final class Host implements GroupedHost<Integer, String, Boolean> {
    final List<String> events = new ArrayList<>();

    @Override public Integer KioHostBinding_api__A_fromBody(Integer value) { return value; }
    @Override public Integer KioHostBinding_api__A_toBody(Integer value) { return value; }
    @Override public String KioHostBinding_api__B_fromBody(String value) { return value; }
    @Override public String KioHostBinding_api__B_toBody(String value) { return value; }
    @Override public Boolean KioHostBinding_api__C_fromBody(Boolean value) { return value; }
    @Override public Boolean KioHostBinding_api__C_toBody(Boolean value) { return value; }

    @Override public String api__importSplit(Integer left, String right) {
      events.add("split:" + left);
      return right + ":" + left;
    }

    @Override public String api__importGrouped(Shapes.Product<Integer, String> pair) {
      events.add("grouped:" + pair.left());
      return pair.right() + ":" + pair.left();
    }

    @Override public String api__importCallback(
        Shapes.Fn1<Shapes.Product<Integer, String>, String> step,
        Shapes.Product<Integer, String> pair) {
      events.add("callback:" + pair.left());
      return step.apply(pair);
    }

    @Override public void api__note(String text) { events.add(text); }
  }

  private static void equal(Object actual, Object expected) {
    if (!expected.equals(actual)) throw new AssertionError(actual + " != " + expected);
  }

  public static void main(String[] args) {
    var host = new Host();
    var pkg = Grouped.create(host);
    equal(host.events, List.of());

    // Equal Kio types have distinct native source-parameter lists.
    BiFunction<Integer, String, String> split = pkg.api::first;
    Function<Shapes.Product<Integer, String>, String> grouped = pkg.api::second;
    equal(split.apply(7, "split"), "split:7");
    equal(grouped.apply(new Shapes.Product<>(11, "grouped")), "grouped:11");
    BiFunction<Integer, Shapes.Product<String, Boolean>, String> mixed = pkg.api::mixed;
    equal(mixed.apply(13, new Shapes.Product<>("mixed", true)), "mixed:13");
    BiFunction<Integer, String, String> curried = pkg.api::curried;
    equal(curried.apply(17, "curried"), "curried:17");

    Function<Integer, Shapes.Fn1<String, String>> returned = pkg.api::returned;
    var later = returned.apply(19);
    equal(host.events, List.of("split:7", "grouped:11", "split:13", "split:17", "returned"));
    equal(later.apply("later"), "later:19");
    equal(later.apply("again"), "again:19");

    int[] callbacks = {0};
    Shapes.Fn1<Shapes.Product<Integer, String>, String> callback = pair -> {
      callbacks[0]++;
      return pair.right() + ":" + pair.left();
    };
    equal(pkg.api.callback(callback, new Shapes.Product<>(23, "callback")), "callback:23");
    var roundtrip = pkg.api.echoCallback(callback);
    equal(callbacks[0], 1);
    equal(roundtrip.apply(new Shapes.Product<>(29, "roundtrip")), "roundtrip:29");
    equal(callbacks[0], 2);
    equal(host.events, List.of("split:7", "grouped:11", "split:13", "split:17", "returned",
        "split:19", "split:19", "callback:23"));

    Runnable directUnit = pkg.api::directUnit;
    directUnit.run();
    var unit = new Shapes.Unit();
    Function<Shapes.Unit, Shapes.Unit> genericUnit = pkg.api::generic;
    equal(genericUnit.apply(unit), unit);
    var pair = new Shapes.Product<>(31, "generic");
    equal(pkg.api.generic(pair), pair);
    int[] genericCalls = {0};
    equal(pkg.api.genericCallback(value -> { genericCalls[0]++; return value; }, unit), unit);
    equal(pkg.api.genericCallback(value -> { genericCalls[0]++; return value; }, pair), pair);
    equal(genericCalls[0], 2);
    System.out.println("source grouping ok");
  }
}
