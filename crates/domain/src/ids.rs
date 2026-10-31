//! Identifier newtypes. Every id is a `UUIDv7`, so ids sort by creation time.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Returned when a string is not a valid id.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid id")]
pub struct ParseIdError;

/// The schema of every id: a canonical (hyphenated, lowercase) `UUIDv7`.
#[cfg(feature = "openapi")]
fn uuid_v7_schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
    use utoipa::openapi::schema::{KnownFormat, ObjectBuilder, SchemaFormat, Type};
    ObjectBuilder::new()
        .schema_type(Type::String)
        .format(Some(SchemaFormat::KnownFormat(KnownFormat::Uuid)))
        .description(Some("UUIDv7"))
        .into()
}

macro_rules! id_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
        #[serde(try_from = "String")]
        pub struct $name(Uuid);

        impl $name {
            /// Generates a fresh id.
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }

            /// Wraps an existing UUID, for example one read from storage.
            #[must_use]
            pub fn from_uuid(uuid: Uuid) -> Self {
                Self(uuid)
            }

            /// The underlying UUID.
            #[must_use]
            pub fn as_uuid(&self) -> Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0.hyphenated(), f)
            }
        }

        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.collect_str(self)
            }
        }

        impl From<$name> for crate::audit::AuditValue {
            fn from(id: $name) -> Self {
                Self::Text(id.to_string())
            }
        }

        #[cfg(feature = "openapi")]
        impl utoipa::PartialSchema for $name {
            fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
                crate::ids::uuid_v7_schema()
            }
        }

        #[cfg(feature = "openapi")]
        impl utoipa::ToSchema for $name {
            fn name() -> std::borrow::Cow<'static, str> {
                std::borrow::Cow::Borrowed(stringify!($name))
            }
        }

        impl TryFrom<String> for $name {
            type Error = ParseIdError;

            fn try_from(s: String) -> Result<Self, Self::Error> {
                s.parse()
            }
        }

        impl FromStr for $name {
            type Err = ParseIdError;

            /// Accepts only the canonical form: hyphenated lowercase.
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                let uuid = Uuid::parse_str(s).map_err(|_| ParseIdError)?;
                if uuid.hyphenated().to_string() == s {
                    Ok(Self(uuid))
                } else {
                    Err(ParseIdError)
                }
            }
        }
    };
}

id_type!(
    /// Identifies a household member.
    MemberId
);
id_type!(
    /// Identifies a registered passkey.
    PasskeyId
);
id_type!(
    /// Identifies an invite, re-enroll or bootstrap link.
    InviteId
);
id_type!(
    /// Identifies a login session.
    SessionId
);
id_type!(
    /// Identifies an agent token.
    AgentTokenId
);
id_type!(
    /// Identifies an audit log entry.
    AuditEventId
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_roundtrip_display_fromstr() {
        let id = MemberId::new();
        assert_eq!(id.to_string().parse::<MemberId>(), Ok(id));
    }

    #[test]
    fn ids_are_time_ordered() {
        let a = MemberId::new();
        let b = MemberId::new();
        assert!(a.as_uuid() < b.as_uuid());
    }

    #[test]
    fn ids_reject_garbage() {
        assert!("nope".parse::<MemberId>().is_err());
    }

    #[test]
    fn ids_reject_noncanonical_forms() {
        let id = MemberId::new();
        assert!(id.to_string().to_uppercase().parse::<MemberId>().is_err());
        assert!(
            id.as_uuid()
                .simple()
                .to_string()
                .parse::<MemberId>()
                .is_err()
        );
        let upper = format!("\"{}\"", id.to_string().to_uppercase());
        assert!(serde_json::from_str::<MemberId>(&upper).is_err());
    }

    #[test]
    fn id_serde_is_plain_string() {
        let id = SessionId::new();
        assert_eq!(serde_json::to_string(&id).unwrap(), format!("\"{id}\""));
    }
}
