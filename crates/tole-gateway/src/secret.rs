//! A string that never prints itself.
//!
//! Bot tokens, serve tokens, passwords and signing secrets are wrapped in
//! [`Secret`] as soon as they are read, so a stray `{:?}`, a log line or an
//! error message cannot leak them. There is deliberately no `Serialize`.

use std::fmt;

use serde::{Deserialize, Deserializer};

/// A secret value. `Debug` and `Display` print `***`; use [`Secret::expose`]
/// at the single place where the value is sent on the wire.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// Wrap a value.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The raw value. Call only where it is sent to the service it belongs to.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// True when the wrapped value is empty (or only whitespace).
    pub fn is_blank(&self) -> bool {
        self.0.trim().is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}

impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Secret)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_prints_the_value() {
        let s = Secret::new("123:abc-very-secret");
        assert_eq!(format!("{s:?}"), "Secret(***)");
        assert_eq!(format!("{s}"), "***");
        assert!(!format!("{s:#?}").contains("very-secret"));
        assert_eq!(s.expose(), "123:abc-very-secret");
    }

    #[test]
    fn blank_detection() {
        assert!(Secret::new("  ").is_blank());
        assert!(!Secret::new("x").is_blank());
    }

    #[test]
    fn debug_of_a_struct_holding_a_secret_is_redacted() {
        #[derive(Debug)]
        struct Holder {
            _token: Secret,
        }
        let h = Holder {
            _token: Secret::new("topsecret"),
        };
        assert!(!format!("{h:?}").contains("topsecret"));
    }
}
