import names.Shapes;
public final class HiddenConstructor {
  Shapes.KioModule_api.Opaque<String> wrong(String value) { return new Shapes.KioModule_api.Opaque<>(value); }
}
