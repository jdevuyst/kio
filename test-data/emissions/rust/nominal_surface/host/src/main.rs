use rust_nominal_surface::host::RustNominalSurfaceHost;
use rust_nominal_surface::shapes::nominal::left::Token as LeftToken;
use rust_nominal_surface::shapes::nominal::main::{Boxed, Name, Pair, Value, Wrap};
use rust_nominal_surface::shapes::nominal::right::Token as RightToken;

#[derive(Clone, Debug, PartialEq)]
struct Scalar(i32);

#[derive(Clone)]
struct Host;

impl RustNominalSurfaceHost for Host {
    type main__I32 = i32;
    type main__Scalar = Scalar;
    type main__Str = String;
}

fn exact_nominal_paths(
    _: Wrap<Host>,
    _: Boxed<Host>,
    _: Pair<Host>,
    _: Name<Host>,
    _: Value<Host>,
    _: LeftToken<Host>,
    _: RightToken<Host>,
) {
}

fn main() {
    let package = rust_nominal_surface::create_rustNominalSurface(Host);

    let wrapped = package.main.wrap(42);
    assert_eq!(package.main.unwrap(wrapped), 42);

    let boxed = package.main.boxScalar(Scalar(7));
    assert_eq!(package.main.unboxScalar(boxed), Scalar(7));

    let name = package.main.makeName("Kio".to_owned());
    assert_eq!(package.main.readName(name), "Kio");
    let value = package.main.makeValue(9);
    assert_eq!(package.main.readValue(value), 9);

    let left: LeftToken<Host> = package.left.token();
    let right: RightToken<Host> = package.right.token();

    let wrap = Wrap::<Host>::mkWrap(1);
    let boxed = Boxed::<Host>::boxIt(Scalar(2));
    let pair = package
        .main
        .pairBoxed(boxed, Boxed::<Host>::boxIt(Scalar(3)));
    let name = Name::<Host>::mk("name".to_owned());
    let value = Value::<Host>::mk(4);
    exact_nominal_paths(
        wrap,
        Boxed::<Host>::boxIt(Scalar(5)),
        pair,
        name,
        value,
        left,
        right,
    );
}
