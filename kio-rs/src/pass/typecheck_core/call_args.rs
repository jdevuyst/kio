//! Replay of checked canonical argument slots. The phase owners materialize
//! types and values; this cursor only decides which written argument a slot
//! consumes. It neither resolves a callee nor infers a call plan.

#[derive(Clone, Copy, Debug)]
pub(crate) enum WrittenCallArg {
    Type,
    #[cfg(any(feature = "surface", test))]
    Infer,
    Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CanonicalCallSlot {
    ImplicitUnit,
    Written,
    ResolvedType { consume_written: bool },
}

#[derive(Default)]
pub(crate) struct CanonicalCallCursor {
    written: usize,
    canonical: usize,
}

impl CanonicalCallCursor {
    #[cfg(any(feature = "surface", test))]
    pub(crate) fn written(&self) -> usize {
        self.written
    }

    pub(crate) fn canonical(&self) -> usize {
        self.canonical
    }

    pub(crate) fn advance(
        &mut self,
        written: Option<WrittenCallArg>,
        implicit_unit: bool,
        resolved: bool,
        value_at_type_slot: bool,
    ) -> Option<CanonicalCallSlot> {
        use CanonicalCallSlot::{ImplicitUnit, ResolvedType, Written};
        let slot = if implicit_unit {
            ImplicitUnit
        } else {
            match (written, resolved) {
                // A written concrete type always wins over recorded inference.
                (Some(WrittenCallArg::Type), _) | (Some(_), false) => Written,
                #[cfg(any(feature = "surface", test))]
                (Some(WrittenCallArg::Infer), true) => ResolvedType {
                    consume_written: true,
                },
                (Some(WrittenCallArg::Value), true) => ResolvedType {
                    consume_written: value_at_type_slot,
                },
                (None, true) => ResolvedType {
                    consume_written: false,
                },
                (None, false) => return None,
            }
        };
        self.canonical += 1;
        if matches!(
            slot,
            Written
                | ResolvedType {
                    consume_written: true
                }
        ) {
            self.written += 1;
        }
        Some(slot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_call_cursor_preserves_all_slot_transitions() {
        use CanonicalCallSlot::{ImplicitUnit, ResolvedType, Written};
        for arg in [
            None,
            Some(WrittenCallArg::Type),
            Some(WrittenCallArg::Infer),
            Some(WrittenCallArg::Value),
        ] {
            for implicit in [false, true] {
                for resolved in [false, true] {
                    for reclassified in [false, true] {
                        let expected = if implicit {
                            Some(ImplicitUnit)
                        } else {
                            match arg {
                                Some(WrittenCallArg::Type) => Some(Written),
                                Some(WrittenCallArg::Infer) if resolved => Some(ResolvedType {
                                    consume_written: true,
                                }),
                                Some(WrittenCallArg::Value) if resolved => Some(ResolvedType {
                                    consume_written: reclassified,
                                }),
                                Some(_) => Some(Written),
                                None if resolved => Some(ResolvedType {
                                    consume_written: false,
                                }),
                                None => None,
                            }
                        };
                        let mut cursor = CanonicalCallCursor::default();
                        assert_eq!(
                            cursor.advance(arg, implicit, resolved, reclassified),
                            expected
                        );
                        assert_eq!(cursor.canonical(), usize::from(expected.is_some()));
                        assert_eq!(
                            cursor.written(),
                            usize::from(matches!(
                                expected,
                                Some(
                                    Written
                                        | ResolvedType {
                                            consume_written: true
                                        }
                                )
                            ))
                        );
                    }
                }
            }
        }
    }
}
