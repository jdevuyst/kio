#[derive(Clone)]
struct LiveHost;

impl app::host::AppHost for LiveHost {
    type api__Str = String;

    fn api__open(&self) -> Self::api__Str {
        String::from("live")
    }
}

#[derive(Clone)]
struct OldHost;

#[allow(deprecated)]
impl app::host::AppHost for OldHost {
    type api__Str = String;

    fn api__open(&self) -> Self::api__Str {
        String::from("old host")
    }

    fn api__log(&self, _value: Self::api__Str) {
        panic!("removed log method dispatched")
    }

    fn api__archived(
        &self,
        _head: Self::api__Str,
        _value: app::ffi::env::api__archived::arg1<Self>,
    ) -> app::ffi::env::api__archived::ret<Self> {
        panic!("removed archived method dispatched")
    }
}

#[allow(dead_code, deprecated, unreachable_code)]
fn retained_source_must_compile<H: app::host::AppHost>(host: &H) {
    host.api__log(panic!("compile-time-only retained method witness"));
    let _ = host.api__archived(
        panic!("compile-time-only retained method witness"),
        panic!("compile-time-only retained method witness"),
    );
}

fn main() {
    let pkg = app::create_app(LiveHost);
    let input = String::from("live");
    assert_eq!(pkg.api.echo(input.clone()), input);

    let old_pkg = app::create_app(OldHost);
    let old_input = String::from("old host");
    assert_eq!(old_pkg.api.echo(old_input.clone()), old_input);
}
