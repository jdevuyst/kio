use rust_structural_surface::host::RustStructuralSurfaceHost;
use rust_structural_surface::ffi;
use rust_structural_surface::shapes::{Product, Sum};

#[derive(Clone)]
struct Host;

impl RustStructuralSurfaceHost for Host {
    type main__Bool = bool;
    type main__I32 = i32;
    type main__Str = String;

    fn main__pair(
        &self,
        number: Self::main__I32,
        text: Self::main__Str,
    ) -> ffi::env::main__pair::ret<Self> {
        Product {
            _0: number,
            _1: text,
        }
    }

    fn main__either(&self) -> ffi::env::main__either::ret<Self> {
        ffi::env::main__either::ret::<Self>::Left(42)
    }

    fn main__flat3(
        &self,
        number: Self::main__I32,
        text: Self::main__Str,
        flag: Self::main__Bool,
    ) -> ffi::env::main__flat3::ret<Self> {
        Product {
            _0: number,
            _1: Product {
                _0: text,
                _1: flag,
            },
        }
    }

    fn main__either3(&self) -> ffi::env::main__either3::ret<Self> {
        ffi::env::main__either3::ret::<Self>::Right(Sum::Right(true))
    }
}

fn main() {
    let package = rust_structural_surface::create_rustStructuralSurface(Host);

    let pair = package.main.makePair(7, "seven".to_owned());
    assert_eq!(pair._0, 7);
    assert_eq!(pair._1, "seven");

    match package.main.makeEither() {
        ffi::exp::main__makeEither::ret::<Host>::Left(value) => {
            assert_eq!(value, 42)
        }
        ffi::exp::main__makeEither::ret::<Host>::Right(_) => unreachable!(),
    }

    let triple = package.main.makeFlat3(8, "eight".to_owned(), true);
    assert_eq!(
        (triple._0, triple._1._0, triple._1._1),
        (8, "eight".to_owned(), true)
    );

    match package.main.makeEither3() {
        ffi::exp::main__makeEither3::ret::<Host>::Left(_) => unreachable!(),
        ffi::exp::main__makeEither3::ret::<Host>::Right(rest) => match rest {
            Sum::Left(_) => unreachable!(),
            Sum::Right(value) => assert!(value),
        }
    }
}
