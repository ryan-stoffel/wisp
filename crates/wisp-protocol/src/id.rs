use std::error::Error;
use std::fmt;

use uuid::{Uuid, Variant, Version};

/// A string or UUID that is not an id. Ids are version 7 UUIDs, written in lowercase with
/// hyphens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidId;

impl fmt::Display for InvalidId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("expected a lowercase, hyphenated UUIDv7")
    }
}

impl Error for InvalidId {}

// Only the canonical form parses, so an id always comes back exactly as its creator spelled it.
pub(crate) fn parse_v7(s: &str) -> Result<Uuid, InvalidId> {
    if s.len() != 36 || s.bytes().any(|b| b.is_ascii_uppercase()) {
        return Err(InvalidId);
    }
    check_v7(Uuid::try_parse(s).map_err(|_| InvalidId)?)
}

pub(crate) fn check_v7(uuid: Uuid) -> Result<Uuid, InvalidId> {
    if uuid.get_version() == Some(Version::SortRand) && uuid.get_variant() == Variant::RFC4122 {
        Ok(uuid)
    } else {
        Err(InvalidId)
    }
}

macro_rules! uuid_v7_id {
    ($(#[doc = $doc:literal])* $name:ident) => {
        $(#[doc = $doc])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, ts_rs::TS)]
        pub struct $name(uuid::Uuid);

        impl $name {
            /// Generates a new id from the current time and random bits.
            #[must_use]
            pub fn generate() -> Self {
                Self(uuid::Uuid::now_v7())
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                std::fmt::Display::fmt(&self.0.hyphenated(), f)
            }
        }

        impl std::str::FromStr for $name {
            type Err = $crate::id::InvalidId;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                $crate::id::parse_v7(s).map(Self)
            }
        }

        impl From<$name> for uuid::Uuid {
            fn from(id: $name) -> Self {
                id.0
            }
        }

        impl TryFrom<uuid::Uuid> for $name {
            type Error = $crate::id::InvalidId;

            /// Fails unless `uuid` is a version 7 UUID.
            fn try_from(uuid: uuid::Uuid) -> Result<Self, Self::Error> {
                $crate::id::check_v7(uuid).map(Self)
            }
        }

        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.collect_str(&self.0.hyphenated())
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                struct Visitor;

                impl serde::de::Visitor<'_> for Visitor {
                    type Value = $name;

                    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                        f.write_str("a lowercase, hyphenated UUIDv7 string")
                    }

                    fn visit_str<E: serde::de::Error>(self, s: &str) -> Result<$name, E> {
                        $crate::id::parse_v7(s).map($name).map_err(E::custom)
                    }
                }

                deserializer.deserialize_str(Visitor)
            }
        }
    };
}

pub(crate) use uuid_v7_id;

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::{InvalidId, parse_v7};
    use crate::{LogId, ProjectId, SubscriptionId};

    const V7: &str = "01997c3a-5b2c-7d4e-9f10-2a3b4c5d6e7f";

    #[test]
    fn canonical_uuid_v7_parses_and_prints_unchanged() {
        let id: ProjectId = V7.parse().unwrap();
        assert_eq!(id.to_string(), V7);
        assert_eq!(serde_json::to_string(&id).unwrap(), format!("\"{V7}\""));
        assert_eq!(
            serde_json::from_str::<ProjectId>(&format!("\"{V7}\"")).unwrap(),
            id
        );
    }

    #[test]
    fn other_spellings_and_versions_are_rejected() {
        for bad in [
            "01997C3A-5B2C-7D4E-9F10-2A3B4C5D6E7F",
            "01997c3a5b2c7d4e9f102a3b4c5d6e7f",
            "{01997c3a-5b2c-7d4e-9f10-2a3b4c5d6e7f}",
            "urn:uuid:01997c3a-5b2c-7d4e-9f10-2a3b4c5d6e7f",
            "3f2b9a1e-8c4d-4e5f-9a6b-7c8d9e0f1a2b",
            "01997c3a-5b2c-7d4e-cf10-2a3b4c5d6e7f",
            "00000000-0000-0000-0000-000000000000",
            "01997c3a-5b2c-7d4e-9f10-2a3b4c5d6e7",
            "",
        ] {
            assert_eq!(parse_v7(bad), Err(InvalidId), "{bad}");
            let json = serde_json::to_string(bad).unwrap();
            assert!(serde_json::from_str::<ProjectId>(&json).is_err(), "{bad}");
        }
        assert!(serde_json::from_str::<ProjectId>("7").is_err());
    }

    #[test]
    fn generated_ids_are_canonical_v7() {
        let id = ProjectId::generate();
        assert_eq!(id.to_string().parse::<ProjectId>(), Ok(id));
        assert_ne!(ProjectId::generate(), id);
    }

    #[test]
    fn ids_convert_to_and_from_version_7_uuids_only() {
        let id: ProjectId = V7.parse().unwrap();
        let uuid = Uuid::from(id);
        assert_eq!(uuid.to_string(), V7);
        assert_eq!(ProjectId::try_from(uuid), Ok(id));
        assert!(LogId::try_from(Uuid::now_v7()).is_ok());
        for other in [
            Uuid::nil(),
            Uuid::max(),
            Uuid::try_parse("3f2b9a1e-8c4d-4e5f-9a6b-7c8d9e0f1a2b").unwrap(),
            Uuid::try_parse("01997c3a-5b2c-7d4e-cf10-2a3b4c5d6e7f").unwrap(),
        ] {
            assert_eq!(SubscriptionId::try_from(other), Err(InvalidId), "{other}");
        }
    }
}
