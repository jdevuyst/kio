//! Boundary actions over the shared canonical topology. Hosts choose their
//! established ownership policy; that choice does not change type structure,
//! nominal authority, or the direction reversal at a callable domain.

use super::boundary_facade::FacadeBinderId;
use super::value_plan::{ValueUse, ValueUseId};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ValueProtectionPolicy {
    Snapshot,
    Validate,
}

pub(crate) enum ValueProtectionUse {
    Identity,
    Bound(FacadeBinderId),
    Product {
        left: ValueUseId,
        right: ValueUseId,
        snapshot: bool,
    },
    Sum {
        left: ValueUseId,
        right: ValueUseId,
        snapshot: bool,
    },
    Function {
        domain: ValueUseId,
        result: ValueUseId,
    },
    Forall {
        result: ValueUseId,
    },
    Application {
        snapshot: bool,
    },
}

impl ValueProtectionPolicy {
    pub(crate) fn project(self, value: &ValueUse) -> ValueProtectionUse {
        match value {
            ValueUse::Unit | ValueUse::Bottom => ValueProtectionUse::Identity,
            ValueUse::Bound(binder) => ValueProtectionUse::Bound(*binder),
            ValueUse::Product { left, right } => ValueProtectionUse::Product {
                left: *left,
                right: *right,
                snapshot: self == Self::Snapshot,
            },
            ValueUse::Sum { left, right } => ValueProtectionUse::Sum {
                left: *left,
                right: *right,
                snapshot: self == Self::Snapshot,
            },
            ValueUse::Function { domain, result, .. } => ValueProtectionUse::Function {
                domain: *domain,
                result: *result,
            },
            ValueUse::Forall { result, .. } => ValueProtectionUse::Forall { result: *result },
            ValueUse::Nominal(_) | ValueUse::Apply { .. } => ValueProtectionUse::Application {
                snapshot: self == Self::Snapshot,
            },
        }
    }
}
