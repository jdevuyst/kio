//! The two-sided opposite-variance compatibility verdict.
//!
//! Compares a **baseline** contract snapshot (the last sealed signature,
//! recovered by [`super::replay`]) against the **current** snapshot (the
//! live typechecked package, via [`super::ContractSnapshot::from_package`])
//! and classifies every difference.
//!
//! Variance is opposite on the two sides:
//!
//! - **Export side (provisions, covariant):** removing an export is
//!   breaking; adding one is compatible; a retained export whose
//!   normalized type is not alpha+kind-equal is breaking (v1 strict); a
//!   `newtype` whose `pub` member becomes private or whose member
//!   signature changes is breaking; a private-member-only payload change
//!   is invisible (no change recorded).
//!
//! - **Env side (requirements, contravariant):** adding a host decl is
//!   breaking; removing one is compatible at the language level; a
//!   retained host decl whose type is not alpha+kind-equal — *or whose
//!   `role(...)` changed* — is breaking.
//!
//! Plus a **closure re-check**: a removal that strands a still-referenced
//! type (some retained signature still names the removed nominal type)
//! is breaking regardless of side, since the replayed interface would be
//! ill-formed.
//!
//! Classification is **verdict-based, not op-based**: a change is placed
//! under breaking / nonbreaking by its *computed* verdict, so when the
//! v1 strictness is later loosened a compatible `modify` can sit under
//! `nonbreaking` without any format change.

use super::{ContractEntry, ContractKind, ContractSide, ContractSnapshot, QualifiedName};
use std::collections::BTreeSet;

/// The compatibility verdict for a single change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The change preserves backward compatibility.
    Compatible,
    /// The change breaks backward compatibility.
    Breaking,
}

/// How a contract item changed between baseline and current.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangePlacement {
    /// Present in current, absent in baseline.
    Added,
    /// Present in baseline, absent in current.
    Removed,
    /// Present in both, but the recorded shape differs.
    Modified,
}

/// One classified change between the baseline and current contract
/// surfaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub name: QualifiedName,
    pub placement: ChangePlacement,
    pub verdict: Verdict,
    /// A human-readable reason — surfaced in `kio sig` reporting and
    /// used to anchor a diagnostic. Surface vocabulary, not internal
    /// jargon.
    pub detail: String,
}

/// The full comparison report.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompatReport {
    /// Every change, in module-qualified-name order.
    pub changes: Vec<Change>,
}

impl CompatReport {
    /// Whether the report contains any breaking change.
    pub fn is_breaking(&self) -> bool {
        self.changes.iter().any(|c| c.verdict == Verdict::Breaking)
    }

    /// Whether the report records any change at all (breaking or not) —
    /// i.e. the current surface drifted from the baseline.
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// The breaking changes only.
    pub fn breaking(&self) -> impl Iterator<Item = &Change> {
        self.changes
            .iter()
            .filter(|c| c.verdict == Verdict::Breaking)
    }

    /// The compatible changes only.
    pub fn compatible(&self) -> impl Iterator<Item = &Change> {
        self.changes
            .iter()
            .filter(|c| c.verdict == Verdict::Compatible)
    }
}

/// Compare a baseline snapshot against the current snapshot, producing a
/// verdict per difference.
pub fn compare(baseline: &ContractSnapshot, current: &ContractSnapshot) -> CompatReport {
    let mut changes = Vec::new();

    // The set of nominal-type reference tokens still present in the
    // current surface, used by the closure re-check.
    let referenced = referenced_nominals(current);

    let mut names: BTreeSet<&QualifiedName> = BTreeSet::new();
    names.extend(baseline.items.keys());
    names.extend(current.items.keys());

    for name in names {
        match (baseline.items.get(name), current.items.get(name)) {
            (None, Some(cur)) => {
                changes.push(added_change(name, cur));
            }
            (Some(base), None) => {
                changes.push(removed_change(name, base, &referenced));
            }
            (Some(base), Some(cur)) => {
                if let Some(change) = retained_change(name, base, cur) {
                    changes.push(change);
                }
            }
            (None, None) => unreachable!("name came from one of the two maps"),
        }
    }

    CompatReport { changes }
}

/// An added item. Env side (host requirement added) is breaking; export
/// side (provision added) is compatible.
fn added_change(name: &QualifiedName, cur: &ContractEntry) -> Change {
    match cur.side {
        ContractSide::Env => Change {
            name: name.clone(),
            placement: ChangePlacement::Added,
            verdict: Verdict::Breaking,
            detail: format!(
                "host requirement `{name}` added — an existing host does not supply it"
            ),
        },
        ContractSide::Export => Change {
            name: name.clone(),
            placement: ChangePlacement::Added,
            verdict: Verdict::Compatible,
            detail: format!("export `{name}` added"),
        },
    }
}

/// A removed item. Export side (provision removed) is breaking. Env side
/// (host requirement removed) is compatible at the language level —
/// unless the removal strands a still-referenced type (closure
/// re-check), which is breaking regardless of side.
fn removed_change(
    name: &QualifiedName,
    base: &ContractEntry,
    referenced: &BTreeSet<String>,
) -> Change {
    let token = nominal_token(name);
    let strands = referenced.contains(&token);
    if strands {
        return Change {
            name: name.clone(),
            placement: ChangePlacement::Removed,
            verdict: Verdict::Breaking,
            detail: format!(
                "`{name}` removed but is still referenced by a retained contract signature — the \
                 interface would no longer be self-contained"
            ),
        };
    }
    match base.side {
        ContractSide::Export => Change {
            name: name.clone(),
            placement: ChangePlacement::Removed,
            verdict: Verdict::Breaking,
            detail: format!("export `{name}` removed"),
        },
        ContractSide::Env => Change {
            name: name.clone(),
            placement: ChangePlacement::Removed,
            verdict: Verdict::Compatible,
            detail: format!(
                "host requirement `{name}` removed — an existing host already supplies a superset"
            ),
        },
    }
}

/// A retained item — present on both sides. Returns `None` when the
/// shapes are identical (no change), else a classified `Modified`.
fn retained_change(
    name: &QualifiedName,
    base: &ContractEntry,
    cur: &ContractEntry,
) -> Option<Change> {
    // A side flip (host ⇄ export under the same qualified name) is a
    // structural break: the item changed which side of the contract it
    // sits on.
    if base.side != cur.side {
        return Some(Change {
            name: name.clone(),
            placement: ChangePlacement::Modified,
            verdict: Verdict::Breaking,
            detail: format!("`{name}` changed between host requirement and export provision"),
        });
    }

    match (&base.kind, &cur.kind) {
        (ContractKind::HostType { .. }, ContractKind::HostType { .. }) => {
            host_type_change(name, &base.kind, &cur.kind)
        }
        (
            ContractKind::Fn {
                signature: b,
                pure: bp,
            },
            ContractKind::Fn {
                signature: c,
                pure: cp,
            },
        ) => {
            if b != c {
                // Both sides: v1 strict alpha+kind-equality. A non-equal
                // retained fn type is breaking (export narrows/param
                // widens; env type change).
                Some(Change {
                    name: name.clone(),
                    placement: ChangePlacement::Modified,
                    verdict: Verdict::Breaking,
                    detail: format!("`{name}`'s signature changed (no longer alpha+kind-equal)"),
                })
            } else if bp == cp {
                None
            } else if base.side == ContractSide::Env {
                unreachable!("host function contract entries cannot carry purity")
            } else if *cp {
                Some(Change {
                    name: name.clone(),
                    placement: ChangePlacement::Modified,
                    verdict: Verdict::Compatible,
                    detail: format!("export `{name}` is now available to compile-time evaluation"),
                })
            } else {
                Some(Change {
                    name: name.clone(),
                    placement: ChangePlacement::Modified,
                    verdict: Verdict::Breaking,
                    detail: format!(
                        "export `{name}` is no longer available to compile-time evaluation"
                    ),
                })
            }
        }
        (
            ContractKind::Alias {
                param_kinds: bk,
                expansion: be,
            },
            ContractKind::Alias {
                param_kinds: ck,
                expansion: ce,
            },
        ) => {
            if bk == ck && be == ce {
                None
            } else {
                Some(Change {
                    name: name.clone(),
                    placement: ChangePlacement::Modified,
                    verdict: Verdict::Breaking,
                    detail: format!("type alias `{name}`'s expansion changed"),
                })
            }
        }
        (ContractKind::Newtype { .. }, ContractKind::Newtype { .. }) => {
            newtype_change(name, &base.kind, &cur.kind)
        }
        // A kind flip (newtype ⇄ alias ⇄ host type ⇄ fn under one name)
        // is a structural break.
        _ => Some(Change {
            name: name.clone(),
            placement: ChangePlacement::Modified,
            verdict: Verdict::Breaking,
            detail: format!("`{name}`'s declaration kind changed"),
        }),
    }
}

fn host_type_change(
    name: &QualifiedName,
    base: &ContractKind,
    cur: &ContractKind,
) -> Option<Change> {
    let (
        ContractKind::HostType {
            param_kinds: bk,
            role: br,
        },
        ContractKind::HostType {
            param_kinds: ck,
            role: cr,
        },
    ) = (base, cur)
    else {
        unreachable!("host_type_change called on non-host-type kinds");
    };
    if bk == ck && br == cr {
        return None;
    }
    let reason = if br != cr {
        // `role(...)` is part of host-type identity.
        format!("host type `{name}`'s role changed ({br:?} → {cr:?})")
    } else {
        format!("host type `{name}`'s kind/arity changed")
    };
    Some(Change {
        name: name.clone(),
        placement: ChangePlacement::Modified,
        verdict: Verdict::Breaking,
        detail: reason,
    })
}

fn newtype_change(name: &QualifiedName, base: &ContractKind, cur: &ContractKind) -> Option<Change> {
    let (
        ContractKind::Newtype {
            param_kinds: bk,
            surface: bs,
        },
        ContractKind::Newtype {
            param_kinds: ck,
            surface: cs,
        },
    ) = (base, cur)
    else {
        unreachable!("newtype_change called on non-newtype kinds");
    };

    if bk != ck {
        return Some(breaking_modify(
            name,
            format!("newtype `{name}`'s kind/arity changed"),
        ));
    }

    if bs.member_names() != cs.member_names() {
        return Some(breaking_modify(
            name,
            format!("newtype `{name}`'s public member surface changed"),
        ));
    }

    match (bs.payload(), cs.payload()) {
        (Some(b), Some(c)) if b != c => Some(breaking_modify(
            name,
            format!("newtype `{name}`'s payload changed (exposed through a public member)"),
        )),
        _ => None,
    }
}

fn breaking_modify(name: &QualifiedName, detail: String) -> Change {
    Change {
        name: name.clone(),
        placement: ChangePlacement::Modified,
        verdict: Verdict::Breaking,
        detail,
    }
}

/// The `@module/leaf` token a nominal type reference renders to in a
/// canonical signature string (matching `super::normalize`'s rendering).
fn nominal_token(name: &QualifiedName) -> String {
    format!("@{}/{}", name.module_path, name.leaf)
}

/// Every nominal-type reference token appearing in any current contract
/// signature — the closure re-check's "still referenced" set.
fn referenced_nominals(current: &ContractSnapshot) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for entry in current.items.values() {
        collect_tokens(&entry.kind, &mut out);
    }
    out
}

fn collect_tokens(kind: &ContractKind, out: &mut BTreeSet<String>) {
    let scan = |s: &str, out: &mut BTreeSet<String>| {
        // Tokens are `@a/b/Leaf`, terminated by one of the structural
        // punctuation chars the normalizer emits.
        let bytes = s.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'@' {
                let start = i + 1;
                let mut j = start;
                while j < bytes.len() && !matches!(bytes[j], b'(' | b')' | b',' | b'>' | b'<') {
                    j += 1;
                }
                out.insert(format!("@{}", &s[start..j]));
                i = j;
            } else {
                i += 1;
            }
        }
    };
    match kind {
        ContractKind::Fn { signature, .. } => scan(signature, out),
        ContractKind::Alias { expansion, .. } => scan(expansion, out),
        ContractKind::Newtype { surface, .. } => {
            if let Some(payload) = surface.payload() {
                scan(payload, out);
            }
        }
        ContractKind::HostType { .. } => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sig::PublicNewtypeSurface;

    fn qn(m: &str, l: &str) -> QualifiedName {
        QualifiedName::new(m, l)
    }

    fn host_type(role: Option<&str>) -> ContractKind {
        ContractKind::HostType {
            param_kinds: vec![],
            role: role.map(|r| r.to_owned()),
        }
    }

    fn func(sig: &str) -> ContractKind {
        ContractKind::Fn {
            signature: sig.to_owned(),
            pure: false,
        }
    }

    fn pure_func(sig: &str) -> ContractKind {
        ContractKind::Fn {
            signature: sig.to_owned(),
            pure: true,
        }
    }

    fn entry(m: &str, l: &str, side: ContractSide, kind: ContractKind) -> ContractEntry {
        ContractEntry {
            name: qn(m, l),
            side,
            kind,
        }
    }

    fn snapshot(entries: Vec<ContractEntry>) -> ContractSnapshot {
        ContractSnapshot {
            items: entries.into_iter().map(|e| (e.name.clone(), e)).collect(),
        }
    }

    fn single_verdict(base: ContractSnapshot, cur: ContractSnapshot) -> (ChangePlacement, Verdict) {
        let report = compare(&base, &cur);
        assert_eq!(
            report.changes.len(),
            1,
            "expected exactly one change: {report:?}"
        );
        let c = &report.changes[0];
        (c.placement, c.verdict)
    }

    // ---- Export side (covariant) -------------------------------------

    #[test]
    fn export_removed_is_breaking() {
        let base = snapshot(vec![entry(
            "m",
            "f",
            ContractSide::Export,
            func("sig[](@m/T)->."),
        )]);
        let cur = snapshot(vec![]);
        assert_eq!(
            single_verdict(base, cur),
            (ChangePlacement::Removed, Verdict::Breaking)
        );
    }

    #[test]
    fn export_added_is_compatible() {
        let base = snapshot(vec![]);
        let cur = snapshot(vec![entry(
            "m",
            "f",
            ContractSide::Export,
            func("sig[]()->."),
        )]);
        assert_eq!(
            single_verdict(base, cur),
            (ChangePlacement::Added, Verdict::Compatible)
        );
    }

    #[test]
    fn export_signature_changed_is_breaking() {
        let base = snapshot(vec![entry(
            "m",
            "f",
            ContractSide::Export,
            func("sig[](@m/A)->."),
        )]);
        let cur = snapshot(vec![entry(
            "m",
            "f",
            ContractSide::Export,
            func("sig[](@m/B)->."),
        )]);
        assert_eq!(
            single_verdict(base, cur),
            (ChangePlacement::Modified, Verdict::Breaking)
        );
    }

    #[test]
    fn export_signature_unchanged_is_no_change() {
        let base = snapshot(vec![entry(
            "m",
            "f",
            ContractSide::Export,
            func("sig[](@m/A)->."),
        )]);
        let cur = snapshot(vec![entry(
            "m",
            "f",
            ContractSide::Export,
            func("sig[](@m/A)->."),
        )]);
        assert!(compare(&base, &cur).is_empty());
    }

    #[test]
    fn export_gaining_purity_is_compatible() {
        let base = snapshot(vec![entry(
            "m",
            "f",
            ContractSide::Export,
            func("sig[](@m/A)->."),
        )]);
        let cur = snapshot(vec![entry(
            "m",
            "f",
            ContractSide::Export,
            pure_func("sig[](@m/A)->."),
        )]);
        assert_eq!(
            single_verdict(base, cur),
            (ChangePlacement::Modified, Verdict::Compatible)
        );
    }

    #[test]
    fn export_losing_purity_is_breaking() {
        let base = snapshot(vec![entry(
            "m",
            "f",
            ContractSide::Export,
            pure_func("sig[](@m/A)->."),
        )]);
        let cur = snapshot(vec![entry(
            "m",
            "f",
            ContractSide::Export,
            func("sig[](@m/A)->."),
        )]);
        assert_eq!(
            single_verdict(base, cur),
            (ChangePlacement::Modified, Verdict::Breaking)
        );
    }

    #[test]
    fn newtype_pub_member_becomes_private_is_breaking() {
        let base_kind = ContractKind::Newtype {
            param_kinds: vec![],
            surface: PublicNewtypeSurface::ConstructorAndProjector {
                constructor: "mk".into(),
                projector: "un".into(),
                payload: "@m/A".into(),
            },
        };
        let cur_kind = ContractKind::Newtype {
            param_kinds: vec![],
            surface: PublicNewtypeSurface::Projector {
                name: "un".into(),
                payload: "@m/A".into(),
            },
        };
        let base = snapshot(vec![entry("m", "N", ContractSide::Export, base_kind)]);
        let cur = snapshot(vec![entry("m", "N", ContractSide::Export, cur_kind)]);
        assert_eq!(
            single_verdict(base, cur),
            (ChangePlacement::Modified, Verdict::Breaking)
        );
    }

    #[test]
    fn newtype_private_member_payload_change_is_invisible() {
        let base_kind = ContractKind::Newtype {
            param_kinds: vec![],
            surface: PublicNewtypeSurface::Opaque,
        };
        let cur_kind = ContractKind::Newtype {
            param_kinds: vec![],
            surface: PublicNewtypeSurface::Opaque,
        };
        let base = snapshot(vec![entry("m", "N", ContractSide::Export, base_kind)]);
        let cur = snapshot(vec![entry("m", "N", ContractSide::Export, cur_kind)]);
        // No pub member exposes the payload, so a payload change between
        // these (both `None`) is invisible — no change recorded.
        assert!(compare(&base, &cur).is_empty());
    }

    #[test]
    fn newtype_visible_payload_change_is_breaking() {
        let mk = |payload: &str| ContractKind::Newtype {
            param_kinds: vec![],
            surface: PublicNewtypeSurface::ConstructorAndProjector {
                constructor: "mk".into(),
                projector: "un".into(),
                payload: payload.to_owned(),
            },
        };
        let base = snapshot(vec![entry("m", "N", ContractSide::Export, mk("@m/A"))]);
        let cur = snapshot(vec![entry("m", "N", ContractSide::Export, mk("@m/B"))]);
        assert_eq!(
            single_verdict(base, cur),
            (ChangePlacement::Modified, Verdict::Breaking)
        );
    }

    // ---- Env side (contravariant) ------------------------------------

    #[test]
    fn host_added_is_breaking() {
        let base = snapshot(vec![]);
        let cur = snapshot(vec![entry(
            "m",
            "T",
            ContractSide::Env,
            host_type(Some("i32")),
        )]);
        assert_eq!(
            single_verdict(base, cur),
            (ChangePlacement::Added, Verdict::Breaking)
        );
    }

    #[test]
    fn host_removed_is_compatible() {
        let base = snapshot(vec![entry(
            "m",
            "T",
            ContractSide::Env,
            host_type(Some("i32")),
        )]);
        let cur = snapshot(vec![]);
        assert_eq!(
            single_verdict(base, cur),
            (ChangePlacement::Removed, Verdict::Compatible)
        );
    }

    #[test]
    fn host_type_role_change_is_breaking() {
        let base = snapshot(vec![entry(
            "m",
            "T",
            ContractSide::Env,
            host_type(Some("str")),
        )]);
        let cur = snapshot(vec![entry(
            "m",
            "T",
            ContractSide::Env,
            host_type(Some("bool")),
        )]);
        assert_eq!(
            single_verdict(base, cur),
            (ChangePlacement::Modified, Verdict::Breaking)
        );
    }

    #[test]
    fn host_fn_signature_change_is_breaking() {
        let base = snapshot(vec![entry(
            "m",
            "open",
            ContractSide::Env,
            func("sig[]()->@m/H"),
        )]);
        let cur = snapshot(vec![entry(
            "m",
            "open",
            ContractSide::Env,
            func("sig[](@m/P)->@m/H"),
        )]);
        assert_eq!(
            single_verdict(base, cur),
            (ChangePlacement::Modified, Verdict::Breaking)
        );
    }

    // ---- Cross-cutting -----------------------------------------------

    #[test]
    fn side_flip_is_breaking() {
        let base = snapshot(vec![entry("m", "x", ContractSide::Env, func("sig[]()->."))]);
        let cur = snapshot(vec![entry(
            "m",
            "x",
            ContractSide::Export,
            func("sig[]()->."),
        )]);
        assert_eq!(
            single_verdict(base, cur),
            (ChangePlacement::Modified, Verdict::Breaking)
        );
    }

    #[test]
    fn host_removal_that_strands_referenced_type_is_breaking() {
        // `m.H` is a host type; `m.f` is a retained export referencing
        // it. Removing `m.H` strands the still-referenced type ⇒ the
        // closure re-check makes the removal breaking even though a host
        // removal is otherwise compatible.
        let base = snapshot(vec![
            entry("m", "H", ContractSide::Env, host_type(Some("i32"))),
            entry("m", "f", ContractSide::Export, func("sig[]()->@m/H")),
        ]);
        let cur = snapshot(vec![entry(
            "m",
            "f",
            ContractSide::Export,
            func("sig[]()->@m/H"),
        )]);
        let report = compare(&base, &cur);
        let h = report
            .changes
            .iter()
            .find(|c| c.name == qn("m", "H"))
            .expect("H change present");
        assert_eq!(h.placement, ChangePlacement::Removed);
        assert_eq!(
            h.verdict,
            Verdict::Breaking,
            "stranded removal must be breaking: {h:?}"
        );
    }

    #[test]
    fn host_removal_hidden_behind_all_private_newtype_is_compatible() {
        let newtype = ContractKind::Newtype {
            param_kinds: vec![],
            surface: PublicNewtypeSurface::Opaque,
        };
        let base = snapshot(vec![
            entry("m", "H", ContractSide::Env, host_type(Some("i32"))),
            entry("m", "N", ContractSide::Export, newtype.clone()),
        ]);
        let cur = snapshot(vec![entry("m", "N", ContractSide::Export, newtype)]);
        let report = compare(&base, &cur);
        let h = report
            .changes
            .iter()
            .find(|c| c.name == qn("m", "H"))
            .expect("H change present");
        assert_eq!(h.placement, ChangePlacement::Removed);
        assert_eq!(h.verdict, Verdict::Compatible);
    }

    #[test]
    fn host_removal_referenced_by_one_public_newtype_member_is_breaking() {
        let newtype = ContractKind::Newtype {
            param_kinds: vec![],
            surface: PublicNewtypeSurface::Constructor {
                name: "mk".into(),
                payload: "@m/H".into(),
            },
        };
        let base = snapshot(vec![
            entry("m", "H", ContractSide::Env, host_type(Some("i32"))),
            entry("m", "N", ContractSide::Export, newtype.clone()),
        ]);
        let cur = snapshot(vec![entry("m", "N", ContractSide::Export, newtype)]);

        let report = compare(&base, &cur);
        let h = report
            .changes
            .iter()
            .find(|c| c.name == qn("m", "H"))
            .expect("H change present");
        assert_eq!(h.placement, ChangePlacement::Removed);
        assert_eq!(h.verdict, Verdict::Breaking);
    }

    #[test]
    fn host_removal_not_referenced_is_compatible() {
        // Same as above but the export no longer references `m.H` — the
        // removal is compatible.
        let base = snapshot(vec![
            entry("m", "H", ContractSide::Env, host_type(Some("i32"))),
            entry("m", "f", ContractSide::Export, func("sig[]()->.")),
        ]);
        let cur = snapshot(vec![entry(
            "m",
            "f",
            ContractSide::Export,
            func("sig[]()->."),
        )]);
        let report = compare(&base, &cur);
        let h = report
            .changes
            .iter()
            .find(|c| c.name == qn("m", "H"))
            .unwrap();
        assert_eq!(h.verdict, Verdict::Compatible);
    }

    #[test]
    fn module_qualified_identity_distinguishes_same_leaf() {
        // `a.main` and `b.main` are distinct keys; removing `a.main` and
        // adding `b.main` are two separate changes, not a modify.
        let base = snapshot(vec![entry(
            "a",
            "main",
            ContractSide::Export,
            func("sig[]()->."),
        )]);
        let cur = snapshot(vec![entry(
            "b",
            "main",
            ContractSide::Export,
            func("sig[]()->."),
        )]);
        let report = compare(&base, &cur);
        assert_eq!(report.changes.len(), 2);
        assert!(
            report
                .changes
                .iter()
                .any(|c| c.name == qn("a", "main") && c.placement == ChangePlacement::Removed)
        );
        assert!(
            report
                .changes
                .iter()
                .any(|c| c.name == qn("b", "main") && c.placement == ChangePlacement::Added)
        );
    }
}
