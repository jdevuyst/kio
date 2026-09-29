use kio_lang::ast::{Enriched, Phase, RoleShape, Type};
use kio_lang::pass::typecheck_core::{ModuleEnv, unique_role_admitted_type};
use kio_lang::span::Span;

fn call_unique_role_helper_with_phase_only<P: Phase>(env: &ModuleEnv<'_, P>) -> Option<Type<P>> {
    unique_role_admitted_type(RoleShape::Bool, env, Span::new(0, 0))
}

#[test]
fn unique_role_helper_public_api_accepts_non_resolver_phases() {
    let _helper = call_unique_role_helper_with_phase_only::<Enriched>;
}
