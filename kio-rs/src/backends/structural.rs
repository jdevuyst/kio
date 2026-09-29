//! Shared backend-emission helpers for recovered structural IR.
//!
//! The recovery pass already turns intrinsic product/sum spines into
//! n-ary `Enriched*` nodes. Emitters still have backend-specific syntax,
//! but cross-backend decisions about those n-ary shapes live here.

use crate::ast::{Expr, Routed};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductRebuildPlan {
    pub source: String,
    pub source_arity: usize,
    pub slots: Vec<Option<usize>>,
}

pub fn render_cached_product_rebuild<E>(
    plan: &ProductRebuildPlan,
    mut cached_slot: impl FnMut(usize) -> String,
    mut fresh_slot: impl FnMut(usize) -> Result<String, E>,
    product_from_slots: impl FnOnce(&[String]) -> String,
    wrap: impl FnOnce(&str, usize, String) -> String,
) -> Result<String, E> {
    let mut rendered = Vec::with_capacity(plan.slots.len());
    for (i, projected_slot) in plan.slots.iter().enumerate() {
        let value = match projected_slot {
            Some(slot) => cached_slot(*slot),
            None => fresh_slot(i)?,
        };
        rendered.push(value);
    }
    let product = product_from_slots(&rendered);
    Ok(wrap(&plan.source, plan.source_arity, product))
}

pub fn bound_product_rebuild_plan(items: &[&Expr<Routed>]) -> Option<ProductRebuildPlan> {
    let mut source: Option<(&str, usize)> = None;
    let mut projection_count = 0usize;
    let mut slots = Vec::with_capacity(items.len());
    for item in items {
        if let Some((name, index, arity)) = bound_product_projection(item) {
            match source {
                None => source = Some((name, arity)),
                Some((source_name, source_arity))
                    if source_name == name && source_arity == arity => {}
                Some(_) => return None,
            }
            projection_count += 1;
            slots.push(Some(index));
        } else {
            slots.push(None);
        }
    }
    let (name, arity) = source?;
    if projection_count < 2 {
        return None;
    }
    Some(ProductRebuildPlan {
        source: name.to_owned(),
        source_arity: arity,
        slots,
    })
}

fn bound_product_projection(item: &Expr<Routed>) -> Option<(&str, usize, usize)> {
    match item {
        Expr::EnrichedProject {
            target,
            index,
            arity,
            ..
        }
        | Expr::EnrichedFieldGet {
            target,
            index,
            arity,
            ..
        } => match target.as_ref() {
            Expr::LowBoundRef { name, .. } if *index < *arity => {
                Some((name.as_str(), *index, *arity))
            }
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Meta, Type};
    use crate::span::Span;

    fn meta() -> Meta<Routed> {
        Meta::new(Span::new(0, 0))
    }

    fn unit_ty() -> Type<Routed> {
        Type::Unit { meta: meta() }
    }

    fn unit_expr() -> Expr<Routed> {
        Expr::Unit {
            occurrence: Default::default(),
            meta: meta(),
        }
    }

    fn bound(name: &str) -> Expr<Routed> {
        Expr::LowBoundRef {
            occurrence: Default::default(),
            name: name.to_owned(),
            meta: meta(),
            ext: (),
        }
    }

    fn project(source: &str, index: usize, arity: usize) -> Expr<Routed> {
        Expr::EnrichedProject {
            occurrence: Default::default(),
            target: Box::new(bound(source)),
            index,
            arity,
            target_ty: unit_ty(),
            meta: meta(),
            ext: (),
        }
    }

    fn field_get(source: &str, index: usize, arity: usize) -> Expr<Routed> {
        Expr::EnrichedFieldGet {
            occurrence: Default::default(),
            target: Box::new(bound(source)),
            field_name: format!("field_{index}"),
            index,
            arity,
            target_ty: unit_ty(),
            meta: meta(),
            ext: (),
        }
    }

    fn plan_for(items: &[Expr<Routed>]) -> Option<ProductRebuildPlan> {
        let refs: Vec<&Expr<Routed>> = items.iter().collect();
        bound_product_rebuild_plan(&refs)
    }

    #[test]
    fn rebuild_plan_requires_at_least_two_projected_slots() {
        let items = vec![project("r", 0, 3), unit_expr()];
        assert_eq!(plan_for(&items), None);
    }

    #[test]
    fn rebuild_plan_allows_mixed_updated_slots() {
        let items = vec![
            project("r", 0, 4),
            unit_expr(),
            field_get("r", 2, 4),
            project("r", 3, 4),
        ];
        assert_eq!(
            plan_for(&items),
            Some(ProductRebuildPlan {
                source: "r".to_owned(),
                source_arity: 4,
                slots: vec![Some(0), None, Some(2), Some(3)],
            })
        );
    }

    #[test]
    fn rebuild_plan_rejects_mixed_sources() {
        let items = vec![project("left", 0, 3), project("right", 1, 3)];
        assert_eq!(plan_for(&items), None);
    }

    #[test]
    fn rebuild_plan_rejects_invalid_projection_index() {
        let items = vec![project("r", 0, 2), project("r", 2, 2)];
        assert_eq!(plan_for(&items), None);
    }

    #[test]
    fn render_cached_rebuild_keeps_projection_and_fresh_slots_in_order() {
        let plan = ProductRebuildPlan {
            source: "r".to_owned(),
            source_arity: 4,
            slots: vec![Some(0), None, Some(2), Some(3)],
        };
        let rendered: Result<String, ()> = render_cached_product_rebuild(
            &plan,
            |slot| format!("cached[{slot}]"),
            |i| Ok(format!("fresh[{i}]")),
            |slots| slots.join(" & "),
            |source, arity, product| format!("from {source}/{arity}: {product}"),
        );
        assert_eq!(
            rendered,
            Ok("from r/4: cached[0] & fresh[1] & cached[2] & cached[3]".to_owned())
        );
    }
}
