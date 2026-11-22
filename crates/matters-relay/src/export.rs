//! Export and import (docs/SYNC_SERVICE_CONTRACT.md "Export and import"):
//! a route's authority objects and records, byte for byte and in order,
//! as `["matters-sync-export", 0, route, generation, [authority...],
//! [[sequence, record]...]]`. Import stores the same bytes under a new
//! generation, renumbering sequences in the exported order, so clients
//! moving between relays see a generation change and resynchronize.
//!
//! The export carries no separate root key: import takes it from the
//! genesis it carries, which the operator vouches for by importing, as
//! they do for the key when provisioning. Every membership transition and
//! root recovery is verified against that root again, and every record's
//! routing fields against the route.

use matters_sync::authority::{self, Authority, Body, Route};
use matters_sync::record::record_hash;
use matters_sync::wire;
use minicbor::{Decoder, Encoder};

use crate::store::{insert_authority, random16, route_row, Relay, RelayError};

const LABEL: &str = "matters-sync-export";

type Result<T> = std::result::Result<T, RelayError>;

impl Relay {
    /// Export `route` as the contract's export bytes.
    pub async fn export(&self, route: &Route) -> Result<Vec<u8>> {
        let mut tx = self.pool().begin().await?;
        let row = route_row(&mut tx, route, false).await?;
        let authority: Vec<Vec<u8>> =
            sqlx::query_scalar("SELECT bytes FROM authority WHERE route = $1 ORDER BY sequence")
                .bind(row.id)
                .fetch_all(&mut *tx)
                .await?;
        let records: Vec<(i64, Vec<u8>)> = sqlx::query_as(
            "SELECT sequence, bytes FROM records WHERE route = $1 ORDER BY sequence",
        )
        .bind(row.id)
        .fetch_all(&mut *tx)
        .await?;
        let mut e = Encoder::new(Vec::new());
        let encoded = (|| {
            e.array(6)?.str(LABEL)?.u8(0)?;
            e.array(3)?
                .bytes(&route.application)?
                .bytes(&route.tenant)?
                .bytes(&route.collection)?;
            e.bytes(&row.generation)?;
            e.array(authority.len() as u64)?;
            for bytes in &authority {
                e.bytes(bytes)?;
            }
            e.array(records.len() as u64)?;
            for (seq, bytes) in &records {
                e.array(2)?
                    .u64(crate::store::sequence(*seq))?
                    .bytes(bytes)?;
            }
            Ok::<_, minicbor::encode::Error<std::convert::Infallible>>(())
        })();
        encoded.map_err(|_| RelayError::Malformed)?;
        Ok(e.into_writer())
    }

    /// Import an export into this relay as a new route with a new
    /// generation, in one transaction. The route must not exist here.
    pub async fn import(&self, export: &[u8]) -> Result<(Route, [u8; 16])> {
        let parsed = parse(export).ok_or(RelayError::Malformed)?;
        let (genesis, rest) = parsed
            .authority
            .split_first()
            .ok_or(RelayError::Malformed)?;
        let root = match authority::parse(genesis)
            .map_err(RelayError::Authority)?
            .message
            .body
        {
            Body::Genesis(g) => g.root_key,
            _ => return Err(RelayError::Malformed),
        };
        let route = parsed.route;
        let mut state =
            Authority::bootstrap(genesis, route.clone(), &root).map_err(RelayError::Authority)?;
        for bytes in rest {
            let p = authority::parse(bytes).map_err(RelayError::Authority)?;
            if p.message.route != route {
                return Err(RelayError::ContextMismatch);
            }
            let applied = match p.message.body {
                Body::Membership(_) => state.apply_membership(bytes),
                Body::RootRecovery(_) => state.apply_recovery(bytes),
                Body::Genesis(_) => Err(authority::Error::Unsupported),
                _ => Ok(true),
            };
            match applied {
                Ok(_) | Err(authority::Error::Fork) => {}
                Err(error) => return Err(RelayError::Authority(error)),
            }
        }
        let generation = random16();
        let mut tx = self.pool().begin().await?;
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
        for bytes in &parsed.authority {
            let kind = authority::parse(bytes)
                .map_err(RelayError::Authority)?
                .message
                .body
                .kind();
            insert_authority(&mut tx, id, &record_hash(bytes), kind, bytes).await?;
        }
        let mut used: i64 = 0;
        for (n, bytes) in parsed.records.iter().enumerate() {
            let record =
                wire::parse_for_route(bytes, &route.application, &route.tenant, &route.collection)?;
            let seq = i64::try_from(n + 1).map_err(|_| RelayError::TooLarge)?;
            sqlx::query(
                "INSERT INTO records (route, sequence, record_id, digest, bytes, snapshot)
                 VALUES ($1, $2, $3, $4, $5, $6)",
            )
            .bind(id)
            .bind(seq)
            .bind(record.header.record_id.as_slice())
            .bind(record_hash(bytes).as_slice())
            .bind(bytes.as_slice())
            .bind(record.header.kind == wire::Kind::Snapshot)
            .execute(&mut *tx)
            .await?;
            used += i64::try_from(bytes.len()).map_err(|_| RelayError::TooLarge)?;
        }
        sqlx::query("UPDATE routes SET record_high_water = $2, used_bytes = $3 WHERE id = $1")
            .bind(id)
            .bind(i64::try_from(parsed.records.len()).map_err(|_| RelayError::TooLarge)?)
            .bind(used)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok((route, generation))
    }
}

struct Parsed {
    route: Route,
    authority: Vec<Vec<u8>>,
    records: Vec<Vec<u8>>,
}

/// Parse an export: its exact shape, sequences strictly increasing.
fn parse(bytes: &[u8]) -> Option<Parsed> {
    let mut d = Decoder::new(bytes);
    if d.array().ok()? != Some(6) || d.str().ok()? != LABEL || d.u8().ok()? != 0 {
        return None;
    }
    if d.array().ok()? != Some(3) {
        return None;
    }
    let route = Route {
        application: d.bytes().ok()?.try_into().ok()?,
        tenant: d.bytes().ok()?.try_into().ok()?,
        collection: d.bytes().ok()?.try_into().ok()?,
    };
    let _generation: [u8; 16] = d.bytes().ok()?.try_into().ok()?;
    let n = d.array().ok()??;
    let mut authority = Vec::new();
    for _ in 0..n {
        authority.push(d.bytes().ok()?.to_vec());
    }
    let n = d.array().ok()??;
    let mut records = Vec::new();
    let mut last = 0;
    for _ in 0..n {
        if d.array().ok()? != Some(2) {
            return None;
        }
        let seq = d.u64().ok()?;
        if seq <= last {
            return None;
        }
        last = seq;
        records.push(d.bytes().ok()?.to_vec());
    }
    (d.position() == bytes.len()).then_some(Parsed {
        route,
        authority,
        records,
    })
}
