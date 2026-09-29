#[derive(Clone)]
struct Host;

impl build_rust_module_alias_scope_collision::host::BuildRustModuleAliasScopeCollisionHost
    for Host
{
}

fn main() {
    let package =
        build_rust_module_alias_scope_collision::create_buildRustModuleAliasScopeCollision(
            Host,
        );
    let a = &package.buildRustModuleAliasScopeCollision.a.main;
    a.consume(a.make());

    let b = &package.buildRustModuleAliasScopeCollision.b.main;
    b.consume(b.make());
}
