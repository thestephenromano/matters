//! This device's membership of a sync collection (#37,
//! docs/SYNC_KEY_DISTRIBUTION.md "Persistence").
//!
//! The device's private keys are made from two 32-byte seeds kept in the
//! platform keyring ([`KeyStore`]; oo7 in the app). SQLite keeps only what
//! is safe to store: the signed control records this device accepted, in
//! order, the key envelopes addressed to it, and its device record. Each
//! open rebuilds the authority by replaying the controls, so membership is
//! never split between two stores: a control and its effect commit in one
//! transaction.
//!
//! A joined, active device seals every recorded write. Until
//! [`Database::resume_sync`] loads its keys, writes are refused rather than
//! journalled unsealed. A device the collection removed, or whose authority
//! froze on a fork, keeps editing locally: those writes are journalled and
//! kept, unsent, for explicit recovery.

#[cfg(feature = "test-support")]
use std::collections::BTreeMap;

#[cfg(feature = "test-support")]
use matters_sync::authority::RecoverySecret;
use matters_sync::authority::{self, Authority, Body, DeviceKeys, Route};
use matters_sync::generate_signing_key;
#[cfg(feature = "test-support")]
use matters_sync::ContentKey;
use rusqlite::{params, Connection, OptionalExtension, Transaction};

use super::db::Database;
#[cfg(feature = "test-support")]
use super::sealing::random32;
use super::sealing::{Sealing, SealingIdentity, SyncKeys};
use crate::error::{MattersError, Result};

/// The one document a Matters collection holds: every device seals its
/// records for it, so a joining device needs nothing more to find it.
#[cfg(feature = "test-support")]
pub(super) const HOME_DOCUMENT: [u8; 16] = *b"matters/home\0\0\0\0";

/// Overwrite secret bytes before they are freed (best effort: the
/// compiler is told the zeroes are observed).
pub(super) fn wipe(bytes: &mut [u8]) {
    bytes.fill(0);
    std::hint::black_box(bytes);
}

/// The two seeds a device's private keys are made from. Zeroed when
/// dropped; never printed or cloned.
pub struct DeviceSeeds {
    signing: [u8; 32],
    kem: [u8; 32],
}

impl DeviceSeeds {
    /// Bytes of a stored seed pair.
    pub const BYTES: usize = 64;

    #[cfg(feature = "test-support")]
    pub(super) fn generate() -> Self {
        Self {
            signing: random32(),
            kem: random32(),
        }
    }

    /// The 64 stored bytes. The caller stores them in a keyring and wipes
    /// its copy.
    pub fn to_bytes(&self) -> [u8; Self::BYTES] {
        let mut out = [0; Self::BYTES];
        out[..32].copy_from_slice(&self.signing);
        out[32..].copy_from_slice(&self.kem);
        out
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != Self::BYTES {
            return Err(MattersError::Validation("stored device keys".into()));
        }
        let mut seeds = Self {
            signing: [0; 32],
            kem: [0; 32],
        };
        seeds.signing.copy_from_slice(&bytes[..32]);
        seeds.kem.copy_from_slice(&bytes[32..]);
        Ok(seeds)
    }

    /// The device keys these seeds make.
    pub fn keys(&self) -> Result<DeviceKeys> {
        Ok(DeviceKeys::generate(self.signing, self.kem)?)
    }
}

impl Drop for DeviceSeeds {
    fn drop(&mut self) {
        wipe(&mut self.signing);
        wipe(&mut self.kem);
    }
}

impl std::fmt::Debug for DeviceSeeds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DeviceSeeds(redacted)")
    }
}

/// Where a device keeps its key seeds: the platform keyring in the app, a
/// map in tests. An implementation must never log or print what it holds.
pub trait KeyStore {
    fn load(&self, name: &str) -> Result<Option<DeviceSeeds>>;
    fn store(&self, name: &str, seeds: &DeviceSeeds) -> Result<()>;
}

/// A [`KeyStore`] in memory, for tests.
#[cfg(feature = "test-support")]
#[derive(Default)]
pub struct MemoryKeyStore(std::sync::Mutex<BTreeMap<String, Vec<u8>>>);

#[cfg(feature = "test-support")]
impl MemoryKeyStore {
    /// The names of the entries held.
    pub fn names(&self) -> Vec<String> {
        self.0.lock().unwrap().keys().cloned().collect()
    }
}

#[cfg(feature = "test-support")]
impl KeyStore for MemoryKeyStore {
    fn load(&self, name: &str) -> Result<Option<DeviceSeeds>> {
        let map = self.0.lock().unwrap();
        map.get(name)
            .map(|b| DeviceSeeds::from_bytes(b))
            .transpose()
    }

    fn store(&self, name: &str, seeds: &DeviceSeeds) -> Result<()> {
        let mut bytes = seeds.to_bytes();
        self.0
            .lock()
            .unwrap()
            .insert(name.to_owned(), bytes.to_vec());
        wipe(&mut bytes);
        Ok(())
    }
}

/// How this device stands in its collection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncStanding {
    /// A member: writes seal.
    Active,
    /// Removed by the collection (or superseded by a recovery): writes are
    /// kept locally, unsent.
    Revoked,
    /// The authority froze on competing signed histories: writes are kept
    /// locally, unsent, until an explicit recovery.
    Frozen,
}

impl SyncStanding {
    fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Revoked => "revoked",
            Self::Frozen => "frozen",
        }
    }
}

/// A joined device's keys and authority, while loaded.
pub(crate) struct Joined {
    pub(super) route: Route,
    pub(super) document: [u8; 16],
    pub(super) seeds: DeviceSeeds,
    pub(super) device: DeviceKeys,
    pub(super) authority: Authority,
}

impl Joined {
    /// The sealing identity of the current epoch, from its stored
    /// envelope.
    fn identity(&self, tx: &Connection) -> Result<SealingIdentity> {
        let manifest = self.authority.manifest();
        let envelope: Vec<u8> = tx
            .query_row(
                "SELECT bytes FROM sync_envelopes WHERE manifest = ?1",
                params![manifest.as_slice()],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| {
                MattersError::Validation("no key envelope for the current epoch".into())
            })?;
        let key = self.authority.unwrap_key(&envelope, &self.device)?;
        let (signing_key, _) = generate_signing_key(self.seeds.signing);
        Ok(SealingIdentity {
            application: self.route.application,
            tenant: self.route.tenant,
            collection: self.route.collection,
            document: self.document,
            epoch: self.authority.epoch()?,
            manifest,
            author: self.device.id(),
            content_key: key.into_content_key(),
            signing_key,
        })
    }

    pub(super) fn standing(&self) -> SyncStanding {
        if self.authority.is_forked() {
            SyncStanding::Frozen
        } else if self
            .authority
            .members()
            .is_ok_and(|m| m.iter().any(|m| m.id == self.device.id()))
        {
            SyncStanding::Active
        } else {
            SyncStanding::Revoked
        }
    }
}

pub(crate) fn joined_and_active(conn: &Connection) -> Result<bool> {
    let state: Option<String> = conn
        .query_row("SELECT state FROM sync_device", [], |r| r.get(0))
        .optional()?;
    Ok(state.as_deref() == Some("active"))
}

fn controls(conn: &Connection) -> Result<Vec<Vec<u8>>> {
    let mut stmt = conn.prepare("SELECT bytes FROM sync_controls ORDER BY seq")?;
    let rows = stmt.query_map([], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Apply one stored or received control to `authority`. `Ok(true)`
/// changed it, `Ok(false)` was a replay; `Err(Fork)` froze it.
pub(super) fn apply(authority: &mut Authority, bytes: &[u8]) -> authority::Result<bool> {
    let parsed = authority::parse(bytes)?;
    match parsed.message.body {
        Body::Membership(_) => authority.apply_membership(bytes),
        Body::RootRecovery(_) => authority.apply_recovery(bytes),
        _ => Err(authority::Error::Unsupported),
    }
}

/// Rebuild the authority from the stored controls, in order. A stored
/// fork's second record freezes it again.
fn replay(conn: &Connection, route: &Route, root: authority::Hash) -> Result<Authority> {
    let controls = controls(conn)?;
    let (genesis, rest) = controls
        .split_first()
        .ok_or_else(|| MattersError::Validation("no stored genesis".into()))?;
    let mut authority = Authority::bootstrap_by_root_id(genesis, route.clone(), root)?;
    for bytes in rest {
        match apply(&mut authority, bytes) {
            Ok(_) | Err(authority::Error::Fork) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(authority)
}

fn set_standing(tx: &Transaction<'_>, standing: SyncStanding) -> Result<()> {
    tx.execute(
        "UPDATE sync_device SET state = ?1",
        params![standing.as_str()],
    )?;
    Ok(())
}

impl SyncKeys {
    /// Seal (or stop sealing) for `joined`'s standing after a change.
    pub(super) fn settle(&mut self, conn: &Connection, joined: Joined) -> Result<SyncStanding> {
        let standing = joined.standing();
        if standing == SyncStanding::Active {
            let identity = joined.identity(conn)?;
            match self.sealing.as_mut() {
                Some(sealing) => sealing.rekey(identity),
                None => self.sealing = Some(Sealing::begin(conn, identity)?),
            }
        } else {
            self.sealing = None;
        }
        self.required = standing == SyncStanding::Active;
        self.membership = Some(joined);
        Ok(standing)
    }
}

impl Database {
    /// Start a new sync collection on this device, its first: turn sync
    /// on, generate the device's keys, the recovery secret and the first
    /// content key, sign the genesis and start sealing. Returns the 24
    /// recovery words: show them once, then drop them; nothing keeps them.
    /// The database must hold no rows yet (#41 migrates existing data).
    ///
    /// The seeds reach `store` before the transaction commits. A crash
    /// between the two leaves an unused keyring entry and no collection.
    #[cfg(feature = "test-support")]
    pub fn create_collection(&self, store: &dyn KeyStore, route: Route) -> Result<String> {
        let seeds = DeviceSeeds::generate();
        let device = seeds.keys()?;
        let recovery = RecoverySecret::new(random32());
        let words = recovery.words()?;
        let keys = recovery.keys(&route)?;
        let content = ContentKey::new(random32());
        let genesis = authority::genesis(&route, &keys, &device, &content, random32())?;
        let authority = Authority::bootstrap(&genesis, route.clone(), keys.root_key())?;
        let envelope = authority.wrap_key(&device, device.id(), &content, random32())?;
        // The words must recover epoch 0 too: its recovery envelope is
        // published like every later epoch's.
        let recovery_envelope =
            authority.wrap_recovery_key(&device, authority.manifest(), &content, random32())?;
        let document = HOME_DOCUMENT;
        let key_name = format!("matters-sync/device/{}", uuid::Uuid::new_v4());
        store.store(&key_name, &seeds)?;
        self.with_sealing(|conn, engine, sync| {
            if engine.is_some() {
                return Err(MattersError::Validation("sync is already on".into()));
            }
            let tx = conn.transaction()?;
            let doc = super::sync::turn_on(&tx)?;
            tx.execute(
                "INSERT INTO sync_device (singleton, application, tenant, collection, document,
                     root, device, key_name)
                 VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    route.application.as_slice(),
                    route.tenant.as_slice(),
                    route.collection.as_slice(),
                    document.as_slice(),
                    authority.root().as_slice(),
                    device.id().as_slice(),
                    key_name,
                ],
            )?;
            tx.execute(
                "INSERT INTO sync_controls (bytes) VALUES (?1)",
                params![genesis],
            )?;
            tx.execute(
                "INSERT INTO sync_envelopes (manifest, bytes) VALUES (?1, ?2)",
                params![authority.manifest().as_slice(), envelope],
            )?;
            tx.execute(
                "INSERT INTO sync_publish (bytes) VALUES (?1)",
                params![recovery_envelope],
            )?;
            super::sync::crash_point("collection-before-commit");
            tx.commit()?;
            *engine = Some(doc);
            sync.settle(
                conn,
                Joined {
                    route,
                    document,
                    seeds,
                    device,
                    authority,
                },
            )?;
            Ok(words)
        })
    }

    /// Load this device's keys from `store` and its authority from the
    /// stored controls, and start sealing as a new writer incarnation.
    /// Call once after each open of a joined database; until then writes
    /// are refused. Missing keys are an explicit error and change nothing:
    /// the journal and every unsent edit stay as they are.
    pub fn resume_sync(&self, store: &dyn KeyStore) -> Result<SyncStanding> {
        self.with_sealing(|conn, engine, sync| {
            if engine.is_none() {
                return Err(MattersError::Validation("sync is off".into()));
            }
            let row = conn
                .query_row(
                    "SELECT application, tenant, collection, document, root, device, key_name
                     FROM sync_device",
                    [],
                    |r| {
                        Ok((
                            r.get::<_, Vec<u8>>(0)?,
                            r.get::<_, Vec<u8>>(1)?,
                            r.get::<_, Vec<u8>>(2)?,
                            r.get::<_, Vec<u8>>(3)?,
                            r.get::<_, Vec<u8>>(4)?,
                            r.get::<_, Vec<u8>>(5)?,
                            r.get::<_, String>(6)?,
                        ))
                    },
                )
                .optional()?
                .ok_or_else(|| {
                    MattersError::Validation("this device has not joined a collection".into())
                })?;
            let fixed = |b: Vec<u8>| -> Result<[u8; 16]> {
                b.try_into()
                    .map_err(|_| MattersError::Validation("stored sync device".into()))
            };
            let route = Route {
                application: fixed(row.0)?,
                tenant: fixed(row.1)?,
                collection: fixed(row.2)?,
            };
            let document = fixed(row.3)?;
            let root: authority::Hash = row
                .4
                .try_into()
                .map_err(|_| MattersError::Validation("stored sync device".into()))?;
            let seeds = store.load(&row.6)?.ok_or_else(|| {
                MattersError::Validation(
                    "this device's sync keys are missing from the keyring; recover or re-enroll"
                        .into(),
                )
            })?;
            let device = seeds.keys()?;
            if device.id().as_slice() != row.5.as_slice() {
                return Err(MattersError::Validation(
                    "the keyring holds another device's keys".into(),
                ));
            }
            let authority = replay(conn, &route, root)?;
            sync.settle(
                conn,
                Joined {
                    route,
                    document,
                    seeds,
                    device,
                    authority,
                },
            )
        })
    }

    /// Accept a signed membership transition or root recovery received for
    /// this collection, with this device's key envelope for the new epoch
    /// when it stays a member. The control, the envelope and the device's
    /// standing commit together; sealing then continues under the new
    /// epoch, or stops if the device was removed. Competing histories
    /// freeze it: the evidence is kept and `Err(Fork)` returned. Anything
    /// else refused changes nothing.
    pub fn apply_control(&self, control: &[u8], envelope: Option<&[u8]>) -> Result<SyncStanding> {
        self.with_sealing(|conn, _, sync| {
            let Some(mut joined) = sync.membership.take() else {
                return Err(MattersError::Validation("sync is not resumed".into()));
            };
            let before = joined.authority.clone();
            let outcome = apply(&mut joined.authority, control);
            let result = (|| {
                let changed = match outcome {
                    Ok(changed) => changed,
                    Err(authority::Error::Fork) => true,
                    Err(error) => return Err(error.into()),
                };
                if !changed {
                    return Ok(false);
                }
                let standing = joined.standing();
                let manifest = joined.authority.manifest();
                let tx = conn.transaction()?;
                tx.execute(
                    "INSERT INTO sync_controls (bytes) VALUES (?1)",
                    params![control],
                )?;
                if standing == SyncStanding::Active {
                    let envelope = envelope.ok_or_else(|| {
                        MattersError::Validation("the new epoch's key envelope is missing".into())
                    })?;
                    joined.authority.unwrap_key(envelope, &joined.device)?;
                    tx.execute(
                        "INSERT OR REPLACE INTO sync_envelopes (manifest, bytes) VALUES (?1, ?2)",
                        params![manifest.as_slice(), envelope],
                    )?;
                }
                set_standing(&tx, standing)?;
                tx.commit()?;
                Ok(true)
            })();
            match result {
                Ok(true) => {
                    let standing = sync.settle(conn, joined)?;
                    match outcome {
                        Err(authority::Error::Fork) => Err(authority::Error::Fork.into()),
                        _ => Ok(standing),
                    }
                }
                Ok(false) => {
                    let standing = joined.standing();
                    sync.membership = Some(joined);
                    Ok(standing)
                }
                Err(error) => {
                    joined.authority = before;
                    sync.membership = Some(joined);
                    Err(error)
                }
            }
        })
    }

    /// The signed control records this device accepted, in order: what a
    /// joining device or a recovery replays.
    pub fn sync_controls(&self) -> Result<Vec<Vec<u8>>> {
        self.with_conn(controls)
    }

    /// The key envelopes stored for this device, by manifest.
    #[cfg(feature = "test-support")]
    pub fn sync_envelopes(&self) -> Result<Vec<(authority::Hash, Vec<u8>)>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT manifest, bytes FROM sync_envelopes")?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get(1)?)))?;
            let mut out = Vec::new();
            for row in rows {
                let (manifest, bytes) = row?;
                let manifest = manifest
                    .try_into()
                    .map_err(|_| MattersError::Validation("stored manifest".into()))?;
                out.push((manifest, bytes));
            }
            Ok(out)
        })
    }

    /// This device's ID and the collection's current members, epoch and
    /// manifest, while sync is resumed.
    pub fn sync_membership(&self) -> Result<SyncMembership> {
        self.with_sealing(|_, _, sync| {
            let joined = sync
                .membership
                .as_ref()
                .ok_or_else(|| MattersError::Validation("sync is not resumed".into()))?;
            let a = &joined.authority;
            Ok(SyncMembership {
                device: joined.device.id(),
                document: joined.document,
                standing: joined.standing(),
                generation: a.generation().ok(),
                epoch: a.epoch().ok(),
                manifest: a.manifest(),
                members: a
                    .members()
                    .map(|m| m.into_iter().map(|m| m.id).collect())
                    .unwrap_or_default(),
            })
        })
    }
}

/// A summary of this device's membership.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncMembership {
    pub device: authority::Hash,
    /// The document this device's records are sealed for.
    pub document: [u8; 16],
    pub standing: SyncStanding,
    /// `None` while frozen.
    pub generation: Option<u64>,
    pub epoch: Option<u64>,
    pub manifest: authority::Hash,
    pub members: Vec<authority::Hash>,
}

#[cfg(test)]
mod tests {
    #[test]
    fn wipe_zeroes_every_byte() {
        let mut bytes = [0xab_u8; 64];
        super::wipe(&mut bytes);
        assert_eq!(bytes, [0; 64]);
    }
}
