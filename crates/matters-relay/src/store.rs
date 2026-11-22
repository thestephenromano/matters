use matters_sync::authority::{self, Authority, Body, Hash, Route};
use matters_sync::control_signature::verify_signature;
use matters_sync::record::record_hash;
use matters_sync::{wire, VerificationKey};
use minicbor::{Decoder, Encoder};
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use sqlx::{Postgres, Row, Transaction};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// The most records or authority objects one page returns.
pub const MAX_PAGE: u32 = 256;
/// The most entry bytes one page returns (records, authority objects or
/// snapshots), beyond its first entry, which is always returned so that a
/// page always makes progress. With records of up to about 1 MiB, a count
/// alone would let one page hold 256 MiB, twice over in memory.
pub const MAX_PAGE_BYTES: i64 = 16 * 1024 * 1024;
/// The largest authority object accepted (the control budget).
const MAX_AUTHORITY_BYTES: usize = authority::MAX_CONTROL_BYTES;

static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// Why the relay refused or could not complete an operation. Each maps to
/// one error code of the contract.
#[derive(Debug, thiserror::Error)]
pub enum RelayError {
    #[error("malformed request or bytes")]
    Malformed,
    #[error("the route is not provisioned")]
    UnknownRoute,
    #[error("the route is already provisioned")]
    RouteExists,
    #[error("the device is not a current member of the route")]
    NotAMember,
    #[error("the device was removed from the route")]
    Revoked,
    #[error("no valid session")]
    Unauthenticated,
    #[error("the session expired")]
    SessionExpired,
    #[error("another record is stored under this ID")]
    RecordConflict { digest: Hash },
    #[error("the record's routing fields do not match")]
    ContextMismatch,
    #[error("the record's version or suite is not supported")]
    UnsupportedSuite,
    #[error("the bytes exceed the size limit")]
    TooLarge,
    #[error("the cursor belongs to another storage generation")]
    GenerationChanged { generation: [u8; 16] },
    #[error("the route's storage quota is exhausted")]
    QuotaExhausted,
    #[error("no registered, unexpired invitation has this lookup")]
    UnknownInvitation,
    #[error("too many requests; retry after {retry_after_ms} ms")]
    RateLimited { retry_after_ms: u64 },
    #[error("the authority object was refused: {0}")]
    Authority(authority::Error),
    #[error("the database is unavailable: {0}")]
    Unavailable(#[from] sqlx::Error),
    #[error("the system's random source failed")]
    Entropy,
}

impl RelayError {
    /// The contract's error code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Malformed | Self::Authority(_) => "malformed",
            Self::UnknownRoute => "unknown-route",
            Self::RouteExists => "route-exists",
            Self::NotAMember => "not-a-member",
            Self::Revoked => "revoked",
            Self::Unauthenticated => "unauthenticated",
            Self::SessionExpired => "session-expired",
            Self::RecordConflict { .. } => "record-conflict",
            Self::ContextMismatch => "context-mismatch",
            Self::UnsupportedSuite => "unsupported-suite",
            Self::TooLarge => "too-large",
            Self::GenerationChanged { .. } => "generation-changed",
            Self::QuotaExhausted => "quota-exhausted",
            Self::UnknownInvitation => "unknown-invitation",
            Self::RateLimited { .. } => "rate-limited",
            Self::Unavailable(_) | Self::Entropy => "unavailable",
        }
    }

    /// Whether the client may retry the same request unchanged.
    pub fn retry(&self) -> bool {
        matches!(
            self,
            Self::Unavailable(_) | Self::Entropy | Self::RateLimited { .. }
        )
    }
}

impl From<wire::Error> for RelayError {
    fn from(error: wire::Error) -> Self {
        match error {
            wire::Error::TooLarge => Self::TooLarge,
            wire::Error::ContextMismatch => Self::ContextMismatch,
            wire::Error::UnsupportedVersion | wire::Error::UnsupportedSuite => {
                Self::UnsupportedSuite
            }
            _ => Self::Malformed,
        }
    }
}

type Result<T> = std::result::Result<T, RelayError>;

/// The durable acknowledgement of a stored record or authority object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    pub sequence: u64,
    pub generation: [u8; 16],
    pub digest: Hash,
}

/// How an append ended: stored now, or found already stored with
/// identical bytes (the original receipt).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Appended {
    Stored(Receipt),
    Existing(Receipt),
}

impl Appended {
    pub fn receipt(&self) -> &Receipt {
        match self {
            Self::Stored(r) | Self::Existing(r) => r,
        }
    }
}

/// A position in a route's record or authority log, opaque to clients:
/// `CBOR([generation, last-sequence])`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub generation: [u8; 16],
    pub last: u64,
}

impl Cursor {
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new(Vec::new());
        // Writing to a Vec cannot fail.
        let _ = e
            .array(2)
            .and_then(|e| e.bytes(&self.generation))
            .and_then(|e| e.u64(self.last));
        e.into_writer()
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut d = Decoder::new(bytes);
        let parse = |d: &mut Decoder<'_>| -> std::result::Result<Self, minicbor::decode::Error> {
            if d.array()? != Some(2) {
                return Err(minicbor::decode::Error::message("cursor"));
            }
            let generation = d
                .bytes()?
                .try_into()
                .map_err(|_| minicbor::decode::Error::message("cursor"))?;
            Ok(Self {
                generation,
                last: d.u64()?,
            })
        };
        let cursor = parse(&mut d).map_err(|_| RelayError::Malformed)?;
        if d.position() != bytes.len() || cursor.encode() != bytes {
            return Err(RelayError::Malformed);
        }
        Ok(cursor)
    }
}

/// One page of a route's log, in sequence order, up to the high-water
/// mark captured when the page was read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    pub generation: [u8; 16],
    pub high_water: u64,
    pub entries: Vec<(u64, Vec<u8>)>,
    pub next: Cursor,
    pub complete: bool,
}

/// Unexpired invitations, and enrollment requests, a route keeps at once.
pub const MAX_ENROLLMENTS: i64 = 16;
/// How long a registered invitation, and an enrollment request, is kept:
/// an invitation is used within minutes.
pub const ENROLLMENT_TTL_SECONDS: u64 = 3600;

/// The route's newest snapshot records, newest first.
#[derive(Debug)]
pub struct Snapshots {
    pub generation: [u8; 16],
    pub entries: Vec<(u64, Vec<u8>)>,
}

/// The relay's store.
#[derive(Clone)]
pub struct Relay {
    pool: PgPool,
    authorities: Arc<Mutex<AuthorityCache>>,
}

/// A provisioned route's row.
pub(crate) struct RouteRow {
    pub(crate) id: i64,
    root_key: Vec<u8>,
    pub(crate) generation: [u8; 16],
    /// The authority log's length and the digest at its end: with the row
    /// and the generation, what names one replay of the log.
    authority_high_water: i64,
    authority_head: Option<Vec<u8>>,
}

/// The route's authority, replayed from its stored log, with what the
/// relay needs to check the objects it stores without admitting them.
pub(crate) struct Replayed {
    pub(crate) authority: Authority,
    /// Every accepted manifest: its epoch, and its admin with the admin's
    /// roster key. [`Authority`] keeps these to itself.
    manifests: HashMap<Hash, Manifest>,
    /// How many `root_recovery` objects are stored for each generation they
    /// would start.
    recoveries: HashMap<u64, usize>,
}

struct Manifest {
    epoch: u64,
    admin: Hash,
    admin_key: VerificationKey,
}

impl Manifest {
    fn of(epoch: u64, admin: Hash, roster: &[authority::Member]) -> Option<Self> {
        let key = roster.iter().find(|m| m.id == admin)?.signing_key.clone();
        Some(Self {
            epoch,
            admin,
            admin_key: key,
        })
    }
}

// Writer claims and key envelopes have no count cap (#45). Every app open
// adds a claim and every transition adds envelopes, so any count would be
// a lifetime cap on legitimate growth. Like records, they count toward
// the route's byte quota, which bounds them.

/// The most membership transitions and root recoveries a route stores
/// (#45). They are exempt from the byte quota, because each is admin- or
/// root-signed and
/// fully verified before it is stored, and a full route must still be able
/// to remove a device. This cap bounds them instead: at most 2,048 of 64 KiB
/// each, 128 MiB per route outside the byte quota. The authority itself has
/// no lifetime limit on transitions. Replaying them is what an authority
/// cache miss costs. A request otherwise reads the route's replayed
/// authority from the cache, keyed by the log's high-water mark and head,
/// so a miss replays at most this many transitions once per change.
pub const MAX_STORED_TRANSITIONS: i64 = 2048;
/// The most `root_recovery` objects stored per generation they start: the
/// accepted one, and one competing recovery kept as fork evidence.
pub const MAX_RECOVERIES_PER_GENERATION: usize = 2;

/// Routes whose replayed authority the relay keeps in memory.
const CACHED_AUTHORITIES: usize = 4096;

/// What names one replay: the route's row, its generation, and the log's
/// length and last digest. Authority rows are append-only, so a key names
/// exactly one stored log. A restore keeps sequences but not the process:
/// the restore procedure restarts the relay, and installs a new
/// generation besides.
type CacheKey = (i64, [u8; 16], i64, Option<Vec<u8>>);

/// The newest replay of each route's authority (#45): a session challenge,
/// a session and every authenticated request would otherwise replay the
/// whole log, an ML-DSA verification per transition, each time. A route
/// keeps only its newest entry, so a new authority object replaces it.
pub(crate) struct AuthorityCache<V = Arc<Replayed>> {
    entries: HashMap<i64, (CacheKey, u64, V)>,
    tick: u64,
}

impl<V> Default for AuthorityCache<V> {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            tick: 0,
        }
    }
}

impl<V: Clone> AuthorityCache<V> {
    fn get(&mut self, key: &CacheKey) -> Option<V> {
        self.tick += 1;
        let tick = self.tick;
        let (stored, used, replayed) = self.entries.get_mut(&key.0)?;
        if stored != key {
            return None;
        }
        *used = tick;
        Some(replayed.clone())
    }

    fn put(&mut self, key: CacheKey, replayed: V) {
        self.tick += 1;
        if self.entries.len() >= CACHED_AUTHORITIES && !self.entries.contains_key(&key.0) {
            // The least recently used route makes room.
            if let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, (_, used, _))| *used)
                .map(|(id, _)| *id)
            {
                self.entries.remove(&oldest);
            }
        }
        self.entries.insert(key.0, (key, self.tick, replayed));
    }
}

pub(crate) fn random16() -> [u8; 16] {
    *uuid::Uuid::new_v4().as_bytes()
}

fn root_key(bytes: &[u8]) -> Result<VerificationKey> {
    let fixed: [u8; matters_sync::record::VERIFICATION_KEY_BYTES] =
        bytes.try_into().map_err(|_| RelayError::Malformed)?;
    Ok(VerificationKey::from_bytes(fixed))
}

pub(crate) fn sequence(value: i64) -> u64 {
    // Sequences are checked non-negative by the schema.
    value.max(0) as u64
}

impl Relay {
    /// Connect to `url` and bring the schema up to date.
    pub async fn connect(url: &str) -> Result<Self> {
        Self::connect_with(url.parse::<PgConnectOptions>()?).await
    }

    /// Connect with `options` and bring the schema up to date.
    pub async fn connect_with(options: PgConnectOptions) -> Result<Self> {
        // A request waits at most 5 s for a connection, then answers
        // `unavailable`, which clients retry with backoff.
        let pool = PgPoolOptions::new()
            .max_connections(16)
            .acquire_timeout(std::time::Duration::from_secs(5))
            .connect_with(options)
            .await?;
        MIGRATOR
            .run(&pool)
            .await
            .map_err(|e| RelayError::Unavailable(e.into()))?;
        Ok(Self {
            pool,
            authorities: Arc::default(),
        })
    }

    /// Whether the relay can serve: its database answers within a second.
    pub async fn ready(&self) -> bool {
        matches!(
            tokio::time::timeout(
                std::time::Duration::from_secs(1),
                sqlx::query_scalar::<_, i32>("SELECT 1").fetch_one(&self.pool),
            )
            .await,
            Ok(Ok(1))
        )
    }

    /// Storage across every route: routes, records stored and record bytes
    /// stored. Totals only, for metrics.
    pub async fn totals(&self) -> Result<(i64, i64, i64)> {
        let row = sqlx::query(
            "SELECT COUNT(*) AS routes, COALESCE(SUM(record_high_water), 0)::BIGINT AS records,
                    COALESCE(SUM(used_bytes), 0)::BIGINT AS bytes FROM routes",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok((row.get("routes"), row.get("records"), row.get("bytes")))
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Provision `route` with its independently obtained root key and its
    /// signed genesis, which must verify against that key. An operator
    /// action, outside the device API.
    pub async fn provision(
        &self,
        route: &Route,
        root: &VerificationKey,
        genesis: &[u8],
    ) -> Result<[u8; 16]> {
        Authority::bootstrap(genesis, route.clone(), root).map_err(RelayError::Authority)?;
        let generation = random16();
        let mut tx = self.pool.begin().await?;
        let id: Option<i64> = sqlx::query_scalar(
            "INSERT INTO routes (application, tenant, collection, root_key, generation)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (application, tenant, collection) DO NOTHING
             RETURNING id",
        )
        .bind(route.application.as_slice())
        .bind(route.tenant.as_slice())
        .bind(route.collection.as_slice())
        .bind(root.as_bytes())
        .bind(generation.as_slice())
        .fetch_optional(&mut *tx)
        .await?;
        let id = id.ok_or(RelayError::RouteExists)?;
        insert_authority(
            &mut tx,
            id,
            &record_hash(genesis),
            "genesis",
            genesis,
            false,
        )
        .await?;
        tx.commit().await?;
        Ok(generation)
    }

    /// Set an operator quota on the bytes `route` stores; `None` lifts it.
    pub async fn set_quota(&self, route: &Route, bytes: Option<u64>) -> Result<()> {
        let changed = sqlx::query(
            "UPDATE routes SET quota_bytes = $4
             WHERE application = $1 AND tenant = $2 AND collection = $3",
        )
        .bind(route.application.as_slice())
        .bind(route.tenant.as_slice())
        .bind(route.collection.as_slice())
        .bind(bytes.map(|b| i64::try_from(b).unwrap_or(i64::MAX)))
        .execute(&self.pool)
        .await?
        .rows_affected();
        if changed == 0 {
            return Err(RelayError::UnknownRoute);
        }
        Ok(())
    }

    /// Set an operator quota on the number of records `route` stores;
    /// `None` lifts it.
    pub async fn set_record_quota(&self, route: &Route, records: Option<u64>) -> Result<()> {
        let changed = sqlx::query(
            "UPDATE routes SET quota_records = $4
             WHERE application = $1 AND tenant = $2 AND collection = $3",
        )
        .bind(route.application.as_slice())
        .bind(route.tenant.as_slice())
        .bind(route.collection.as_slice())
        .bind(records.map(|r| i64::try_from(r).unwrap_or(i64::MAX)))
        .execute(&self.pool)
        .await?
        .rows_affected();
        if changed == 0 {
            return Err(RelayError::UnknownRoute);
        }
        Ok(())
    }

    /// Install a new generation on every route: the last step of restoring
    /// the whole database from a backup.
    pub async fn new_generation_everywhere(&self) -> Result<u64> {
        let routes: Vec<i64> = sqlx::query_scalar("SELECT id FROM routes")
            .fetch_all(&self.pool)
            .await?;
        let mut tx = self.pool.begin().await?;
        for id in &routes {
            sqlx::query("UPDATE routes SET generation = $2 WHERE id = $1")
                .bind(id)
                .bind(random16().as_slice())
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(routes.len() as u64)
    }

    /// The route's database ID, for matching hints.
    pub(crate) async fn route_id(&self, route: &Route) -> Result<i64> {
        let mut tx = self.pool.begin().await?;
        Ok(route_row(&mut tx, route, false).await?.id)
    }

    /// Install a new random storage generation for `route`: the restore
    /// procedure's last step, so every client's old cursor is refused.
    pub async fn new_generation(&self, route: &Route) -> Result<[u8; 16]> {
        let generation = random16();
        let changed = sqlx::query(
            "UPDATE routes SET generation = $4
             WHERE application = $1 AND tenant = $2 AND collection = $3",
        )
        .bind(route.application.as_slice())
        .bind(route.tenant.as_slice())
        .bind(route.collection.as_slice())
        .bind(generation.as_slice())
        .execute(&self.pool)
        .await?
        .rows_affected();
        if changed == 0 {
            return Err(RelayError::UnknownRoute);
        }
        Ok(generation)
    }

    /// Whether `device` is in the roster of the route's newest accepted
    /// membership.
    pub async fn is_member(&self, route: &Route, device: &Hash) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let row = route_row(&mut tx, route, false).await?;
        let replayed = self.replayed(&mut tx, route, &row).await?;
        Ok(member(&replayed.authority, device))
    }

    /// The route's authority as the relay has accepted it.
    pub async fn authority(&self, route: &Route) -> Result<Authority> {
        let mut tx = self.pool.begin().await?;
        let row = route_row(&mut tx, route, false).await?;
        Ok(self.replayed(&mut tx, route, &row).await?.authority.clone())
    }

    /// Append a content record uploaded by `device` (the session's device)
    /// under `record_id` (the request path). Durable when it returns.
    pub async fn append(
        &self,
        route: &Route,
        device: &Hash,
        record_id: &[u8; 32],
        bytes: &[u8],
    ) -> Result<Appended> {
        let parsed =
            wire::parse_for_route(bytes, &route.application, &route.tenant, &route.collection)?;
        if parsed.header.record_id != record_id || parsed.header.author != device {
            return Err(RelayError::ContextMismatch);
        }
        let digest = record_hash(bytes);
        let mut tx = self.pool.begin().await?;
        // Serializes appends to this route until the commit.
        let row = route_row(&mut tx, route, true).await?;
        let existing =
            sqlx::query("SELECT sequence, digest FROM records WHERE route = $1 AND record_id = $2")
                .bind(row.id)
                .bind(record_id.as_slice())
                .fetch_optional(&mut *tx)
                .await?;
        if let Some(existing) = existing {
            let stored: Vec<u8> = existing.get("digest");
            let stored: Hash = stored.try_into().map_err(|_| RelayError::Malformed)?;
            if stored != digest {
                return Err(RelayError::RecordConflict { digest: stored });
            }
            return Ok(Appended::Existing(Receipt {
                sequence: sequence(existing.get("sequence")),
                generation: row.generation,
                digest,
            }));
        }
        let replayed = self.replayed(&mut tx, route, &row).await?;
        if !member(&replayed.authority, device) {
            return Err(RelayError::NotAMember);
        }
        let size = i64::try_from(bytes.len()).map_err(|_| RelayError::TooLarge)?;
        let seq: Option<i64> = sqlx::query_scalar(
            "UPDATE routes SET record_high_water = record_high_water + 1,
                 used_bytes = used_bytes + $2
             WHERE id = $1 AND (quota_bytes IS NULL OR used_bytes + $2 <= quota_bytes)
               AND (quota_records IS NULL OR record_high_water < quota_records)
             RETURNING record_high_water",
        )
        .bind(row.id)
        .bind(size)
        .fetch_optional(&mut *tx)
        .await?;
        let seq = seq.ok_or(RelayError::QuotaExhausted)?;
        sqlx::query(
            "INSERT INTO records (route, sequence, record_id, digest, bytes, snapshot)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(row.id)
        .bind(seq)
        .bind(record_id.as_slice())
        .bind(digest.as_slice())
        .bind(bytes)
        .bind(parsed.header.kind == wire::Kind::Snapshot)
        .execute(&mut *tx)
        .await?;
        // Delivered by PostgreSQL only when this transaction commits, so a
        // hint never precedes its record.
        sqlx::query("SELECT pg_notify($1, $2)")
            .bind(crate::hints::CHANNEL)
            .bind(crate::hints::payload(
                row.id,
                sequence(seq),
                &row.generation,
            ))
            .execute(&mut *tx)
            .await?;
        crate::fault::crash_point("before-commit");
        tx.commit().await?;
        crate::fault::crash_point("after-commit-before-response");
        Ok(Appended::Stored(Receipt {
            sequence: sequence(seq),
            generation: row.generation,
            digest,
        }))
    }

    /// Store an authority object under its SHA-384 `digest`, uploaded with
    /// `session`'s device (`None`: no session). Identical bytes again return
    /// the original receipt. Every check below runs in the transaction that
    /// stores the object, under the route's lock, against the authority as
    /// stored then.
    ///
    /// - `membership` and `root_recovery` must be accepted by the route's
    ///   authority. A second valid successor of a manifest is kept as fork
    ///   evidence, which freezes membership: further transitions are refused.
    ///   At most [`MAX_RECOVERIES_PER_GENERATION`] recoveries are kept per
    ///   generation. These need no session: their signatures say who may
    ///   publish them.
    /// - `writer_start` and `writer_handoff` must be signed by the session's
    ///   device, a current member, with its roster key.
    /// - `key_envelope` needs a member's or the root's session, and must be
    ///   signed by the admin of the manifest it names, or the current admin,
    ///   with that admin's roster key.
    /// - `genesis` (provisioned by the operator) and `enrollment_request`
    ///   (left in the mailbox) do not belong in the log.
    ///
    /// Writer claims and envelopes count toward the route's byte quota,
    /// their only bound, as for records. Transitions are exempt from it, so
    /// a full route can still remove a device, and are bounded by
    /// [`MAX_STORED_TRANSITIONS`] instead.
    pub async fn put_authority_as(
        &self,
        route: &Route,
        session: Option<&Hash>,
        digest: &Hash,
        bytes: &[u8],
    ) -> Result<Appended> {
        if bytes.len() > MAX_AUTHORITY_BYTES {
            return Err(RelayError::TooLarge);
        }
        if &record_hash(bytes) != digest {
            return Err(RelayError::ContextMismatch);
        }
        let parsed = authority::parse(bytes).map_err(RelayError::Authority)?;
        if parsed.message.route != *route {
            return Err(RelayError::ContextMismatch);
        }
        let kind = parsed.message.body.kind();
        let mut tx = self.pool.begin().await?;
        let row = route_row(&mut tx, route, true).await?;
        let existing: Option<i64> =
            sqlx::query_scalar("SELECT sequence FROM authority WHERE route = $1 AND digest = $2")
                .bind(row.id)
                .bind(digest.as_slice())
                .fetch_optional(&mut *tx)
                .await?;
        if let Some(seq) = existing {
            return Ok(Appended::Existing(Receipt {
                sequence: sequence(seq),
                generation: row.generation,
                digest: *digest,
            }));
        }
        if matches!(
            parsed.message.body,
            Body::Membership(_) | Body::RootRecovery(_)
        ) {
            let stored: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM authority
                 WHERE route = $1 AND kind IN ('membership', 'root_recovery')",
            )
            .bind(row.id)
            .fetch_one(&mut *tx)
            .await?;
            if stored >= MAX_STORED_TRANSITIONS {
                return Err(RelayError::QuotaExhausted);
            }
        }
        let replayed = self.replayed(&mut tx, route, &row).await?;
        let transition = match &parsed.message.body {
            Body::Membership(_) => {
                // A frozen authority answers every transition with `Fork`
                // before checking its signature: only the first competing
                // successor, verified, is evidence.
                if replayed.authority.is_forked() {
                    return Err(RelayError::Authority(authority::Error::Fork));
                }
                let mut authority = replayed.authority.clone();
                match authority.apply_membership(bytes) {
                    Ok(_) | Err(authority::Error::Fork) => {}
                    Err(error) => return Err(RelayError::Authority(error)),
                }
                true
            }
            Body::RootRecovery(m) => {
                let mut authority = replayed.authority.clone();
                match authority.apply_recovery(bytes) {
                    Ok(_) => {}
                    Err(authority::Error::Fork) => {
                        let stored = replayed.recoveries.get(&m.generation).copied();
                        if stored.unwrap_or(0) >= MAX_RECOVERIES_PER_GENERATION {
                            return Err(RelayError::QuotaExhausted);
                        }
                    }
                    Err(error) => return Err(RelayError::Authority(error)),
                }
                true
            }
            Body::WriterStart(_) | Body::WriterHandoff(_) => {
                let device = session.ok_or(RelayError::Unauthenticated)?;
                if *device == replayed.authority.root() {
                    return Err(RelayError::NotAMember);
                }
                let claim = parsed.message.body.writer_claim();
                if parsed.message.signer != *device {
                    return Err(RelayError::ContextMismatch);
                }
                if claim.map(|c| c.device) != Some(*device) {
                    return Err(RelayError::Authority(authority::Error::Identity));
                }
                let member = replayed
                    .authority
                    .members()
                    .ok()
                    .and_then(|m| m.into_iter().find(|m| m.id == *device))
                    .ok_or(RelayError::NotAMember)?;
                verify_signature(&parsed, &member.signing_key).map_err(RelayError::Authority)?;
                false
            }
            Body::KeyEnvelope(k) => {
                let device = session.ok_or(RelayError::Unauthenticated)?;
                if *device != replayed.authority.root() && !member(&replayed.authority, device) {
                    return Err(RelayError::NotAMember);
                }
                let signer = parsed.message.signer;
                let named = replayed
                    .manifests
                    .get(&k.manifest)
                    .ok_or(RelayError::Authority(authority::Error::Stale))?;
                if k.epoch != named.epoch {
                    return Err(RelayError::Authority(authority::Error::Stale));
                }
                if k.authority != signer {
                    return Err(RelayError::Authority(authority::Error::Unauthorized));
                }
                // The named epoch's admin (its own and recovery envelopes),
                // or the current admin (history envelopes for a new member).
                let key = if signer == named.admin {
                    named.admin_key.clone()
                } else if replayed.authority.admin().ok() == Some(signer) {
                    replayed
                        .authority
                        .members()
                        .ok()
                        .and_then(|m| m.into_iter().find(|m| m.id == signer))
                        .map(|m| m.signing_key)
                        .ok_or(RelayError::Authority(authority::Error::Unauthorized))?
                } else {
                    return Err(RelayError::Authority(authority::Error::Unauthorized));
                };
                verify_signature(&parsed, &key).map_err(RelayError::Authority)?;
                false
            }
            // Stored only by provisioning (its bytes again are answered
            // above, as a client re-publishing its controls sends them), and
            // never: requests go to the mailbox.
            Body::Genesis(_) | Body::EnrollmentRequest(_) => {
                return Err(RelayError::Authority(authority::Error::Unsupported))
            }
        };
        let seq = insert_authority(&mut tx, row.id, digest, kind, bytes, !transition).await?;
        tx.commit().await?;
        Ok(Appended::Stored(Receipt {
            sequence: seq,
            generation: row.generation,
            digest: *digest,
        }))
    }

    /// As [`Self::put_authority_as`] with no session: only transitions
    /// and recoveries.
    pub async fn put_authority(
        &self,
        route: &Route,
        digest: &Hash,
        bytes: &[u8],
    ) -> Result<Appended> {
        self.put_authority_as(route, None, digest, bytes).await
    }

    /// Register an invitation's `lookup` for `device`, which must be the
    /// route's current admin (#45): the mailbox then takes one enrollment
    /// request under it, for [`ENROLLMENT_TTL_SECONDS`] from now. A route
    /// keeps at most [`MAX_ENROLLMENTS`] unexpired invitations. The same
    /// lookup again returns `false` and keeps its first expiry.
    pub async fn register_invitation(
        &self,
        route: &Route,
        device: &Hash,
        lookup: &[u8; 16],
    ) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let row = route_row(&mut tx, route, true).await?;
        let replayed = self.replayed(&mut tx, route, &row).await?;
        if replayed.authority.admin().ok() != Some(*device) {
            return Err(RelayError::NotAMember);
        }
        sqlx::query(
            "DELETE FROM invitations WHERE route = $1
             AND registered_at < now() - ($2::BIGINT * interval '1 second')",
        )
        .bind(row.id)
        .bind(ENROLLMENT_TTL_SECONDS as i64)
        .execute(&mut *tx)
        .await?;
        let known: Option<i32> =
            sqlx::query_scalar("SELECT 1 FROM invitations WHERE route = $1 AND lookup = $2")
                .bind(row.id)
                .bind(lookup.as_slice())
                .fetch_optional(&mut *tx)
                .await?;
        if known.is_some() {
            tx.commit().await?;
            return Ok(false);
        }
        let pending: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM invitations WHERE route = $1")
            .bind(row.id)
            .fetch_one(&mut *tx)
            .await?;
        if pending >= MAX_ENROLLMENTS {
            return Err(RelayError::QuotaExhausted);
        }
        sqlx::query("INSERT INTO invitations (route, lookup) VALUES ($1, $2)")
            .bind(row.id)
            .bind(lookup.as_slice())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Leave a joining device's enrollment request under `lookup` (#42).
    /// No session is needed: the device is not a member yet. The lookup
    /// must be a registered, unexpired invitation's
    /// ([`Self::register_invitation`]), which takes one request. The relay
    /// checks that the bytes are an enrollment request for this route, and
    /// keeps at most [`MAX_ENROLLMENTS`] unexpired ones per route, each for
    /// [`ENROLLMENT_TTL_SECONDS`].
    ///
    /// The same bytes again return `false`. Other bytes under the same
    /// lookup conflict.
    pub async fn put_enrollment(
        &self,
        route: &Route,
        lookup: &[u8; 16],
        bytes: &[u8],
    ) -> Result<bool> {
        let parsed = authority::parse(bytes).map_err(|_| RelayError::Malformed)?;
        if parsed.message.body.kind() != "enrollment_request" {
            return Err(RelayError::Malformed);
        }
        if parsed.message.route != *route {
            return Err(RelayError::ContextMismatch);
        }
        let mut tx = self.pool.begin().await?;
        let row = route_row(&mut tx, route, true).await?;
        let invited: Option<i32> = sqlx::query_scalar(
            "SELECT 1 FROM invitations WHERE route = $1 AND lookup = $2
             AND registered_at >= now() - ($3::BIGINT * interval '1 second')",
        )
        .bind(row.id)
        .bind(lookup.as_slice())
        .bind(ENROLLMENT_TTL_SECONDS as i64)
        .fetch_optional(&mut *tx)
        .await?;
        if invited.is_none() {
            return Err(RelayError::UnknownInvitation);
        }
        sqlx::query(
            "DELETE FROM enrollments WHERE route = $1
             AND received_at < now() - ($2::BIGINT * interval '1 second')",
        )
        .bind(row.id)
        .bind(ENROLLMENT_TTL_SECONDS as i64)
        .execute(&mut *tx)
        .await?;
        let existing: Option<Vec<u8>> =
            sqlx::query_scalar("SELECT request FROM enrollments WHERE route = $1 AND lookup = $2")
                .bind(row.id)
                .bind(lookup.as_slice())
                .fetch_optional(&mut *tx)
                .await?;
        if let Some(existing) = existing {
            if existing != bytes {
                return Err(RelayError::RecordConflict {
                    digest: record_hash(&existing),
                });
            }
            return Ok(false);
        }
        let pending: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM enrollments WHERE route = $1")
            .bind(row.id)
            .fetch_one(&mut *tx)
            .await?;
        if pending >= MAX_ENROLLMENTS {
            return Err(RelayError::QuotaExhausted);
        }
        sqlx::query("INSERT INTO enrollments (route, lookup, request) VALUES ($1, $2, $3)")
            .bind(row.id)
            .bind(lookup.as_slice())
            .bind(bytes)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(true)
    }

    /// The unexpired enrollment request under `lookup`, for the route's
    /// admin (`device`, already authenticated). Anyone else, and an
    /// expired or absent request, gets `None`.
    pub async fn enrollment(
        &self,
        route: &Route,
        device: &Hash,
        lookup: &[u8; 16],
    ) -> Result<Option<Vec<u8>>> {
        let authority = self.authority(route).await?;
        if authority.admin().ok() != Some(*device) {
            return Ok(None);
        }
        let mut tx = self.pool.begin().await?;
        let row = route_row(&mut tx, route, false).await?;
        let request: Option<Vec<u8>> = sqlx::query_scalar(
            "SELECT request FROM enrollments WHERE route = $1 AND lookup = $2
             AND received_at >= now() - ($3::BIGINT * interval '1 second')",
        )
        .bind(row.id)
        .bind(lookup.as_slice())
        .bind(ENROLLMENT_TTL_SECONDS as i64)
        .fetch_optional(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(request)
    }

    /// A page of the route's records after `after` (from sequence 1 when
    /// `None`), at most `limit` (1 to [`MAX_PAGE`]) and [`MAX_PAGE_BYTES`]
    /// of entry bytes after the first.
    pub async fn records(&self, route: &Route, after: Option<&Cursor>, limit: u32) -> Result<Page> {
        self.page(route, after, limit, Log::Records).await
    }

    /// The route's newest `snapshot` records (checkpoint parts), newest
    /// first, at most `limit` (1 to [`MAX_PAGE`]), with the generation they
    /// are in.
    pub async fn snapshots(&self, route: &Route, limit: u32) -> Result<Snapshots> {
        if !(1..=MAX_PAGE).contains(&limit) {
            return Err(RelayError::Malformed);
        }
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query(
            "SELECT id, generation FROM routes
             WHERE application = $1 AND tenant = $2 AND collection = $3",
        )
        .bind(route.application.as_slice())
        .bind(route.tenant.as_slice())
        .bind(route.collection.as_slice())
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(RelayError::UnknownRoute)?;
        let id: i64 = row.get("id");
        let generation = generation_of(row.get("generation"))?;
        let rows = sqlx::query(
            "SELECT sequence, bytes FROM (
                 SELECT sequence, bytes,
                        row_number() OVER (ORDER BY sequence DESC) AS n,
                        SUM(octet_length(bytes)) OVER (ORDER BY sequence DESC) AS running
                 FROM records WHERE route = $1 AND snapshot
                 ORDER BY sequence DESC LIMIT $2
             ) page WHERE n = 1 OR running <= $3
             ORDER BY sequence DESC",
        )
        .bind(id)
        .bind(i64::from(limit))
        .bind(MAX_PAGE_BYTES)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Snapshots {
            generation,
            entries: rows
                .iter()
                .map(|r| (sequence(r.get("sequence")), r.get("bytes")))
                .collect(),
        })
    }

    /// A page of the route's authority objects, as for [`Self::records`].
    pub async fn authority_objects(
        &self,
        route: &Route,
        after: Option<&Cursor>,
        limit: u32,
    ) -> Result<Page> {
        self.page(route, after, limit, Log::Authority).await
    }

    async fn page(
        &self,
        route: &Route,
        after: Option<&Cursor>,
        limit: u32,
        log: Log,
    ) -> Result<Page> {
        if !(1..=MAX_PAGE).contains(&limit) {
            return Err(RelayError::Malformed);
        }
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query(match log {
            Log::Records => {
                "SELECT id, generation, record_high_water AS high_water FROM routes
                 WHERE application = $1 AND tenant = $2 AND collection = $3"
            }
            Log::Authority => {
                "SELECT id, generation, authority_high_water AS high_water FROM routes
                 WHERE application = $1 AND tenant = $2 AND collection = $3"
            }
        })
        .bind(route.application.as_slice())
        .bind(route.tenant.as_slice())
        .bind(route.collection.as_slice())
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(RelayError::UnknownRoute)?;
        let id: i64 = row.get("id");
        let generation = generation_of(row.get("generation"))?;
        let high_water = sequence(row.get("high_water"));
        // A cursor of another generation, or one past the high-water mark
        // (history shrank under the same generation: a restore that skipped
        // `new-generation`), is not continuity. Answering it with an empty,
        // complete page would silently skip whatever is appended next.
        let last = match after {
            Some(cursor) if cursor.generation != generation || cursor.last > high_water => {
                return Err(RelayError::GenerationChanged { generation });
            }
            Some(cursor) => cursor.last,
            None => 0,
        };
        // Up to `limit` entries, cut where their bytes pass the page's
        // budget, but never below one entry. Sizes come from the column
        // headers, so entries past the cut are never read out.
        let rows = sqlx::query(match log {
            Log::Records => {
                "SELECT sequence, bytes FROM (
                     SELECT sequence, bytes,
                            row_number() OVER (ORDER BY sequence) AS n,
                            SUM(octet_length(bytes)) OVER (ORDER BY sequence) AS running
                     FROM records
                     WHERE route = $1 AND sequence > $2 AND sequence <= $3
                     ORDER BY sequence LIMIT $4
                 ) page WHERE n = 1 OR running <= $5
                 ORDER BY sequence"
            }
            Log::Authority => {
                "SELECT sequence, bytes FROM (
                     SELECT sequence, bytes,
                            row_number() OVER (ORDER BY sequence) AS n,
                            SUM(octet_length(bytes)) OVER (ORDER BY sequence) AS running
                     FROM authority
                     WHERE route = $1 AND sequence > $2 AND sequence <= $3
                     ORDER BY sequence LIMIT $4
                 ) page WHERE n = 1 OR running <= $5
                 ORDER BY sequence"
            }
        })
        .bind(id)
        .bind(i64::try_from(last).map_err(|_| RelayError::Malformed)?)
        .bind(i64::try_from(high_water).map_err(|_| RelayError::Malformed)?)
        .bind(i64::from(limit))
        .bind(MAX_PAGE_BYTES)
        .fetch_all(&mut *tx)
        .await?;
        let entries: Vec<(u64, Vec<u8>)> = rows
            .into_iter()
            .map(|r| (sequence(r.get("sequence")), r.get("bytes")))
            .collect();
        let reached = entries.last().map_or(last, |(s, _)| *s);
        Ok(Page {
            generation,
            high_water,
            complete: reached >= high_water,
            next: Cursor {
                generation,
                last: reached,
            },
            entries,
        })
    }
}

/// The two logs a route keeps.
#[derive(Clone, Copy)]
enum Log {
    Records,
    Authority,
}

fn generation_of(bytes: Vec<u8>) -> Result<[u8; 16]> {
    bytes.try_into().map_err(|_| RelayError::Malformed)
}

pub(crate) fn member(authority: &Authority, device: &Hash) -> bool {
    authority
        .members()
        .is_ok_and(|m| m.iter().any(|m| m.id == *device))
}

pub(crate) async fn route_row(
    tx: &mut Transaction<'static, Postgres>,
    route: &Route,
    lock: bool,
) -> Result<RouteRow> {
    let sql = if lock {
        "SELECT id, root_key, generation, authority_high_water,
                (SELECT digest FROM authority a
                 WHERE a.route = routes.id AND a.sequence = routes.authority_high_water)
                AS authority_head
         FROM routes
         WHERE application = $1 AND tenant = $2 AND collection = $3 FOR UPDATE"
    } else {
        "SELECT id, root_key, generation, authority_high_water,
                (SELECT digest FROM authority a
                 WHERE a.route = routes.id AND a.sequence = routes.authority_high_water)
                AS authority_head
         FROM routes
         WHERE application = $1 AND tenant = $2 AND collection = $3"
    };
    let row = sqlx::query(sql)
        .bind(route.application.as_slice())
        .bind(route.tenant.as_slice())
        .bind(route.collection.as_slice())
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(RelayError::UnknownRoute)?;
    Ok(RouteRow {
        id: row.get("id"),
        root_key: row.get("root_key"),
        generation: generation_of(row.get("generation"))?,
        authority_high_water: row.get("authority_high_water"),
        authority_head: row.get("authority_head"),
    })
}

impl Relay {
    /// The route's authority as of `row`, read in `tx`: from the cache when
    /// the log is unchanged since it was last replayed, else replayed.
    pub(crate) async fn replayed(
        &self,
        tx: &mut Transaction<'static, Postgres>,
        route: &Route,
        row: &RouteRow,
    ) -> Result<Arc<Replayed>> {
        let key: CacheKey = (
            row.id,
            row.generation,
            row.authority_high_water,
            row.authority_head.clone(),
        );
        let cached = self
            .authorities
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key);
        if let Some(replayed) = cached {
            return Ok(replayed);
        }
        let replayed = Arc::new(replay(tx, route, row).await?);
        self.authorities
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .put(key, replayed.clone());
        Ok(replayed)
    }
}

/// Replay the route's genesis, transitions and recoveries, in acceptance
/// order, against its pinned root.
async fn replay(
    tx: &mut Transaction<'static, Postgres>,
    route: &Route,
    row: &RouteRow,
) -> Result<Replayed> {
    // Up to the high-water mark the row was read at, so that the replay is
    // exactly the one its cache key names, even under READ COMMITTED.
    let rows = sqlx::query(
        "SELECT kind, bytes FROM authority
         WHERE route = $1 AND sequence <= $2
           AND kind IN ('genesis', 'membership', 'root_recovery')
         ORDER BY sequence",
    )
    .bind(row.id)
    .bind(row.authority_high_water)
    .fetch_all(&mut **tx)
    .await?;
    let mut rows = rows.into_iter();
    let genesis: Vec<u8> = rows.next().ok_or(RelayError::UnknownRoute)?.get("bytes");
    let mut authority = Authority::bootstrap(&genesis, route.clone(), &root_key(&row.root_key)?)
        .map_err(RelayError::Authority)?;
    let mut manifests = HashMap::new();
    if let Body::Genesis(g) = authority::parse(&genesis)
        .map_err(RelayError::Authority)?
        .message
        .body
    {
        if let Some(m) = Manifest::of(g.epoch, g.admin, &g.roster) {
            manifests.insert(record_hash(&genesis), m);
        }
    }
    let mut recoveries: HashMap<u64, usize> = HashMap::new();
    for r in rows {
        let kind: String = r.get("kind");
        let bytes: Vec<u8> = r.get("bytes");
        let applied = if kind == "membership" {
            authority.apply_membership(&bytes)
        } else {
            authority.apply_recovery(&bytes)
        };
        let body = authority::parse(&bytes)
            .map_err(RelayError::Authority)?
            .message
            .body;
        let (Body::Membership(m) | Body::RootRecovery(m)) = &body else {
            return Err(RelayError::Malformed);
        };
        if matches!(body, Body::RootRecovery(_)) {
            *recoveries.entry(m.generation).or_default() += 1;
        }
        match applied {
            Ok(true) => {
                if let Some(manifest) = Manifest::of(m.epoch, m.admin, &m.roster) {
                    manifests.insert(record_hash(&bytes), manifest);
                }
            }
            Ok(false) | Err(authority::Error::Fork) => {}
            Err(error) => return Err(RelayError::Authority(error)),
        }
    }
    Ok(Replayed {
        authority,
        manifests,
        recoveries,
    })
}

/// Append `bytes` to the route's authority log, counting them toward its
/// stored bytes. With `quota`, refuse with [`RelayError::QuotaExhausted`]
/// before storing anything if they would pass the route's byte quota.
pub(crate) async fn insert_authority(
    tx: &mut Transaction<'static, Postgres>,
    route: i64,
    digest: &Hash,
    kind: &str,
    bytes: &[u8],
    quota: bool,
) -> Result<u64> {
    let size = i64::try_from(bytes.len()).map_err(|_| RelayError::TooLarge)?;
    let seq: Option<i64> = sqlx::query_scalar(
        "UPDATE routes SET authority_high_water = authority_high_water + 1,
             used_bytes = used_bytes + $2
         WHERE id = $1 AND (NOT $3 OR quota_bytes IS NULL OR used_bytes + $2 <= quota_bytes)
         RETURNING authority_high_water",
    )
    .bind(route)
    .bind(size)
    .bind(quota)
    .fetch_optional(&mut **tx)
    .await?;
    let seq = seq.ok_or(RelayError::QuotaExhausted)?;
    sqlx::query(
        "INSERT INTO authority (route, sequence, digest, kind, bytes) VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(route)
    .bind(seq)
    .bind(digest.as_slice())
    .bind(kind)
    .bind(bytes)
    .execute(&mut **tx)
    .await?;
    Ok(sequence(seq))
}

#[cfg(test)]
mod tests {
    use super::{AuthorityCache, CACHED_AUTHORITIES};

    #[test]
    fn the_authority_cache_keeps_each_routes_newest_replay_and_is_bounded() {
        let mut cache = AuthorityCache::<u32>::default();
        let key =
            |id: i64, high_water: i64| (id, [1; 16], high_water, Some(vec![high_water as u8]));
        cache.put(key(1, 1), 10);
        assert_eq!(cache.get(&key(1, 1)), Some(10));
        // A longer log, another generation or another head is another replay.
        assert_eq!(cache.get(&key(1, 2)), None);
        assert_eq!(cache.get(&(1, [2; 16], 1, Some(vec![1]))), None);
        assert_eq!(cache.get(&(1, [1; 16], 1, Some(vec![9]))), None);
        cache.put(key(1, 2), 11);
        assert_eq!(cache.get(&key(1, 1)), None, "replaced, not kept beside");
        assert_eq!(cache.get(&key(1, 2)), Some(11));
        // Full: the least recently used route makes room.
        for id in 2..=CACHED_AUTHORITIES as i64 {
            cache.put(key(id, 1), 0);
        }
        assert_eq!(cache.get(&key(1, 2)), Some(11));
        cache.put(key(0, 1), 1);
        assert_eq!(cache.entries.len(), CACHED_AUTHORITIES);
        assert_eq!(cache.get(&key(2, 1)), None, "evicted");
        assert_eq!(cache.get(&key(1, 2)), Some(11), "recently used, kept");
    }
}
