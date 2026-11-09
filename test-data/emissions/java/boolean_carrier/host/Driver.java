import boolean_carrier.BooleanCarrier;
import boolean_carrier.BooleanCarrierHost;
import boolean_carrier.Shapes;
import java.util.ArrayList;
import java.util.List;

public final class Driver {
  private static final class Flag {
    final boolean truth;
    final String tag;

    Flag(boolean truth, String tag) {
      this.truth = truth;
      this.tag = tag;
    }
  }

  private static final class Host implements BooleanCarrierHost<Flag> {
    final List<String> events = new ArrayList<>();
    Flag left = new Flag(false, "initial-left");
    Flag right = new Flag(false, "initial-right");

    @Override public Flag KioHostBinding_api__Flag_fromBody(Boolean value) {
      return new Flag(value, "literal");
    }

    @Override public Boolean KioHostBinding_api__Flag_toBody(Flag value) {
      return value.truth;
    }

    @Override public Flag api__left() {
      events.add("L");
      return left;
    }

    Flag callback(Shapes.Unit unit) {
      events.add("R");
      return right;
    }

    @Override public Shapes.Fn1<Shapes.Unit, Flag> api__factory() {
      events.add("F");
      return this::callback;
    }
  }

  private static void selected(Flag actual, Flag expected) {
    if (actual != expected || !actual.tag.equals(expected.tag)) {
      throw new AssertionError("the selected Boolean carrier was replaced");
    }
  }

  private static void events(Host host, List<String> expected) {
    if (!host.events.equals(expected)) {
      throw new AssertionError(host.events + " != " + expected);
    }
  }

  public static void main(String[] args) {
    var host = new Host();
    var pkg = BooleanCarrier.create(host);
    for (boolean leftTruth : new boolean[] {false, true}) {
      for (boolean rightTruth : new boolean[] {false, true}) {
        host.left = new Flag(leftTruth, "left");
        host.right = new Flag(rightTruth, "right");
        for (boolean conjunction : new boolean[] {false, true}) {
          boolean taken = conjunction ? leftTruth : !leftTruth;
          Flag expected = taken ? host.right : host.left;
          selected(conjunction ? pkg.api.andInline(host.left, host.right)
              : pkg.api.orInline(host.left, host.right), expected);

          host.events.clear();
          selected(conjunction ? pkg.api.andCallback(host.left, host::callback)
              : pkg.api.orCallback(host.left, host::callback), expected);
          events(host, taken ? List.of("R") : List.of());

          host.events.clear();
          selected(conjunction ? pkg.api.andFactory() : pkg.api.orFactory(), expected);
          events(host, taken ? List.of("L", "F", "R") : List.of("L", "F"));
        }
      }
    }
  }
}
