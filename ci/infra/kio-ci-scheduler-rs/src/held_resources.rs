//! Canonical parsing and acquisition ordering for inherited scheduler resources.

use std::fmt;

/// The order in which a nested command may acquire scheduler resources.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum HeldResource {
    Work,
    Cargo,
    Compiler,
}

impl HeldResource {
    const ALL: [Self; 3] = [Self::Work, Self::Cargo, Self::Compiler];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Work => "work",
            Self::Cargo => "cargo",
            Self::Compiler => "compiler",
        }
    }

    const fn bit(self) -> u8 {
        1 << self as u8
    }

    fn parse(token: &str) -> Option<Self> {
        match token {
            "work" => Some(Self::Work),
            "cargo" => Some(Self::Cargo),
            "compiler" => Some(Self::Compiler),
            _ => None,
        }
    }
}

impl fmt::Display for HeldResource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Resources inherited from an outer scheduler invocation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HeldResources {
    bits: u8,
}

impl HeldResources {
    /// Parse the value of `KIO_CI_SCHEDULE_HELD`.
    ///
    /// An absent or empty value holds no resources. Non-empty values must be
    /// a comma-separated subset of `work,cargo,compiler` in that order.
    pub fn parse(value: Option<&str>) -> Result<Self, HeldResourcesError> {
        let Some(value) = value.filter(|value| !value.is_empty()) else {
            return Ok(Self::default());
        };

        let mut held = Self::default();
        let mut previous = None;
        for (index, token) in value.split(',').enumerate() {
            if token.is_empty() {
                return Err(HeldResourcesError::EmptySegment { index });
            }
            let resource =
                HeldResource::parse(token).ok_or_else(|| HeldResourcesError::UnknownResource {
                    token: token.to_owned(),
                })?;
            if held.contains(resource) {
                return Err(HeldResourcesError::DuplicateResource { resource });
            }
            if let Some(previous) = previous
                && resource < previous
            {
                return Err(HeldResourcesError::OutOfOrder { previous, resource });
            }
            held.bits |= resource.bit();
            previous = Some(resource);
        }
        Ok(held)
    }

    pub const fn contains(self, resource: HeldResource) -> bool {
        self.bits & resource.bit() != 0
    }

    /// Combine two independently validated inherited-resource inventories.
    pub const fn union(self, other: Self) -> Self {
        Self {
            bits: self.bits | other.bits,
        }
    }

    /// Determine whether a request reuses an inherited resource or may acquire
    /// a new one without violating `work -> cargo -> compiler` ordering.
    pub fn request(self, resource: HeldResource) -> Result<ResourceRequest, HeldResourcesError> {
        if self.contains(resource) {
            return Ok(ResourceRequest::Reuse);
        }
        if let Some(already_held) = HeldResource::ALL
            .into_iter()
            .find(|held| *held > resource && self.contains(*held))
        {
            return Err(HeldResourcesError::EarlierResourceRequested {
                requested: resource,
                already_held,
            });
        }
        Ok(ResourceRequest::Acquire)
    }

    /// Return the canonical inherited-resource set after a successful request.
    pub fn with_requested(self, resource: HeldResource) -> Result<Self, HeldResourcesError> {
        match self.request(resource)? {
            ResourceRequest::Reuse => Ok(self),
            ResourceRequest::Acquire => Ok(Self {
                bits: self.bits | resource.bit(),
            }),
        }
    }

    /// Encode the set for `KIO_CI_SCHEDULE_HELD` propagation.
    pub fn encode(self) -> String {
        HeldResource::ALL
            .into_iter()
            .filter(|resource| self.contains(*resource))
            .map(HeldResource::as_str)
            .collect::<Vec<_>>()
            .join(",")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceRequest {
    Reuse,
    Acquire,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HeldResourcesError {
    EmptySegment {
        index: usize,
    },
    UnknownResource {
        token: String,
    },
    DuplicateResource {
        resource: HeldResource,
    },
    OutOfOrder {
        previous: HeldResource,
        resource: HeldResource,
    },
    EarlierResourceRequested {
        requested: HeldResource,
        already_held: HeldResource,
    },
}

impl fmt::Display for HeldResourcesError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptySegment { index } => write!(
                formatter,
                "KIO_CI_SCHEDULE_HELD contains an empty segment at position {}",
                index + 1
            ),
            Self::UnknownResource { token } => write!(
                formatter,
                "KIO_CI_SCHEDULE_HELD contains unknown resource {token:?}"
            ),
            Self::DuplicateResource { resource } => write!(
                formatter,
                "KIO_CI_SCHEDULE_HELD contains duplicate resource {resource}"
            ),
            Self::OutOfOrder { previous, resource } => write!(
                formatter,
                "KIO_CI_SCHEDULE_HELD must follow work,cargo,compiler order ({resource} follows {previous})"
            ),
            Self::EarlierResourceRequested {
                requested,
                already_held,
            } => write!(
                formatter,
                "cannot acquire {requested} after inherited {already_held} admission"
            ),
        }
    }
}

impl std::error::Error for HeldResourcesError {}

#[cfg(test)]
mod tests {
    use super::{HeldResource, HeldResources, HeldResourcesError, ResourceRequest};

    #[test]
    fn held_resources_accept_only_canonical_work_cargo_compiler_order() {
        for (value, encoded) in [
            (None, ""),
            (Some(""), ""),
            (Some("work"), "work"),
            (Some("cargo"), "cargo"),
            (Some("compiler"), "compiler"),
            (Some("work,cargo"), "work,cargo"),
            (Some("work,compiler"), "work,compiler"),
            (Some("cargo,compiler"), "cargo,compiler"),
            (Some("work,cargo,compiler"), "work,cargo,compiler"),
        ] {
            assert_eq!(HeldResources::parse(value).unwrap().encode(), encoded);
        }

        for value in [
            "cargo,work",
            "compiler,work",
            "compiler,cargo",
            "work,compiler,cargo",
        ] {
            assert!(matches!(
                HeldResources::parse(Some(value)),
                Err(HeldResourcesError::OutOfOrder { .. })
            ));
        }
    }

    #[test]
    fn malformed_held_resource_lists_are_rejected() {
        for value in [",work", "work,", "work,,compiler"] {
            assert!(matches!(
                HeldResources::parse(Some(value)),
                Err(HeldResourcesError::EmptySegment { .. })
            ));
        }
        for value in ["unknown", " work", "work "] {
            assert!(matches!(
                HeldResources::parse(Some(value)),
                Err(HeldResourcesError::UnknownResource { .. })
            ));
        }
        for value in ["work,work", "cargo,cargo", "compiler,compiler"] {
            assert!(matches!(
                HeldResources::parse(Some(value)),
                Err(HeldResourcesError::DuplicateResource { .. })
            ));
        }
    }

    #[test]
    fn already_held_resource_is_reused() {
        let held = HeldResources::parse(Some("work,compiler")).unwrap();
        assert_eq!(
            held.request(HeldResource::Work).unwrap(),
            ResourceRequest::Reuse
        );
        assert_eq!(
            held.request(HeldResource::Compiler).unwrap(),
            ResourceRequest::Reuse
        );
        assert_eq!(held.with_requested(HeldResource::Work).unwrap(), held);
    }

    #[test]
    fn requesting_earlier_resource_after_later_one_is_rejected() {
        let held = HeldResources::parse(Some("work,compiler")).unwrap();
        assert_eq!(
            held.request(HeldResource::Cargo),
            Err(HeldResourcesError::EarlierResourceRequested {
                requested: HeldResource::Cargo,
                already_held: HeldResource::Compiler,
            })
        );

        let held = HeldResources::parse(Some("cargo")).unwrap();
        assert_eq!(
            held.request(HeldResource::Work),
            Err(HeldResourcesError::EarlierResourceRequested {
                requested: HeldResource::Work,
                already_held: HeldResource::Cargo,
            })
        );
    }

    #[test]
    fn newly_acquired_resources_extend_the_canonical_marker() {
        let held = HeldResources::parse(Some("work")).unwrap();
        let held = held.with_requested(HeldResource::Cargo).unwrap();
        let held = held.with_requested(HeldResource::Compiler).unwrap();
        assert_eq!(held.encode(), "work,cargo,compiler");

        let held = HeldResources::default()
            .with_requested(HeldResource::Compiler)
            .unwrap();
        assert_eq!(held.encode(), "compiler");
    }

    #[test]
    fn independently_validated_resource_sets_union_canonically() {
        let outer = HeldResources::parse(Some("work,compiler")).unwrap();
        let command = HeldResources::parse(Some("cargo,compiler")).unwrap();
        assert_eq!(outer.union(command).encode(), "work,cargo,compiler");
    }
}
