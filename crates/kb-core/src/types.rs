//! Domain types that don't fit elsewhere — `KbName` newtype, SSE envelope,
//! lightweight enums.

use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, OnceLock};

/// Validated kb name. Must match `[a-z0-9_-]+` (1-64 chars).
/// Subdomain-safe and filesystem-safe.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct KbName(String);

impl KbName {
    pub fn new(s: impl Into<String>) -> Result<Self> {
        let s = s.into();
        if s.is_empty() || s.len() > 64 {
            return Err(Error::BadRequest(format!(
                "kb name length must be 1-64, got {}",
                s.len()
            )));
        }
        if !s
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
        {
            return Err(Error::BadRequest(format!(
                "kb name must match [a-z0-9_-]+, got {s:?}"
            )));
        }
        Ok(Self(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for KbName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for KbName {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        KbName::new(s).map_err(serde::de::Error::custom)
    }
}

/// SSE event envelope. Topic 04 / topic 11 canonical form.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    /// Schema version. Always `1` for v0.0.1.
    pub v: u8,

    /// Monotonic id assigned by `EventBus::publish`.
    pub id: u64,

    /// Event type (e.g. `index.start`, `watch.create`).
    #[serde(rename = "type")]
    pub type_: String,

    /// RFC 3339 / ISO 8601 timestamp.
    pub ts: chrono::DateTime<chrono::Utc>,

    /// Type-specific payload.
    pub payload: serde_json::Value,

    /// Lazily-memoized JSON serialization of `payload`. `Clone` shares the
    /// `Arc`, so the replay-ring copy and every broadcast subscriber's copy
    /// hold the same `OnceLock` — with M open SSE streams each event's
    /// payload is serialized ONCE (on first use), not M times. Never on the
    /// wire (`serde(skip)`): the canonical `{v, id, type, ts, payload}`
    /// shape is unchanged. Private so struct literals can't forget it —
    /// construct via [`Envelope::new`].
    #[serde(skip)]
    payload_json: Arc<OnceLock<Arc<str>>>,
}

impl Envelope {
    pub fn new(type_: impl Into<String>, payload: serde_json::Value) -> Self {
        Self {
            v: 1,
            id: 0, // set by EventBus::publish
            type_: type_.into(),
            ts: chrono::Utc::now(),
            payload,
            payload_json: Arc::new(OnceLock::new()),
        }
    }

    /// `serde_json::to_string(&self.payload)`, memoized on first use and
    /// shared across clones (see the field doc). Byte-identical to
    /// serializing `payload` directly.
    pub fn payload_json(&self) -> Arc<str> {
        self.payload_json
            .get_or_init(|| {
                // Serializing a `serde_json::Value` to a String cannot fail
                // in practice (no IO, keys are strings, no NaN); fall back
                // defensively rather than panicking on the event path.
                serde_json::to_string(&self.payload)
                    .unwrap_or_else(|_| "null".to_string())
                    .into()
            })
            .clone()
    }
}

/// What kind of change an indexed artifact represents in a SSE event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Created,
    Modified,
    Removed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kb_name_accepts_valid() {
        assert!(KbName::new("work").is_ok());
        assert!(KbName::new("work-laptop").is_ok());
        assert!(KbName::new("kb_2026").is_ok());
        assert!(KbName::new("a").is_ok());
    }

    #[test]
    fn kb_name_rejects_invalid() {
        assert!(KbName::new("").is_err());
        assert!(KbName::new("Work").is_err()); // uppercase
        assert!(KbName::new("work space").is_err()); // space
        assert!(KbName::new("work/path").is_err()); // slash
        assert!(KbName::new("work.dot").is_err()); // dot
        assert!(KbName::new("a".repeat(65)).is_err()); // too long
    }

    #[test]
    fn envelope_serialises_with_renamed_type() {
        let env = Envelope::new("index.start", serde_json::json!({"run": "r-abc"}));
        let s = serde_json::to_string(&env).unwrap();
        assert!(s.contains(r#""type":"index.start""#));
        assert!(s.contains(r#""v":1"#));
        // The payload_json memo is serde(skip) — never on the wire.
        assert!(!s.contains("payload_json"));
    }

    #[test]
    fn payload_json_is_memoized_and_shared_across_clones() {
        let env = Envelope::new("index.start", serde_json::json!({"run": "r-abc"}));
        // Clone BEFORE first use — the ring copy vs a broadcast copy.
        let copy = env.clone();
        let a = env.payload_json();
        let b = copy.payload_json();
        assert_eq!(&*a, serde_json::to_string(&env.payload).unwrap());
        assert!(
            Arc::ptr_eq(&a, &b),
            "one serialization, shared by every clone"
        );
    }

    proptest::proptest! {
        #[test]
        fn kb_name_roundtrip_for_valid(s in "[a-z0-9_-]{1,64}") {
            let kb = KbName::new(&s).expect("valid by regex");
            let json = serde_json::to_string(&kb).unwrap();
            assert_eq!(json, format!(r#""{s}""#));
            let back: KbName = serde_json::from_str(&json).unwrap();
            assert_eq!(back, kb);
        }
    }
}
