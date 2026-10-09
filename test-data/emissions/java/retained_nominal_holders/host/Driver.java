import retention.Retention;
import retention.Shapes;

@SuppressWarnings("deprecation")
public final class Driver {
  private static void retained(Class<?> type) {
    if (!type.isAnnotationPresent(Deprecated.class)) throw new AssertionError(type + " is not deprecated");
  }

  private static void live(Class<?> type) {
    if (type.isAnnotationPresent(Deprecated.class)) throw new AssertionError(type + " inherited deprecation");
  }

  public static void main(String[] args) {
    Retention.create().api.unit();
    retained(Shapes.KioModule_retired.class);
    retained(Shapes.KioModule_retired.KioModule_deep.class);
    retained(Shapes.KioModule_retired.KioModule_deep.Gone.class);
    retained(Shapes.KioModule_shared.KioModule_removed.class);
    retained(Shapes.KioModule_shared.KioModule_removed.Forgotten.class);
    live(Shapes.KioModule_shared.class);
    live(Shapes.KioModule_shared.KioModule_current.class);
    live(Shapes.KioModule_shared.KioModule_current.Token.class);
    if (!Shapes.KioModule_retired.KioModule_deep.Gone.class.isInterface()) throw new AssertionError("retained nullary carrier");
    new Shapes.KioModule_shared.KioModule_current.Token(new Shapes.Unit()).value();
    System.out.println("retained nominal holder provenance ok");
  }
}
