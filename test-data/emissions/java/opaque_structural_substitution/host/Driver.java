import opaque_substitution.OpaqueSubstitutionHost;
import opaque_substitution.Shapes;
import java.util.ArrayList;
import java.util.List;

public final class Driver {
  private record Box<T>(Object value) {}

  private static final class Host implements OpaqueSubstitutionHost<Integer> {
    int unboxCalls;
    final List<List<Integer>> consumed = new ArrayList<>();

    @Override public Integer KioHostBinding_api__Number_fromBody(Integer value) { return value; }
    @Override public Integer KioHostBinding_api__Number_toBody(Integer value) { return value; }

    @Override public <T> Shapes.Apply1<Shapes.KioHostTypeMk_api__Box, T> KioHostBinding_api__Box_fromBody(Object value) {
      return Shapes.KioHostType_api__Box.fromNative(new Box<T>(value));
    }

    @Override public <T> Object KioHostBinding_api__Box_toBody(
        Shapes.Apply1<Shapes.KioHostTypeMk_api__Box, T> value) {
      return ((Box<?>) Shapes.KioHostType_api__Box.toNative(value)).value();
    }

    @Override public <T> T api__unbox(Shapes.Apply1<Shapes.KioHostTypeMk_api__Box, T> boxed) {
      unboxCalls++;
      return (T) ((Box<?>) Shapes.KioHostType_api__Box.toNative(boxed)).value();
    }

    @Override public Integer api__consume(Integer head, Integer tail) {
      consumed.add(List.of(head, tail));
      return head;
    }
  }

  public static void main(String[] args) {
    var host = new Host();
    var pkg = opaque_substitution.OpaqueSubstitution.create(host);
    Shapes.Apply1<Shapes.KioHostTypeMk_api__Box, Shapes.Product<Integer, Integer>> direct =
        Shapes.KioHostType_api__Box.fromNative(new Box<>(new Shapes.Product<Integer, Integer>(173, 409)));
    Shapes.Apply1<Shapes.KioHostTypeMk_api__Box, Shapes.Product<Integer, Integer>> relayed =
        Shapes.KioHostType_api__Box.fromNative(new Box<>(new Shapes.Product<Integer, Integer>(291, 617)));
    if (pkg.api.direct(direct) != 173) throw new AssertionError("direct result");
    if (pkg.api.throughRelay(relayed) != 291) throw new AssertionError("relay result");
    if (host.unboxCalls != 2) throw new AssertionError("unbox count");
    if (!host.consumed.equals(List.of(List.of(173, 409), List.of(291, 617)))) {
      throw new AssertionError("consumed payloads: " + host.consumed);
    }
    System.out.println("opaque structural substitution ok");
  }
}
