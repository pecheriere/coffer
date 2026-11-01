//! Identity types: roles, scopes, validated names and the acting principal.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::ids::{AgentTokenId, MemberId};

/// Why an identity value was rejected. Messages never echo the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum IdentityError {
    #[error("username must be 1 to 32 characters of a-z, 0-9, '_' or '-'")]
    InvalidUsername,
    #[error("display name must be 1 to 64 characters without control characters")]
    InvalidDisplayName,
    #[error("token name must be 1 to 64 characters without control characters")]
    InvalidTokenName,
    #[error("unknown role")]
    UnknownRole,
    #[error("unknown scope")]
    UnknownScope,
    #[error("theme name must be 1 to 32 characters of a-z, 0-9 or '-'")]
    InvalidThemeName,
    #[error("unknown colour scheme")]
    UnknownColorScheme,
    #[error("unknown theme")]
    UnknownTheme,
}

/// A household member's role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Admin,
    Member,
}

impl Role {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Admin => "admin",
            Role::Member => "member",
        }
    }
}

impl FromStr for Role {
    type Err = IdentityError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "admin" => Ok(Role::Admin),
            "member" => Ok(Role::Member),
            _ => Err(IdentityError::UnknownRole),
        }
    }
}

/// The rights an agent token or role carries. Ordered `Read < Write < Admin`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Read,
    Write,
    Admin,
}

impl Scope {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Read => "read",
            Scope::Write => "write",
            Scope::Admin => "admin",
        }
    }
}

impl FromStr for Scope {
    type Err = IdentityError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "read" => Ok(Scope::Read),
            "write" => Ok(Scope::Write),
            "admin" => Ok(Scope::Admin),
            _ => Err(IdentityError::UnknownScope),
        }
    }
}

/// The highest scope a role may exercise.
#[must_use]
pub fn role_ceiling(role: Role) -> Scope {
    match role {
        Role::Admin => Scope::Admin,
        Role::Member => Scope::Write,
    }
}

/// A login name: lowercase `[a-z0-9_-]`, 1 to 32 characters.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Username(String);

impl Username {
    /// Lowercases and validates `input`.
    ///
    /// # Errors
    /// Returns [`IdentityError::InvalidUsername`] when `input` is empty, too
    /// long or contains characters outside `[a-zA-Z0-9_-]`.
    pub fn parse(input: &str) -> Result<Self, IdentityError> {
        let name = input.to_ascii_lowercase();
        let valid = (1..=32).contains(&name.len())
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-');
        if valid {
            Ok(Self(name))
        } else {
            Err(IdentityError::InvalidUsername)
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Username {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The name of a visual theme: `[a-z0-9-]`, 1 to 32 characters. Whether a
/// theme of that name exists is the server's concern.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ThemeName(String);

impl ThemeName {
    /// Validates `input` without changing it.
    ///
    /// # Errors
    /// Returns [`IdentityError::InvalidThemeName`] when `input` is empty, too
    /// long or contains characters outside `[a-z0-9-]`.
    pub fn parse(input: &str) -> Result<Self, IdentityError> {
        let valid = (1..=32).contains(&input.len())
            && input
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        if valid {
            Ok(Self(input.to_owned()))
        } else {
            Err(IdentityError::InvalidThemeName)
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ThemeName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Light or dark rendering: follow the device, or force one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ColorScheme {
    #[default]
    System,
    Light,
    Dark,
}

impl ColorScheme {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ColorScheme::System => "system",
            ColorScheme::Light => "light",
            ColorScheme::Dark => "dark",
        }
    }
}

impl FromStr for ColorScheme {
    type Err = IdentityError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "system" => Ok(ColorScheme::System),
            "light" => Ok(ColorScheme::Light),
            "dark" => Ok(ColorScheme::Dark),
            _ => Err(IdentityError::UnknownColorScheme),
        }
    }
}

/// Trims `input` and accepts it when it has 1 to `max` characters and no
/// control characters.
pub(crate) fn parse_label(input: &str, max: usize) -> Option<String> {
    let trimmed = input.trim();
    let valid =
        (1..=max).contains(&trimmed.chars().count()) && !trimmed.chars().any(char::is_control);
    valid.then(|| trimmed.to_owned())
}

/// Defines a human-readable label: trimmed, 1 to `max` characters, no
/// control characters. Rejection is the given error variant.
macro_rules! label_type {
    ($(#[$meta:meta])* $name:ident, $error:ident::$variant:ident, max = $max:literal) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        pub struct $name(String);

        impl $name {
            /// The most characters it may have.
            pub const MAX_CHARS: usize = $max;

            #[doc = concat!("Trims and validates `input`: 1 to ", stringify!($max), " characters, no control characters.")]
            ///
            /// # Errors
            #[doc = concat!("Returns [`", stringify!($error), "::", stringify!($variant), "`] when `input` is invalid.")]
            pub fn parse(input: &str) -> Result<Self, $error> {
                $crate::identity::parse_label(input, Self::MAX_CHARS)
                    .map(Self)
                    .ok_or($error::$variant)
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        #[cfg(feature = "openapi")]
        impl utoipa::PartialSchema for $name {
            fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
                use utoipa::openapi::schema::{ObjectBuilder, Type};
                ObjectBuilder::new()
                    .schema_type(Type::String)
                    .min_length(Some(1))
                    .max_length(Some($max))
                    .description(Some(concat!(
                        "1 to ",
                        stringify!($max),
                        " characters without control characters; surrounding whitespace is trimmed."
                    )))
                    .into()
            }
        }

        #[cfg(feature = "openapi")]
        impl utoipa::ToSchema for $name {
            fn name() -> std::borrow::Cow<'static, str> {
                std::borrow::Cow::Borrowed(stringify!($name))
            }
        }
    };
}
pub(crate) use label_type;

label_type!(
    /// A member's human-readable name.
    DisplayName,
    IdentityError::InvalidDisplayName,
    max = 64
);
label_type!(
    /// A human-readable name for an agent token.
    TokenName,
    IdentityError::InvalidTokenName,
    max = 64
);

/// How an actor authenticated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    Session,
    Agent { token: AgentTokenId, scope: Scope },
}

/// The authenticated principal performing an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Actor {
    pub member: MemberId,
    pub role: Role,
    pub via: Via,
}

impl Actor {
    /// The scope this actor may exercise: the role ceiling, capped by the
    /// token scope for agents.
    #[must_use]
    pub fn effective_scope(&self) -> Scope {
        let ceiling = role_ceiling(self.role);
        match self.via {
            Via::Session => ceiling,
            Via::Agent { scope, .. } => ceiling.min(scope),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn username_normalizes_and_validates() {
        assert_eq!(Username::parse("Alice").unwrap().as_str(), "alice");
        for bad in ["", &"a".repeat(33), "a b", "é"] {
            assert_eq!(
                Username::parse(bad),
                Err(IdentityError::InvalidUsername),
                "{bad:?}"
            );
        }
        assert_eq!(Username::parse(&"a".repeat(32)).unwrap().as_str().len(), 32);
    }

    #[test]
    fn display_name_rejects_control_chars() {
        assert_eq!(
            DisplayName::parse("a\u{0007}"),
            Err(IdentityError::InvalidDisplayName)
        );
        assert_eq!(DisplayName::parse("  Alice  ").unwrap().as_str(), "Alice");
        for bad in ["   ", &"x".repeat(65)] {
            assert_eq!(
                DisplayName::parse(bad),
                Err(IdentityError::InvalidDisplayName)
            );
        }
    }

    #[test]
    fn scopes_are_ordered_and_parse_lowercase() {
        assert!(Scope::Read < Scope::Write && Scope::Write < Scope::Admin);
        assert_eq!("write".parse::<Scope>(), Ok(Scope::Write));
        assert_eq!("Write".parse::<Scope>(), Err(IdentityError::UnknownScope));
        assert_eq!(serde_json::to_string(&Role::Admin).unwrap(), "\"admin\"");
    }

    proptest::proptest! {
        #[test]
        fn username_parse_is_idempotent_and_bounded(input in "\\PC{0,40}") {
            if let Ok(name) = Username::parse(&input) {
                proptest::prop_assert_eq!(Username::parse(name.as_str()), Ok(name.clone()));
                proptest::prop_assert!((1..=32).contains(&name.as_str().len()));
                proptest::prop_assert_eq!(name.as_str(), input.to_ascii_lowercase());
            }
        }

        #[test]
        fn username_accepts_every_well_formed_name(input in "[a-zA-Z0-9_-]{1,32}") {
            proptest::prop_assert_eq!(
                Username::parse(&input).map(|name| name.as_str().to_owned()),
                Ok(input.to_ascii_lowercase())
            );
        }

        #[test]
        fn username_rejects_more_than_32_characters(input in "[a-z0-9]{33,64}") {
            proptest::prop_assert_eq!(Username::parse(&input), Err(IdentityError::InvalidUsername));
        }

        #[test]
        fn labels_are_idempotent_trimmed_and_bounded(input in "\\PC{0,80}|\\s{0,3}\\PC{0,5}\\s{0,3}") {
            if let Ok(name) = DisplayName::parse(&input) {
                proptest::prop_assert_eq!(DisplayName::parse(name.as_str()), Ok(name.clone()));
                proptest::prop_assert_eq!(name.as_str(), input.trim());
                proptest::prop_assert!((1..=64).contains(&name.as_str().chars().count()));
            }
            // Both label types apply the same rules.
            proptest::prop_assert_eq!(
                TokenName::parse(&input).map(|name| name.as_str().to_owned()),
                DisplayName::parse(&input)
                    .map(|name| name.as_str().to_owned())
                    .map_err(|_| IdentityError::InvalidTokenName)
            );
        }

        #[test]
        fn labels_reject_control_characters(prefix in "[a-z]{1,10}", control in "\\p{Cc}", suffix in "[a-z]{1,10}") {
            let input = format!("{prefix}{control}{suffix}");
            proptest::prop_assert_eq!(DisplayName::parse(&input), Err(IdentityError::InvalidDisplayName));
        }

        #[test]
        fn labels_reject_more_than_64_characters(input in "[a-zé]{65,100}") {
            proptest::prop_assert_eq!(TokenName::parse(&input), Err(IdentityError::InvalidTokenName));
        }
    }

    #[test]
    fn theme_name_validates() {
        for good in ["mochi", "a-1"] {
            assert_eq!(ThemeName::parse(good).unwrap().as_str(), good);
        }
        for bad in ["", &"a".repeat(33), "Mochi", "a b"] {
            assert_eq!(
                ThemeName::parse(bad),
                Err(IdentityError::InvalidThemeName),
                "{bad:?}"
            );
        }
        assert_eq!(
            ThemeName::parse(&"a".repeat(32)).unwrap().as_str().len(),
            32
        );
    }

    #[test]
    fn color_scheme_roundtrips() {
        assert_eq!("dark".parse::<ColorScheme>(), Ok(ColorScheme::Dark));
        assert_eq!(ColorScheme::Dark.as_str(), "dark");
        assert_eq!(
            "purple".parse::<ColorScheme>(),
            Err(IdentityError::UnknownColorScheme)
        );
        assert_eq!(ColorScheme::default(), ColorScheme::System);
        assert_eq!(
            serde_json::to_string(&ColorScheme::Light).unwrap(),
            "\"light\""
        );
    }

    #[test]
    fn errors_do_not_echo_input() {
        let msg = Username::parse("secret value").unwrap_err().to_string();
        assert!(!msg.contains("secret"));
    }
}
