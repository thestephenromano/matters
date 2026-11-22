use matters_sync::authority::{self, Authority, Body, Hash, Route};
use matters_sync::record::record_hash;
use matters_sync::{wire, VerificationKey};
use minicbor::{Decoder, Encoder};
use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::{Postgres, Row, Transaction};

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
    #[error("too many requests; retry after {retry_after_ms} ms")]
    RateLimited { retry_after_ms: u64 },
    #[error("the authority object was refused: {0}")]
    Authority(authority::Error),
    #[error("the database is unavailable: {0}")]
    Unavailable(#[from] sqlx::Error),
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
            Self::RateLimited { .. } => "rate-limited",
            Self::Unavailable(_) => "unavailable",
        }
    }

    /// Whether the client may retry the same request unchanged.
    pub fn retry(&self) -> bool {
        matches!(self, Self::Unavailable(_) | Self::RateLimited { .. })
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

/// Unexpired enrollment requests a route keeps at once.
pub const MAX_ENROLLMENTS: i64 = 16;
/// How long an enrollment request is kept: an invitation is used within
/// minutes.
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
}

/// A provisioned route's row.
pub(crate) struct RouteRow {
    pub(crate) id: i64,
    root_key: Vec<u8>,
    pub(crate) generation: [u8; 16],
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
        // A request waits at most 5 s for a connection, then answers
        // `unavailable`, which clients retry with backoff.
        let pool = PgPoolOptions::new()
            .max_connections(16)
            .acquire_timeout(std::time::Duration::from_secs(5))
            .connect(url)
            .await?;
        MIGRATOR
            .run(&pool)
            .await
            .map_err(|e| RelayError::Unavailable(e.into()))?;
        Ok(Self { pool })
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
        insert_authority(&mut tx, id, &record_hash(genesis), "genesis", genesis).await?;
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
        let authority = route_authority(&mut tx, route, &row).await?;
        Ok(member(&authority, device))
    }

    /// The route's authority as the relay has accepted it.
    pub async fn authority(&self, route: &Route) -> Result<Authority> {
        let mut tx = self.pool.begin().await?;
        let row = route_row(&mut tx, route, false).await?;
        route_authority(&mut tx, route, &row).await
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
        let authority = route_authority(&mut tx, route, &row).await?;
        if !member(&authority, device) {
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

    /// Store an authority object under its SHA-384 `digest`. Membership
    /// transitions and root recoveries must be accepted by the route's
    /// authority first (a fork's competing record is kept as evidence);
    /// other kinds are checked structurally. Identical bytes again return
    /// the original receipt.
    pub async fn put_authority(
        &self,
        route: &Route,
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
        match parsed.message.body {
            Body::Membership(_) | Body::RootRecovery(_) => {
                let mut authority = route_authority(&mut tx, route, &row).await?;
                let applied = match parsed.message.body {
                    Body::Membership(_) => authority.apply_membership(bytes),
                    _ => authority.apply_recovery(bytes),
                };
                match applied {
                    Ok(_) | Err(authority::Error::Fork) => {}
                    Err(error) => return Err(RelayError::Authority(error)),
                }
            }
            Body::Genesis(_) => return Err(RelayError::Authority(authority::Error::Unsupported)),
            _ => {}
        }
        let seq = insert_authority(&mut tx, row.id, digest, kind, bytes).await?;
        tx.commit().await?;
        Ok(Appended::Stored(Receipt {
            sequence: seq,
            generation: row.generation,
            digest: *digest,
        }))
    }

    /// Leave a joining device's enrollment request under `lookup` (#42).
    /// No session is needed: the device is not a member yet. The relay checks
    /// that the bytes are an enrollment request for this route, and keeps at
    /// most [`MAX_ENROLLMENTS`] unexpired ones per route, each for
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
        "SELECT id, root_key, generation FROM routes
         WHERE application = $1 AND tenant = $2 AND collection = $3 FOR UPDATE"
    } else {
        "SELECT id, root_key, generation FROM routes
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
    })
}

/// Replay the route's genesis, transitions and recoveries, in acceptance
/// order, against its pinned root.
pub(crate) async fn route_authority(
    tx: &mut Transaction<'static, Postgres>,
    route: &Route,
    row: &RouteRow,
) -> Result<Authority> {
    let rows = sqlx::query(
        "SELECT kind, bytes FROM authority
         WHERE route = $1 AND kind IN ('genesis', 'membership', 'root_recovery')
         ORDER BY sequence",
    )
    .bind(row.id)
    .fetch_all(&mut **tx)
    .await?;
    let mut rows = rows.into_iter();
    let genesis: Vec<u8> = rows.next().ok_or(RelayError::UnknownRoute)?.get("bytes");
    let mut authority = Authority::bootstrap(&genesis, route.clone(), &root_key(&row.root_key)?)
        .map_err(RelayError::Authority)?;
    for r in rows {
        let kind: String = r.get("kind");
        let bytes: Vec<u8> = r.get("bytes");
        let applied = if kind == "membership" {
            authority.apply_membership(&bytes)
        } else {
            authority.apply_recovery(&bytes)
        };
        match applied {
            Ok(_) | Err(authority::Error::Fork) => {}
            Err(error) => return Err(RelayError::Authority(error)),
        }
    }
    Ok(authority)
}

pub(crate) async fn insert_authority(
    tx: &mut Transaction<'static, Postgres>,
    route: i64,
    digest: &Hash,
    kind: &str,
    bytes: &[u8],
) -> Result<u64> {
    let seq: i64 = sqlx::query_scalar(
        "UPDATE routes SET authority_high_water = authority_high_water + 1
         WHERE id = $1 RETURNING authority_high_water",
    )
    .bind(route)
    .fetch_one(&mut **tx)
    .await?;
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
