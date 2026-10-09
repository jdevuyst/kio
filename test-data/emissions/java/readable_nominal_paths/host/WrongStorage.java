import names.Shapes;
public final class WrongStorage {
  Object wrong(Shapes.Apply1<Shapes.KioModule_api.Box, String> value) {
    return Shapes.KioModule_api.Storage.KioStorage.toNative(value);
  }
}
