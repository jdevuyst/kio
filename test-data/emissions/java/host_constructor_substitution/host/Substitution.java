import substitution.SubstitutionHost;
import substitution.Shapes;
import java.util.ArrayList;
import java.util.List;

public final class Substitution {
  private record Box<T>(Object value) {}
  private record ProductStorage(Shapes.Product<Integer, Shapes.Product<Integer, Integer>> value) {}
  private record SumStorage(Shapes.Sum<Integer, Shapes.Sum<Integer, Integer>> value) {}

  private static final class Host implements SubstitutionHost<Integer> {
    final List<String> events = new ArrayList<>();

    @Override public Integer KioHostBinding_api__I32_fromBody(Integer value) { return value; }
    @Override public Integer KioHostBinding_api__I32_toBody(Integer value) { return value; }

    @Override
    public <T> Shapes.Apply1<Shapes.KioHostTypeMk_api__Box, T> KioHostBinding_api__Box_fromBody(Object value) {
      return Shapes.KioHostType_api__Box.fromNative(new Box<T>(value));
    }

    @Override public <T> Object KioHostBinding_api__Box_toBody(
        Shapes.Apply1<Shapes.KioHostTypeMk_api__Box, T> value) {
      if (Shapes.KioHostType_api__Box.toNative(value) instanceof Box<?> box) return box.value();
      throw new AssertionError("foreign host storage");
    }

    @Override public <T> Shapes.Apply1<Shapes.KioHostTypeMk_api__Box, T> api__echo(
        Shapes.Apply1<Shapes.KioHostTypeMk_api__Box, T> value) {
      events.add("echo");
      return value;
    }
  }

  private static Object payload(Shapes.Apply1<Shapes.KioHostTypeMk_api__Box, ?> value) {
    if (Shapes.KioHostType_api__Box.toNative(value) instanceof Box<?> box) return box.value();
    throw new AssertionError("foreign host storage");
  }

  private static void equal(Object actual, Object expected) {
    if (!expected.equals(actual)) throw new AssertionError(actual + " != " + expected);
  }

  public static void main(String[] args) {
    var host = new Host();
    var pkg = substitution.Substitution.create(host);
    var triple = pkg.api.productValue();
    var nested = new Shapes.Product<>(11, new Shapes.Product<>(22, 33));
    Shapes.Apply1<Shapes.KioHostTypeMk_api__Box, Shapes.Product<Integer, Shapes.Product<Integer, Integer>>> generic =
        pkg.api.<Integer, Shapes.Product<Integer, Integer>>product(
            Shapes.KioHostType_api__Box.fromNative(new Box<>(nested)));
    Shapes.Apply1<Shapes.KioHostTypeMk_api__Box, Shapes.Product<Integer, Shapes.Product<Integer, Integer>>> result =
        pkg.api.concreteProduct(pkg.api.whole(Shapes.KioHostType_api__Box.fromNative(new Box<>(triple))));
    equal(payload(result), new Shapes.Product<>(11, new Shapes.Product<>(22, 33)));

    var restored = pkg.api.abstractKeep(result);
    equal(payload(restored), triple);

    int[] calls = {0};
    var callbackResult = pkg.api.callback(value -> { calls[0]++; host.events.add("callback"); return value; }, generic);
    var later = pkg.api.returned(callbackResult);
    equal(calls[0], 1);
    equal(payload(later.apply(new Shapes.Unit())), nested);
    equal(payload(later.apply(new Shapes.Unit())), nested);
    equal(host.events, List.of("echo", "echo", "echo", "callback"));

    List<String> transport = new ArrayList<>();
    Shapes.Fn1<
        Shapes.Product<Integer, Shapes.Product<Integer, Integer>>,
        Shapes.Apply1<Shapes.KioHostTypeMk_api__Box, Shapes.Product<Integer, Shapes.Product<Integer, Integer>>>> packProduct =
        value -> {
          equal(List.of(value.left(), value.right().left(), value.right().right()), List.of(11, 22, 33));
          transport.add("pack-product");
          return Shapes.KioHostType_api__Box.fromNative(new Box<>(new ProductStorage(value)));
        };
    Shapes.Fn1<
        Shapes.Apply1<Shapes.KioHostTypeMk_api__Box, Shapes.Product<Integer, Shapes.Product<Integer, Integer>>>,
        Shapes.Product<Integer, Shapes.Product<Integer, Integer>>> readProduct =
        value -> {
          if (!(payload(value) instanceof ProductStorage stored)) throw new AssertionError("product storage");
          equal(List.of(stored.value().left(), stored.value().right().left(), stored.value().right().right()),
              List.of(11, 22, 33));
          transport.add("read-product");
          return stored.value();
        };
    var productResult = pkg.api.concreteProductRoundtrip(packProduct, readProduct, triple);
    equal(List.of(productResult.left(), productResult.right().left(), productResult.right().right()),
        List.of(11, 22, 33));
    equal(transport, List.of("pack-product", "read-product"));

    List<Shapes.Sum<Integer, Shapes.Sum<Integer, Integer>>> sums = List.of(
        new Shapes.Left<Integer, Shapes.Sum<Integer, Integer>>(11),
        new Shapes.Right<Integer, Shapes.Sum<Integer, Integer>>(new Shapes.Left<Integer, Integer>(22)),
        new Shapes.Right<Integer, Shapes.Sum<Integer, Integer>>(new Shapes.Right<Integer, Integer>(33)));
    for (int index = 0; index < sums.size(); index++) {
      var expected = sums.get(index);
      var arm = index;
      Shapes.Fn1<
          Shapes.Sum<Integer, Shapes.Sum<Integer, Integer>>,
          Shapes.Apply1<Shapes.KioHostTypeMk_api__Box, Shapes.Sum<Integer, Shapes.Sum<Integer, Integer>>>> packSum =
          value -> {
            equal(value, expected);
            transport.add("pack-sum-" + arm);
            return Shapes.KioHostType_api__Box.fromNative(new Box<>(new SumStorage(value)));
          };
      Shapes.Fn1<
          Shapes.Apply1<Shapes.KioHostTypeMk_api__Box, Shapes.Sum<Integer, Shapes.Sum<Integer, Integer>>>,
          Shapes.Sum<Integer, Shapes.Sum<Integer, Integer>>> readSum =
          value -> {
            if (!(payload(value) instanceof SumStorage stored)) throw new AssertionError("sum storage");
            equal(stored.value(), expected);
            transport.add("read-sum-" + arm);
            return stored.value();
          };
      equal(pkg.api.concreteSumRoundtrip(packSum, readSum, expected), expected);
    }
    equal(transport, List.of("pack-product", "read-product",
        "pack-sum-0", "read-sum-0", "pack-sum-1", "read-sum-1", "pack-sum-2", "read-sum-2"));
    System.out.println("host constructor substitution ok");
  }
}
