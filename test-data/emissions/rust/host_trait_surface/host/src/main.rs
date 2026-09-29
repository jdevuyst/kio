use rust_host_trait_surface::host::RustHostTraitSurfaceHost;
use std::cell::RefCell;
use std::rc::Rc;

fn require_i32_role<T: Clone + PartialEq + 'static + From<i32>>() {}
fn require_string_role<T: Clone + PartialEq + 'static + From<String>>() {}

#[derive(Clone, Default)]
struct Host {
    seen: Rc<RefCell<Vec<String>>>,
}

impl RustHostTraitSurfaceHost for Host {
    type main__I32 = i32;
    type main__OwnedStr = String;
    type main__Str = String;

    fn main__print(&self, value: Self::main__Str) {
        self.seen.borrow_mut().push(format!("print:{value}"));
    }

    fn main__enqueue(&self, value: Self::main__OwnedStr) {
        self.seen.borrow_mut().push(format!("owned:{value}"));
    }

    fn main__id<T: rust_host_trait_surface::shapes::KioType>(
        &self,
        value: T::Facade,
    ) -> T::Facade {
        value
    }

    fn main__intToStr(&self, value: Self::main__I32) -> Self::main__Str {
        value.to_string()
    }
}

fn main() {
    require_i32_role::<<Host as RustHostTraitSurfaceHost>::main__I32>();
    require_string_role::<<Host as RustHostTraitSurfaceHost>::main__Str>();
    require_string_role::<<Host as RustHostTraitSurfaceHost>::main__OwnedStr>();

    let host = Host::default();
    let seen = Rc::clone(&host.seen);
    let package = rust_host_trait_surface::create_rustHostTraitSurface(host);

    package.main.sayHello("hello".to_owned());
    package.main.enqueueValue("queued".to_owned());
    assert_eq!(
        package
            .main
            .identity::<rust_host_trait_surface::shapes::KioNative<i32>>(42_i32),
        42,
    );
    assert_eq!(package.main.render(7), "7");
    assert_eq!(
        seen.borrow().as_slice(),
        ["print:hello".to_owned(), "owned:queued".to_owned()],
    );
}
