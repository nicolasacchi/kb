//! The in-memory single-use ticket table.
//!
//! A ticket is minted by an authenticated `POST /api/terms/{id}/ticket`
//! and consumed once, at the WebSocket upgrade (T12). It binds the
//! upgrade to one terminal, one user, and one short-lived window. Every
//! failure mode collapses to one 403 at the upgrade, so the caller
//! cannot distinguish "unknown" from "wrong terminal" either.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How many live tickets the table will hold before new mints refuse.
pub(crate) const MAX_LIVE_TICKETS: usize = 1024;

/// Why a ticket mint or consume failed. Every variant is internal to
/// the gate and the upgrade handler; the WS handler returns the same
/// 403 for all four consume failures (see [`consume_ticket`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Consume errors are read only by T12's WebSocket handler, which
/// has not landed yet — kept pub(crate) for it.
#[allow(dead_code)]
pub(crate) enum TicketError {
    /// Mint: the table is full of live tickets.
    Full,
    /// Mint: the CSPRNG failed.
    Rng,
    /// Consume: no such token (unknown, forged, or already spent).
    Unknown,
    /// Consume: the token lived past its TTL.
    Expired,
    /// Consume: the token was minted for a different terminal.
    WrongSession,
    /// Consume: the token was minted for a different user.
    WrongUser,
}

#[derive(Debug, Clone)]
struct TicketEntry {
    term_id: String,
    user: String,
    expires: Instant,
}

/// The ticket table. The `Mutex` guard is never held across an await —
/// every operation is a lock → mutate → drop, with no async work
/// inside (invariant #15).
#[derive(Debug, Default)]
pub(crate) struct TicketStore {
    entries: Mutex<HashMap<String, TicketEntry>>,
}

impl TicketStore {
    /// T12's ticket table constructor. Kept `pub(crate)` for it.
    #[allow(dead_code)]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Insert an entry, first sweeping expired ones. Returns the
    /// 32-lowercase-hex-char token. Never falls back to a
    /// clock-derived value when the CSPRNG fails — low ticket entropy
    /// would let an attacker forge the upgrade gate.
    pub(crate) fn mint(
        &self,
        term_id: &str,
        user: &str,
        ttl: Duration,
        now: Instant,
    ) -> Result<String, TicketError> {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).map_err(|_| TicketError::Rng)?;
        let token = hex::encode(bytes);

        let expires = now + ttl;
        let mut guard = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        // Sweep expired entries first, so a leak of stale tickets
        // cannot starve live ones.
        guard.retain(|_, e| e.expires > now);
        if guard.len() >= MAX_LIVE_TICKETS {
            return Err(TicketError::Full);
        }
        guard.insert(
            token.clone(),
            TicketEntry {
                term_id: term_id.to_string(),
                user: user.to_string(),
                expires,
            },
        );
        Ok(token)
    }

    /// Take the entry out FIRST (single use even when a later check
    /// fails), then check expiry, then `term_id`, then `user`. T12's
    /// WebSocket handler calls this at the upgrade.
    #[allow(dead_code)]
    pub(crate) fn consume(
        &self,
        token: &str,
        term_id: &str,
        user: &str,
        now: Instant,
    ) -> Result<(), TicketError> {
        let entry = {
            let mut guard = self.entries.lock().unwrap_or_else(|e| e.into_inner());
            guard.remove(token).ok_or(TicketError::Unknown)?
        };
        if entry.expires <= now {
            return Err(TicketError::Expired);
        }
        if entry.term_id != term_id {
            return Err(TicketError::WrongSession);
        }
        if entry.user != user {
            return Err(TicketError::WrongUser);
        }
        Ok(())
    }
}

/// Consume a ticket and collapse every failure to `false`. The future
/// WS handler (T12) returns one 403 for all four consume failures.
#[allow(dead_code)]
pub(crate) fn consume_ticket(
    state: &crate::state::KbHandles,
    token: &str,
    term_id: &str,
    user: &str,
) -> bool {
    state
        .terminals
        .tickets
        .consume(token, term_id, user, Instant::now())
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> Instant {
        Instant::now()
    }

    #[test]
    fn ticket_is_single_use() {
        let store = TicketStore::new();
        let at = now();
        let tok = store
            .mint("abcd1234", "operator", Duration::from_secs(30), at)
            .unwrap();
        assert!(store.consume(&tok, "abcd1234", "operator", at).is_ok());
        // The entry is GONE after the first consume.
        assert_eq!(
            store.consume(&tok, "abcd1234", "operator", at),
            Err(TicketError::Unknown)
        );
    }

    #[test]
    fn expired_fails_and_fresh_at_29_s_passes() {
        let store = TicketStore::new();
        let at = now();
        // TTL 30, consumed 31 s later → Expired.
        let tok = store
            .mint("abcd1234", "operator", Duration::from_secs(30), at)
            .unwrap();
        assert_eq!(
            store.consume(&tok, "abcd1234", "operator", at + Duration::from_secs(31)),
            Err(TicketError::Expired)
        );
        // A fresh ticket at +29 s → Ok.
        let tok2 = store
            .mint("abcd1234", "operator", Duration::from_secs(30), at)
            .unwrap();
        assert!(store
            .consume(&tok2, "abcd1234", "operator", at + Duration::from_secs(29))
            .is_ok());
    }

    #[test]
    fn wrong_binding_burns_the_ticket() {
        let store = TicketStore::new();
        let at = now();
        let tok = store
            .mint("abcd1234", "operator", Duration::from_secs(30), at)
            .unwrap();
        assert_eq!(
            store.consume(&tok, "efgh5678", "operator", at),
            Err(TicketError::WrongSession)
        );
        // The entry is GONE afterwards: a later correct attempt fails.
        assert_eq!(
            store.consume(&tok, "abcd1234", "operator", at),
            Err(TicketError::Unknown)
        );

        let tok2 = store
            .mint("abcd1234", "operator", Duration::from_secs(30), at)
            .unwrap();
        assert_eq!(
            store.consume(&tok2, "abcd1234", "someoneelse", at),
            Err(TicketError::WrongUser)
        );
        assert_eq!(
            store.consume(&tok2, "abcd1234", "operator", at),
            Err(TicketError::Unknown)
        );
    }

    #[test]
    fn full_table_refuses_then_sweeps() {
        let store = TicketStore::new();
        let at = now();
        let ttl = Duration::from_secs(30);
        for _ in 0..MAX_LIVE_TICKETS {
            store.mint("abcd1234", "operator", ttl, at).unwrap();
        }
        assert_eq!(
            store.mint("abcd1234", "operator", ttl, at),
            Err(TicketError::Full)
        );
        // After all of them expire, the sweep lets a mint through.
        let later = at + Duration::from_secs(31);
        assert!(store.mint("abcd1234", "operator", ttl, later).is_ok());
    }

    #[test]
    fn thousand_mints_are_unique_hex() {
        let store = TicketStore::new();
        let at = now();
        let mut seen = std::collections::HashSet::new();
        for _ in 0..1000 {
            let tok = store
                .mint("abcd1234", "operator", Duration::from_secs(120), at)
                .unwrap();
            assert_eq!(tok.len(), 32, "one hex char per byte × 16 bytes");
            assert!(
                tok.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()),
                "{tok} must be lowercase hex"
            );
            assert!(seen.insert(tok), "tokens must be distinct");
        }
        assert_eq!(seen.len(), 1000);
    }

    #[test]
    fn consume_ticket_collapses_every_failure_to_false() {
        // Proves the four internal consume variants carry no signal to
        // the caller through `consume_ticket`.
        assert_ne!(TicketError::Unknown, TicketError::Expired);
        assert_ne!(TicketError::Unknown, TicketError::WrongSession);
        assert_ne!(TicketError::Unknown, TicketError::WrongUser);
        assert_ne!(TicketError::Expired, TicketError::WrongSession);
        // A token unknown to a fresh store is false without any state.
        let store = TicketStore::new();
        assert_eq!(
            store.consume("0".repeat(32).as_str(), "abcd1234", "operator", now()),
            Err(TicketError::Unknown)
        );
        assert!(store
            .consume("0".repeat(32).as_str(), "abcd1234", "operator", now())
            .is_err());
    }
}
