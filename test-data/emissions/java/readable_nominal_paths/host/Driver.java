import names.Names;
import names.NamesHost;
import names.Shapes;
import names.Shapes.KioModule_api.Token;

@SuppressWarnings("deprecation")
public final class Driver {
  static java.lang.Void bottom(Shapes.KioModule_api.Bottom<String> value) { return value.value(); }
  static Shapes.KioModule_api.Opaque<String> opaqueIdentity(Shapes.KioModule_api.Opaque<String> value) { return value; }

  private static final class Host implements NamesHost<String> {
    int relays;
    public String KioHostBinding_api__Text_fromBody(String value) { return value; }
    public String KioHostBinding_api__Text_toBody(String value) { return value; }
    public <T> Shapes.Apply1<Shapes.KioModule_api.Box, T> KioHostBinding_api__Box_fromBody(Object value) {
      return Shapes.KioModule_api.Box.Storage.fromNative(value);
    }
    public <T> Object KioHostBinding_api__Box_toBody(Shapes.Apply1<Shapes.KioModule_api.Box, T> value) {
      return Shapes.KioModule_api.Box.Storage.toNative(value);
    }
    public <T> Shapes.Apply1<Shapes.KioModule_api.Storage, T> KioHostBinding_api__Storage_fromBody(Object value) {
      return Shapes.KioModule_api.Storage.KioStorage.fromNative(value);
    }
    public <T> Object KioHostBinding_api__Storage_toBody(Shapes.Apply1<Shapes.KioModule_api.Storage, T> value) {
      return Shapes.KioModule_api.Storage.KioStorage.toNative(value);
    }
    public <T> Shapes.Apply1<Shapes.KioNominal_Host_Hc0c1f3f1c3b4c370, T> KioHostBinding_a_sa_sa_sa_sa_sa_sa_sa_sa_sa_sa_sa_sa_sa_sa_sa_sa_sa_sa_sa__Box_fromBody(Object value) {
      return Shapes.KioNominal_Host_Hc0c1f3f1c3b4c370.Storage.fromNative(value);
    }
    public <T> Object KioHostBinding_a_sa_sa_sa_sa_sa_sa_sa_sa_sa_sa_sa_sa_sa_sa_sa_sa_sa_sa_sa__Box_toBody(Shapes.Apply1<Shapes.KioNominal_Host_Hc0c1f3f1c3b4c370, T> value) {
      return Shapes.KioNominal_Host_Hc0c1f3f1c3b4c370.Storage.toNative(value);
    }
    public Token<String> api__relay(Token<String> value) { relays++; return value; }
  }

  public static void main(String[] args) {
    var host = new Host();
    var pkg = Names.create(host);
    Token<String> token = new Token<>("readable");
    if (pkg.api.round(token) != token || !token.value().equals("readable") || host.relays != 1) {
      throw new AssertionError("ordinary host and Kio roundtrip");
    }
    var nested = new Shapes.KioModule_api.KioModule_types.Token<String>(new Shapes.Unit());
    if (pkg.api.nestedRound(nested) != nested) throw new AssertionError("same leaf identity");
    new Shapes.KioModule_api.Types<String>(new Shapes.Unit()).value();
    Shapes.KioModule_api.Foo_bar<String> words = pkg.api.KioType_FooBar.makeFooBar();
    Shapes.KioModule_api.Foobar<String> word = pkg.api.KioType_Foobar.makeFoobar();
    words.value();
    word.value();
    Shapes.KioModule_api.KioModule_foo_ubar.Token<String> moduleWords =
        pkg.api.KioModule_fooBar.KioType_Token.makeToken();
    Shapes.KioModule_api.KioModule_foobar.Token<String> moduleWord =
        pkg.api.KioModule_foobar.KioType_Token.makeToken();
    moduleWords.value();
    moduleWord.value();
    Shapes.KioModule_api.KioType_Kiomodule_utypes<String> moduleRole =
        pkg.api.KioType_KiomoduleTypes.makeModule();
    Shapes.KioModule_api.KioType_Kiotype_ushapes<String> typeRole =
        pkg.api.KioType_KiotypeShapes.makeType();
    Shapes.KioModule_api.KioType_Kionominal_uhost<String> nominalRole =
        pkg.api.KioType_KionominalHost.makeNominal();
    moduleRole.value();
    typeRole.value();
    nominalRole.value();
    var chain = new Shapes.KioModule_api.Chain<String>(new Shapes.Left<Shapes.Unit, Shapes.KioModule_api.Chain<String>>(new Shapes.Unit()));
    if (pkg.api.chainRound(chain) != chain || !(chain.value() instanceof Shapes.Left<?, ?>)) {
      throw new AssertionError("recursive carrier identity");
    }
    var repeated = new Shapes.KioModule_api.KioModule_D1_api.Token<String>(new Shapes.Unit());
    if (pkg.api.repeatRound(repeated) != repeated) throw new AssertionError("ancestor escape");
    var reserved = new Shapes.KioModule_java.KioModule_class.Token<String>(new Shapes.Unit());
    if (pkg.api.reservedRound(reserved) != reserved) throw new AssertionError("reserved modules");
    pkg.KioModule_java.KioModule_class.java();
    pkg.KioModule_java.KioModule_class.KioType_Token.makeToken().value();
    var deep = new Shapes.KioNominal_Newtype_Hf02cf3a0e284f12e<String>(new Shapes.Unit());
    if (pkg.api.deepRound(deep) != deep) throw new AssertionError("bounded deep nominal");
    Shapes.Apply1<Shapes.Apply1<Shapes.KioModule_api.Pick_first<String>, String>, Shapes.Unit> picked =
        pkg.api.KioType_PickFirst.<String, Shapes.Unit>makePick("picked");
    if (!pkg.api.KioType_PickFirst.readPick(picked).equals("picked")) throw new AssertionError("generic indices");
    Object storage = new Object();
    Shapes.Apply1<Shapes.KioModule_api.Box, Token<String>> boxed = Shapes.KioModule_api.Box.Storage.fromNative(storage);
    if (Shapes.KioModule_api.Box.Storage.toNative(pkg.api.boxRound(boxed)) != storage) throw new AssertionError("host storage");
    Shapes.Apply1<Shapes.KioModule_api.Storage, String> stored = Shapes.KioModule_api.Storage.KioStorage.fromNative(storage);
    if (Shapes.KioModule_api.Storage.KioStorage.toNative(pkg.api.storageRound(stored)) != storage) throw new AssertionError("helper ancestor");
    Shapes.Apply1<Shapes.KioNominal_Host_Hc0c1f3f1c3b4c370, String> deepBox = Shapes.KioNominal_Host_Hc0c1f3f1c3b4c370.Storage.fromNative(storage);
    if (Shapes.KioNominal_Host_Hc0c1f3f1c3b4c370.Storage.toNative(pkg.api.deepBoxRound(deepBox)) != storage) throw new AssertionError("bounded deep host");
    new Shapes.KioModule_api.Object<String>(new Shapes.Unit()).value();
    new Shapes.KioModule_api.String<String>(new Shapes.Unit()).value();
    new Shapes.KioModule_api.Void<String>(new Shapes.Unit()).value();
    new Shapes.KioModule_api.Deprecated<String>(new Shapes.Unit()).value();
    new Shapes.KioModule_api.Suppress_warnings<String>(new Shapes.Unit()).value();
    new Shapes.KioModule_api.KioType_Shapes<String>(new Shapes.Unit()).value();
    new Shapes.KioModule_api.Product<String>(new Shapes.Unit()).value();
    new Shapes.KioModule_api.Apply1<String>(new Shapes.Unit()).value();
    Shapes.KioModule_api.Gone gone = null;
    try { host.api__archived(gone); throw new AssertionError("retained default did not trap"); }
    catch (IllegalStateException expected) { }
    System.out.println("readable nominal paths ok");
  }
}
