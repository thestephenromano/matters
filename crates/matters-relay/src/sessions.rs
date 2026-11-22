//! Device sessions (docs/SYNC_SERVICE_CONTRACT.md "Device sessions"): a
//! device answers a single-use challenge with its enrolled ML-DSA-65 key
//! and gets a short-lived bearer token scoped to one route and device.
//! Membership is checked again on every authenticated request, so a
//! device's sessions end with the transition that removes it.

use matters_sync::authority::{self, Hash, Route};
use matters_sync::record::record_hash;
use matters_sync::wire::SIGNATURE_BYTES;

use crate::store::{route_row, Relay, RelayError};

/// How long a challenge may be answered.
pub const CHALLENGE_MS: u64 = 60_000;
/// How long a session token lasts.
pub const SESSION_MS: u64 = 15 * 60_000;

type Result<T> = std::result::Result<T, RelayError>;

/// An open session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub token: [u8; 32],
    pub expires: u64,
    pub generation: [u8; 16],
}

/// 256 bits from the operating system's random source, for challenges and
/// tokens. (Two UUIDv4s carry only 244.)
fn random32() -> Result<[u8; 32]> {
    let mut out = [0; 32];
    getrandom::fill(&mut out).map_err(|_| RelayError::Entropy)?;
    Ok(out)
}

fn ms(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

impl Relay {
    /// A single-use challenge for `device` on `route`, if it is a current
    /// member. Expired challenges and sessions of the route are dropped.
    pub async fn challenge(
        &self,
        route: &Route,
        device: &Hash,
        now: u64,
    ) -> Result<([u8; 32], u64)> {
        let mut tx = self.pool().begin().await?;
        let row = route_row(&mut tx, route, false).await?;
        let replayed = self.replayed(&mut tx, route, &row).await?;
        let authority = &replayed.authority;
        if !crate::store::member(authority, device) && *device != authority.root() {
            return Err(RelayError::NotAMember);
        }
        sqlx::query("DELETE FROM challenges WHERE route = $1 AND expires_at < $2")
            .bind(row.id)
            .bind(ms(now))
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM sessions WHERE route = $1 AND expires_at < $2")
            .bind(row.id)
            .bind(ms(now))
            .execute(&mut *tx)
            .await?;
        let challenge = random32()?;
        let expires = now + CHALLENGE_MS;
        sqlx::query(
            "INSERT INTO challenges (challenge, route, device, expires_at) VALUES ($1, $2, $3, $4)",
        )
        .bind(challenge.as_slice())
        .bind(row.id)
        .bind(device.as_slice())
        .bind(ms(expires))
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok((challenge, expires))
    }

    /// Open a session: the challenge must be this route's and device's,
    /// unused and unexpired (it is used up either way), the device a
    /// current member, and the signature its enrolled key's.
    pub async fn open_session(
        &self,
        route: &Route,
        device: &Hash,
        challenge: &[u8; 32],
        signature: &[u8; SIGNATURE_BYTES],
        now: u64,
    ) -> Result<Session> {
        let mut tx = self.pool().begin().await?;
        let row = route_row(&mut tx, route, false).await?;
        let used: Option<i64> = sqlx::query_scalar(
            "DELETE FROM challenges WHERE challenge = $1 AND route = $2 AND device = $3
             RETURNING expires_at",
        )
        .bind(challenge.as_slice())
        .bind(row.id)
        .bind(device.as_slice())
        .fetch_optional(&mut *tx)
        .await?;
        // Using up the challenge commits even when the answer is refused.
        let fresh = used.is_some_and(|expires| expires >= ms(now));
        let replayed = self.replayed(&mut tx, route, &row).await?;
        let authority = &replayed.authority;
        // A member's enrolled key, or for the root's ID the root key: a
        // recovering device's session (#42).
        let key = authority
            .members()
            .ok()
            .and_then(|m| m.into_iter().find(|m| m.id == *device))
            .map(|m| m.signing_key)
            .or_else(|| (*device == authority.root()).then(|| authority.root_key().clone()));
        let verified = match (&key, fresh) {
            (Some(key), true) => {
                authority::verify_session(key, route, device, challenge, signature).is_ok()
            }
            _ => false,
        };
        if !verified {
            tx.commit().await?;
            return Err(if key.is_none() {
                RelayError::NotAMember
            } else {
                RelayError::Unauthenticated
            });
        }
        let token = random32()?;
        let expires = now + SESSION_MS;
        sqlx::query(
            "INSERT INTO sessions (token_hash, route, device, expires_at) VALUES ($1, $2, $3, $4)",
        )
        .bind(record_hash(&token).as_slice())
        .bind(row.id)
        .bind(device.as_slice())
        .bind(ms(expires))
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Session {
            token,
            expires,
            generation: row.generation,
        })
    }

    /// The device a bearer `token` authenticates on `route`: unexpired,
    /// for this route, and still a member.
    pub async fn authenticate(&self, route: &Route, token: &[u8], now: u64) -> Result<Hash> {
        self.authenticate_as(route, token, now, false).await
    }

    /// As [`Self::authenticate`], also accepting the route's root, whose
    /// sessions only read the logs and publish a root recovery and envelopes.
    pub async fn authenticate_as(
        &self,
        route: &Route,
        token: &[u8],
        now: u64,
        root_allowed: bool,
    ) -> Result<Hash> {
        Ok(self.session_of(route, token, now, root_allowed).await?.0)
    }

    /// As [`Self::authenticate_as`], with the session's expiry (Unix ms).
    pub async fn session_of(
        &self,
        route: &Route,
        token: &[u8],
        now: u64,
        root_allowed: bool,
    ) -> Result<(Hash, u64)> {
        let mut tx = self.pool().begin().await?;
        let row = route_row(&mut tx, route, false).await?;
        let session = sqlx::query_as::<_, (i64, Vec<u8>, i64)>(
            "SELECT route, device, expires_at FROM sessions WHERE token_hash = $1",
        )
        .bind(record_hash(token).as_slice())
        .fetch_optional(&mut *tx)
        .await?;
        let Some((session_route, device, expires)) = session else {
            return Err(RelayError::Unauthenticated);
        };
        if session_route != row.id {
            return Err(RelayError::Unauthenticated);
        }
        if expires < ms(now) {
            return Err(RelayError::SessionExpired);
        }
        let device: Hash = device.try_into().map_err(|_| RelayError::Malformed)?;
        let replayed = self.replayed(&mut tx, route, &row).await?;
        let authority = &replayed.authority;
        let root = device == authority.root();
        if root && !root_allowed {
            return Err(RelayError::NotAMember);
        }
        if !root && !crate::store::member(authority, &device) {
            return Err(RelayError::Revoked);
        }
        Ok((device, u64::try_from(expires).unwrap_or(0)))
    }
}
