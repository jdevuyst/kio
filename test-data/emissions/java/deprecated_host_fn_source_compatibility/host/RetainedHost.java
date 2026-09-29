import app.App;
import app.AppHost;
import app.Shapes;

public final class RetainedHost {
  private static final class LiveHost implements AppHost<String> {
    @Override
    public String KioHostBinding_api__Str_fromBody(String value) {
      return value;
    }

    @Override
    public String KioHostBinding_api__Str_toBody(String value) {
      return value;
    }

    @Override
    public String api__open() {
      return "live";
    }
  }

  @SuppressWarnings("deprecation")
  private static final class OldHost implements AppHost<String> {
    @Override
    public String KioHostBinding_api__Str_fromBody(String value) {
      return value;
    }

    @Override
    public String KioHostBinding_api__Str_toBody(String value) {
      return value;
    }

    @Override
    public String api__open() {
      return "old host";
    }

    @Override
    public void api__log(String value) {
      throw new AssertionError("removed log method dispatched");
    }

    @Override
    public Shapes.Sum<Shapes.Sum<String, String>, String> api__archived(
        String head, Shapes.Sum<String, String> value) {
      throw new AssertionError("removed archived method dispatched");
    }
  }

  @SuppressWarnings("deprecation")
  private static Shapes.Sum<Shapes.Sum<String, String>, String> retainedSourceMustCompile(AppHost<String> host) {
    host.api__log("old");
    return host.api__archived("head", new Shapes.Sum__0<String, String>("left"));
  }

  private static void assertDeprecated(String methodName) {
    for (var method : AppHost.class.getMethods()) {
      if (method.getName().equals(methodName)
          && method.isAnnotationPresent(Deprecated.class)) {
        return;
      }
    }
    throw new AssertionError(methodName + " is not marked @Deprecated");
  }

  private static void assertLiveExport(AppHost<String> host, String expected) {
    App<String> pkg = App.create(host);
    if (!expected.equals(pkg.api.echo(expected))) {
      throw new AssertionError("live export returned the wrong value");
    }
  }

  public static void main(String[] args) {
    assertDeprecated("api__log");
    assertDeprecated("api__archived");
    assertLiveExport(new LiveHost(), "live");
    assertLiveExport(new OldHost(), "old host");
  }
}
