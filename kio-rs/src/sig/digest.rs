//! Canonical contract-surface digest.
//!
//! A [`super::ContractSnapshot`] is the backend-independent, normalized
//! projection of a package's contract surface (env + export items, keyed
//! by module-qualified name). [`contract_digest`] hashes it to a stable
//! hex string: two snapshots produce the same digest iff they are equal
//! as contract surfaces.
//!
//! The digest is what a consumer's `<local>.lock.kio` records for a git
//! dependency (the `sig "<…>";` key): the pinned commit's contract
//! surface, fingerprinted so a re-pin can tell whether the surface moved
//! without storing the whole surface in the lock. It is *not* a build
//! cache key — it never folds in a compiler version, target, or
//! implementation tag; it is a pure function of the contract surface so
//! the same surface digests identically across implementations and across
//! time.
//!
//! Serialization is length-framed (every variable-length field is
//! preceded by its byte length) so no concatenation of distinct fields
//! can alias a different one. The `BTreeMap` iteration over
//! [`super::ContractSnapshot::items`] is already module-qualified-name
//! ordered, so the byte stream is deterministic.

use super::{ContractEntry, ContractKind, ContractSide, ContractSnapshot, PublicNewtypeSurface};

/// The canonical contract-surface digest of a snapshot: a 64-char blake3
/// hex string over the length-framed serialization of every contract
/// item in module-qualified-name order.
pub fn contract_digest(snapshot: &ContractSnapshot) -> String {
    blake3::hash(&contract_bytes(snapshot)).to_hex().to_string()
}

fn contract_bytes(snapshot: &ContractSnapshot) -> Vec<u8> {
    let mut out = Vec::new();
    // Domain-separation tag + a serialization-format version. Bumping the
    // version invalidates every recorded digest deliberately (a format
    // change must not silently compare equal across the boundary).
    write_framed(&mut out, b"kio-contract-digest-v1");
    write_u64(&mut out, snapshot.items.len() as u64);
    for (name, entry) in &snapshot.items {
        write_framed(&mut out, name.module_path.as_bytes());
        write_framed(&mut out, name.leaf.as_bytes());
        write_entry(&mut out, entry);
    }
    out
}

fn write_entry(out: &mut Vec<u8>, entry: &ContractEntry) {
    out.push(side_tag(entry.side));
    write_kind(out, &entry.kind);
}

fn side_tag(side: ContractSide) -> u8 {
    match side {
        ContractSide::Env => 0,
        ContractSide::Export => 1,
    }
}

fn write_kind(out: &mut Vec<u8>, kind: &ContractKind) {
    match kind {
        ContractKind::HostType { param_kinds, role } => {
            out.push(0);
            write_strings(out, param_kinds);
            match role {
                Some(r) => {
                    out.push(1);
                    write_framed(out, r.as_bytes());
                }
                None => {
                    out.push(0);
                }
            }
        }
        ContractKind::Fn { signature, pure } => {
            out.push(1);
            write_framed(out, signature.as_bytes());
            out.push(*pure as u8);
        }
        ContractKind::Alias {
            param_kinds,
            expansion,
        } => {
            out.push(2);
            write_strings(out, param_kinds);
            write_framed(out, expansion.as_bytes());
        }
        ContractKind::Newtype {
            param_kinds,
            surface,
        } => {
            out.push(3);
            write_strings(out, param_kinds);
            write_newtype_surface(out, surface);
        }
    }
}

fn write_newtype_surface(out: &mut Vec<u8>, surface: &PublicNewtypeSurface) {
    match surface {
        PublicNewtypeSurface::Opaque => out.push(0),
        PublicNewtypeSurface::Constructor { name, payload } => {
            out.push(1);
            write_framed(out, name.as_bytes());
            write_framed(out, payload.as_bytes());
        }
        PublicNewtypeSurface::Projector { name, payload } => {
            out.push(2);
            write_framed(out, name.as_bytes());
            write_framed(out, payload.as_bytes());
        }
        PublicNewtypeSurface::ConstructorAndProjector {
            constructor,
            projector,
            payload,
        } => {
            out.push(3);
            write_framed(out, constructor.as_bytes());
            write_framed(out, projector.as_bytes());
            write_framed(out, payload.as_bytes());
        }
    }
}

fn write_strings(out: &mut Vec<u8>, strings: &[String]) {
    write_u64(out, strings.len() as u64);
    for s in strings {
        write_framed(out, s.as_bytes());
    }
}

/// Hash `bytes` framed by its length, so adjacent fields never alias.
fn write_framed(out: &mut Vec<u8>, bytes: &[u8]) {
    write_u64(out, bytes.len() as u64);
    out.extend_from_slice(bytes);
}

fn write_u64(out: &mut Vec<u8>, n: u64) {
    out.extend_from_slice(&n.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sig::{PublicNewtypeSurface, QualifiedName};
    use std::collections::BTreeMap;

    fn host_fn(module: &str, leaf: &str, sig: &str) -> (QualifiedName, ContractEntry) {
        let name = QualifiedName::new(module, leaf);
        (
            name.clone(),
            ContractEntry {
                name,
                side: ContractSide::Env,
                kind: ContractKind::Fn {
                    signature: sig.to_owned(),
                    pure: false,
                },
            },
        )
    }

    fn export_fn(module: &str, leaf: &str, sig: &str) -> (QualifiedName, ContractEntry) {
        let name = QualifiedName::new(module, leaf);
        (
            name.clone(),
            ContractEntry {
                name,
                side: ContractSide::Export,
                kind: ContractKind::Fn {
                    signature: sig.to_owned(),
                    pure: false,
                },
            },
        )
    }

    fn snapshot(entries: Vec<(QualifiedName, ContractEntry)>) -> ContractSnapshot {
        ContractSnapshot {
            items: entries.into_iter().collect::<BTreeMap<_, _>>(),
        }
    }

    fn export_newtype(payload: &str) -> (QualifiedName, ContractEntry) {
        let name = QualifiedName::new("api", "Opaque");
        (
            name.clone(),
            ContractEntry {
                name,
                side: ContractSide::Export,
                kind: ContractKind::Newtype {
                    param_kinds: vec![],
                    surface: PublicNewtypeSurface::Constructor {
                        name: "mk".to_owned(),
                        payload: payload.to_owned(),
                    },
                },
            },
        )
    }

    #[test]
    fn digest_is_stable_and_64_hex() {
        let s = snapshot(vec![export_fn("api", "f", ". -> @api/H")]);
        let a = contract_digest(&s);
        let b = contract_digest(&s);
        assert_eq!(a, b, "same snapshot digests identically");
        assert_eq!(a.len(), 64, "blake3 hex is 64 chars");
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn digest_is_insensitive_to_insertion_order() {
        let a = snapshot(vec![
            export_fn("api", "f", ". -> @api/H"),
            export_fn("api", "g", ". -> @api/H"),
        ]);
        let b = snapshot(vec![
            export_fn("api", "g", ". -> @api/H"),
            export_fn("api", "f", ". -> @api/H"),
        ]);
        assert_eq!(contract_digest(&a), contract_digest(&b));
    }

    #[test]
    fn framing_distinguishes_adjacent_field_boundaries() {
        let left = snapshot(vec![export_fn("a", "bc", ". -> @api/H")]);
        let right = snapshot(vec![export_fn("ab", "c", ". -> @api/H")]);
        assert_ne!(contract_digest(&left), contract_digest(&right));
    }

    #[test]
    fn added_export_changes_the_digest() {
        let base = snapshot(vec![export_fn("api", "f", ". -> @api/H")]);
        let grown = snapshot(vec![
            export_fn("api", "f", ". -> @api/H"),
            export_fn("api", "g", ". -> @api/H"),
        ]);
        assert_ne!(contract_digest(&base), contract_digest(&grown));
    }

    #[test]
    fn changed_signature_changes_the_digest() {
        let a = snapshot(vec![export_fn("api", "f", ". -> @api/H")]);
        let b = snapshot(vec![export_fn("api", "f", "(@api/H) -> @api/H")]);
        assert_ne!(contract_digest(&a), contract_digest(&b));
    }

    #[test]
    fn export_purity_changes_the_digest() {
        let impure = snapshot(vec![export_fn("api", "f", ". -> @api/H")]);
        let mut pure_snapshot = impure.clone();
        let ContractKind::Fn { pure: is_pure, .. } = &mut pure_snapshot
            .items
            .get_mut(&QualifiedName::new("api", "f"))
            .unwrap()
            .kind
        else {
            panic!("f should be a function");
        };
        *is_pure = true;
        assert_ne!(contract_digest(&impure), contract_digest(&pure_snapshot));
    }

    #[test]
    fn side_is_part_of_the_digest() {
        // An item that flips env <-> export is a breaking side flip; the
        // digest must distinguish it even when the signature string is
        // identical.
        let env = snapshot(vec![host_fn("api", "f", ". -> @api/H")]);
        let export = snapshot(vec![export_fn("api", "f", ". -> @api/H")]);
        assert_ne!(contract_digest(&env), contract_digest(&export));
    }

    #[test]
    fn visible_newtype_payload_changes_the_digest() {
        let left = snapshot(vec![export_newtype("@api/Left")]);
        let right = snapshot(vec![export_newtype("@api/Right")]);
        assert_ne!(contract_digest(&left), contract_digest(&right));
    }

    #[test]
    fn canonical_v1_newtype_bytes_and_digest_are_pinned() {
        let snapshot = snapshot(vec![export_newtype("@api/Value")]);
        let bytes_hex = contract_bytes(&snapshot)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert_eq!(
            bytes_hex,
            "16000000000000006b696f2d636f6e74726163742d6469676573742d7631\
             0100000000000000030000000000000061706906000000000000004f7061717565\
             010300000000000000000102000000000000006d6b0a00000000000000406170692f56616c7565"
        );
        assert_eq!(
            contract_digest(&snapshot),
            "592d5a5b44c9c02ff46eb3291398abeaa4c2ec35e72c77fbef40e3eb4d698db1"
        );
    }
}
