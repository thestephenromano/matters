//! Growing and changing a sync collection (#37, docs/SYNC_KEY_DISTRIBUTION.md
//! "Joining and administering"): the admin's invitations, enrollment,
//! removal and rotation, and a new device's side of joining.
//!
//! Everything the admin signs for others (the transition, every member's
//! envelope, the recovery envelope, a new member's history) commits with
//! the transition and waits in `sync_publish` until acknowledged. Signing
//! a transition again would fork the collection, so retries resend these
//! exact bytes.

use matters_sync::authority::{
    self, Body, Hash, Invitation, InvitationSecret, Member, Membership, Message,
};
use matters_sync::ContentKey;
use rusqlite::params;

use super::db::Database;
use super::membership::{Joined, SyncStanding};
use super::sealing::random32;
use crate::error::{MattersError, Result};

/// A control or envelope this device signed for others, to publish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outgoing {
    pub seq: i64,
    pub bytes: Vec<u8>,
}

fn not_admin() -> MattersError {
    MattersError::Validation("only the collection's admin can change its membership".into())
}

impl Joined {
    fn require_admin(&self) -> Result<()> {
        if self.standing() != SyncStanding::Active || self.authority.admin()? != self.device.id() {
            return Err(not_admin());
        }
        Ok(())
    }
}

impl Database {
    /// A one-time invitation into this collection, for the admin to show
    /// as a QR code or as a code and offer. Not stored: show it once.
    pub fn invite(&self) -> Result<Invitation> {
        self.with_sealing(|_, _, sync| {
            let joined = sync.membership.as_ref().ok_or_else(not_resumed)?;
            joined.require_admin()?;
            let mut secret = [0; 16];
            secret.copy_from_slice(&random32()[..16]);
            Ok(joined.authority.invite(InvitationSecret::new(secret))?)
        })
    }

    /// Admit a new device's enrollment request made under `invitation`: a
    /// new epoch with the device in the roster, its envelopes and its
    /// retained history, all committed and queued for publishing.
    pub fn admit(&self, request: &[u8], invitation: &Invitation) -> Result<()> {
        self.change_membership(|joined| {
            let member = joined.authority.accept_enrollment(request, invitation)?;
            let id = member.id;
            let mut roster = joined.authority.members()?;
            roster.push(member);
            Ok((roster, Some(id)))
        })
    }

    /// Remove `device` from the collection: a new epoch whose key it never
    /// receives. The admin cannot remove itself.
    pub fn remove_member(&self, device: &Hash) -> Result<()> {
        self.change_membership(|joined| {
            if *device == joined.device.id() {
                return Err(MattersError::Validation(
                    "the admin cannot remove itself".into(),
                ));
            }
            let mut roster = joined.authority.members()?;
            let before = roster.len();
            roster.retain(|m| m.id != *device);
            if roster.len() == before {
                return Err(MattersError::NotFound {
                    entity: "member",
                    id: hex(device),
                });
            }
            Ok((roster, None))
        })
    }

    /// Start a new epoch with the same members and a fresh content key.
    pub fn rotate_epoch(&self) -> Result<()> {
        self.change_membership(|joined| Ok((joined.authority.members()?, None)))
    }

    /// Everything this device signed for others and not yet acknowledged,
    /// oldest first: exactly the bytes first stored.
    pub fn outgoing_controls(&self) -> Result<Vec<Outgoing>> {
        self.with_conn(|conn| {
            let mut stmt =
                conn.prepare("SELECT seq, bytes FROM sync_publish WHERE acked = 0 ORDER BY seq")?;
            let rows = stmt.query_map([], |r| {
                Ok(Outgoing {
                    seq: r.get(0)?,
                    bytes: r.get(1)?,
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
    }

    /// Mark one outgoing control as durably received by the relay.
    pub fn acknowledge_control(&self, seq: i64) -> Result<()> {
        self.with_sealing(|conn, _, _| {
            let changed = conn.execute(
                "UPDATE sync_publish SET acked = 1 WHERE seq = ?1",
                params![seq],
            )?;
            if changed == 0 {
                return Err(MattersError::NotFound {
                    entity: "outgoing control",
                    id: seq.to_string(),
                });
            }
            Ok(())
        })
    }

    /// Sign the next transition to the roster `change` returns, with a
    /// fresh content key; wrap it for every member and the recovery key,
    /// and, for a new member, every earlier epoch's key; commit it all
    /// with this device's own envelope, then seal under the new epoch.
    fn change_membership(
        &self,
        change: impl FnOnce(&Joined) -> Result<(Vec<Member>, Option<Hash>)>,
    ) -> Result<()> {
        self.with_sealing(|conn, engine, sync| {
            let joined = sync.membership.as_ref().ok_or_else(not_resumed)?;
            joined.require_admin()?;
            let (mut roster, newcomer) = change(joined)?;
            roster.sort_by_key(|m| m.id);
            let a = &joined.authority;
            let device = &joined.device;
            let key = ContentKey::new(random32());
            let generation = a.generation()?;
            let epoch = a.epoch()?.checked_add(1).ok_or_else(exhausted)?;
            let message = Message {
                route: joined.route.clone(),
                signer: device.id(),
                body: Body::Membership(Membership {
                    generation,
                    sequence: a.sequence()?.checked_add(1).ok_or_else(exhausted)?,
                    previous: a.manifest(),
                    roster,
                    admin: device.id(),
                    epoch,
                    recovery_key: a.recovery()?,
                    key_commitment: authority::key_commitment(
                        &joined.route,
                        generation,
                        epoch,
                        &key,
                    )?,
                }),
            };
            let transition = authority::sign(&message, device.signing_key(), random32())?;
            let mut next = a.clone();
            next.apply_membership(&transition)?;
            let manifest = next.manifest();
            let mut own = None;
            let mut publish = vec![transition.clone()];
            for member in next.members()? {
                let envelope = next.wrap_key(device, member.id, &key, random32())?;
                if member.id == device.id() {
                    own = Some(envelope);
                } else {
                    publish.push(envelope);
                }
            }
            publish.push(next.wrap_recovery_key(device, manifest, &key, random32())?);
            if let Some(newcomer) = newcomer {
                for (old, envelope) in own_envelopes(conn)? {
                    let earlier = next.unwrap_history_key(&envelope, device)?;
                    publish.push(next.wrap_history_key(
                        device,
                        newcomer,
                        old,
                        earlier.content_key(),
                        random32(),
                    )?);
                }
            }
            let own = own.ok_or_else(not_admin)?;
            let tx = conn.transaction()?;
            tx.execute(
                "INSERT INTO sync_controls (bytes) VALUES (?1)",
                params![transition],
            )?;
            tx.execute(
                "INSERT INTO sync_envelopes (manifest, bytes) VALUES (?1, ?2)",
                params![manifest.as_slice(), own],
            )?;
            for bytes in &publish {
                tx.execute(
                    "INSERT INTO sync_publish (bytes) VALUES (?1)",
                    params![bytes],
                )?;
            }
            super::sync::crash_point("membership-before-commit");
            tx.commit()?;
            let mut joined = sync.membership.take().ok_or_else(not_resumed)?;
            joined.authority = next;
            let actor = engine.as_ref().map_or(0, |d| d.peer());
            sync.settle(conn, actor, joined)?;
            Ok(())
        })
    }
}

/// This device's own stored envelopes, by manifest.
fn own_envelopes(conn: &rusqlite::Connection) -> Result<Vec<(Hash, Vec<u8>)>> {
    let mut stmt = conn.prepare("SELECT manifest, bytes FROM sync_envelopes ORDER BY rowid")?;
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
}

fn not_resumed() -> MattersError {
    MattersError::Validation("sync is not resumed".into())
}

fn exhausted() -> MattersError {
    MattersError::Validation("the collection's sequence is exhausted".into())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(feature = "test-support")]
pub use joining::Recovered;

#[cfg(feature = "test-support")]
mod joining {
    use super::*;
    use crate::storage::membership::{DeviceSeeds, KeyStore, HOME_DOCUMENT};
    use matters_sync::authority::{Authority, Route};
    use rusqlite::OptionalExtension;

    /// What a recovery restored, and what it could not.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Recovered {
        pub standing: SyncStanding,
        /// Manifests of earlier epochs with no recovery envelope: their
        /// records cannot be read on this device, and nothing can bring
        /// their keys back.
        pub unrecoverable: Vec<Hash>,
    }

    /// A stored enrollment: application, tenant, collection, root, request.
    type Pending = (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>);

    impl Database {
        /// A new device's first step: generate its keys and sign a request
        /// to join under `invitation`, for the admin. Kept until the
        /// admin's transition arrives; asking again returns the same bytes.
        /// The database must not sync yet and hold no rows (#41).
        pub fn start_enrollment(
            &self,
            store: &dyn KeyStore,
            invitation: &Invitation,
        ) -> Result<Vec<u8>> {
            self.with_sealing(|conn, engine, _| {
                if engine.is_some() {
                    return Err(MattersError::Validation("sync is already on".into()));
                }
                let pending: Option<Pending> = conn
                    .query_row(
                        "SELECT application, tenant, collection, root, request FROM sync_enrolling",
                        [],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                    )
                    .optional()?;
                if let Some((application, tenant, collection, root, request)) = pending {
                    let route = &invitation.route;
                    if application != route.application
                        || tenant != route.tenant
                        || collection != route.collection
                        || root != invitation.root
                    {
                        return Err(MattersError::Validation(
                            "this device is already joining another collection".into(),
                        ));
                    }
                    return Ok(request);
                }
                let seeds = DeviceSeeds::generate();
                let device = seeds.keys()?;
                let request = authority::request_enrollment(invitation, &device, random32())?;
                let key_name = format!("matters-sync/device/{}", uuid::Uuid::new_v4());
                store.store(&key_name, &seeds)?;
                let route = &invitation.route;
                conn.execute(
                    "INSERT INTO sync_enrolling (singleton, application, tenant, collection,
                         root, device, key_name, request)
                     VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        route.application.as_slice(),
                        route.tenant.as_slice(),
                        route.collection.as_slice(),
                        invitation.root.as_slice(),
                        device.id().as_slice(),
                        key_name,
                        request
                    ],
                )?;
                Ok(request)
            })
        }

        /// Recover the collection on this new device after every device is
        /// lost: from the 24 `words`, the collection's control records as
        /// the relay holds them (genesis first) and its recovery envelopes.
        /// Opens every epoch key it can, then signs the root recovery that
        /// makes this device the sole admin of the next generation, keeps
        /// its own envelopes of the new and the recovered keys, and queues
        /// the recovery and its recovery envelope for publishing. Wrong
        /// words fail before anything is stored. The database must not
        /// sync yet and hold no rows (#41).
        pub fn recover_collection(
            &self,
            store: &dyn KeyStore,
            words: &str,
            route: Route,
            controls: &[Vec<u8>],
            recovery_envelopes: &[Vec<u8>],
        ) -> Result<Recovered> {
            self.with_sealing(|conn, engine, sync| {
                if engine.is_some() {
                    return Err(MattersError::Validation("sync is already on".into()));
                }
                let keys = authority::RecoverySecret::from_words(words)?.keys(&route)?;
                let (genesis, rest) = controls
                    .split_first()
                    .ok_or_else(|| MattersError::Validation("no genesis".into()))?;
                let mut authority = Authority::bootstrap(genesis, route.clone(), keys.root_key())?;
                let mut epochs = vec![authority.manifest()];
                for control in rest {
                    let before = authority.manifest();
                    match super::super::membership::apply(&mut authority, control) {
                        Ok(_) | Err(authority::Error::Fork) => {}
                        Err(error) => return Err(error.into()),
                    }
                    // Writer claims change no epoch; transitions do.
                    if authority.manifest() != before {
                        epochs.push(authority.manifest());
                    }
                }
                let mut recovered = Vec::new();
                for envelope in recovery_envelopes {
                    let key = authority.unwrap_recovery_key(envelope, &keys)?;
                    recovered.push((key.manifest(), key.into_content_key()));
                }
                let seeds = DeviceSeeds::generate();
                let device = seeds.keys()?;
                let first = ContentKey::new(random32());
                let recovery = authority.recover(&keys, &device, &first, random32())?;
                authority.apply_recovery(&recovery)?;
                let manifest = authority.manifest();
                let own = authority.wrap_key(&device, device.id(), &first, random32())?;
                let mut history = Vec::new();
                for (old, key) in &recovered {
                    history.push((
                        *old,
                        authority.wrap_history_key(&device, device.id(), *old, key, random32())?,
                    ));
                }
                let recovery_envelope =
                    authority.wrap_recovery_key(&device, manifest, &first, random32())?;
                let unrecoverable: Vec<Hash> = epochs
                    .into_iter()
                    .filter(|m| !recovered.iter().any(|(r, _)| r == m))
                    .collect();
                let key_name = format!("matters-sync/device/{}", uuid::Uuid::new_v4());
                store.store(&key_name, &seeds)?;
                let tx = conn.transaction()?;
                let doc = super::super::sync::turn_on(&tx)?;
                tx.execute(
                    "INSERT INTO sync_device (singleton, application, tenant, collection,
                         document, root, device, key_name)
                     VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        route.application.as_slice(),
                        route.tenant.as_slice(),
                        route.collection.as_slice(),
                        HOME_DOCUMENT.as_slice(),
                        authority.root().as_slice(),
                        device.id().as_slice(),
                        key_name
                    ],
                )?;
                for control in controls.iter().chain([&recovery]) {
                    tx.execute(
                        "INSERT OR IGNORE INTO sync_controls (bytes) VALUES (?1)",
                        params![control],
                    )?;
                }
                for (m, envelope) in history.iter().chain([&(manifest, own)]) {
                    tx.execute(
                        "INSERT INTO sync_envelopes (manifest, bytes) VALUES (?1, ?2)",
                        params![m.as_slice(), envelope],
                    )?;
                }
                for bytes in [&recovery, &recovery_envelope] {
                    tx.execute(
                        "INSERT INTO sync_publish (bytes) VALUES (?1)",
                        params![bytes],
                    )?;
                }
                super::super::sync::crash_point("recovery-before-commit");
                tx.commit()?;
                let actor = doc.peer();
                *engine = Some(doc);
                let standing = sync.settle(
                    conn,
                    actor,
                    Joined {
                        route,
                        document: HOME_DOCUMENT,
                        seeds,
                        device,
                        authority,
                    },
                )?;
                Ok(Recovered {
                    standing,
                    unrecoverable,
                })
            })
        }

        /// A new device's last step: the collection's control records,
        /// genesis first, ending with the admin's transition that adds this
        /// device, and the envelopes the admin wrapped for it (current
        /// epoch and history). Every envelope must open for this device.
        /// Turns sync on and starts sealing, in one transaction.
        pub fn finish_enrollment(
            &self,
            store: &dyn KeyStore,
            controls: &[Vec<u8>],
            envelopes: &[Vec<u8>],
        ) -> Result<SyncStanding> {
            self.with_sealing(|conn, engine, sync| {
                if engine.is_some() {
                    return Err(MattersError::Validation("sync is already on".into()));
                }
                let (application, tenant, collection, root, key_name): (
                    Vec<u8>,
                    Vec<u8>,
                    Vec<u8>,
                    Vec<u8>,
                    String,
                ) = conn
                    .query_row(
                        "SELECT application, tenant, collection, root, key_name FROM sync_enrolling",
                        [],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                    )
                    .optional()?
                    .ok_or_else(|| {
                        MattersError::Validation("this device has not started enrolling".into())
                    })?;
                let stored = || MattersError::Validation("stored enrollment".into());
                let route = Route {
                    application: application.try_into().map_err(|_| stored())?,
                    tenant: tenant.try_into().map_err(|_| stored())?,
                    collection: collection.try_into().map_err(|_| stored())?,
                };
                let root: Hash = root.try_into().map_err(|_| stored())?;
                let seeds = store.load(&key_name)?.ok_or_else(|| {
                    MattersError::Validation(
                        "this device's sync keys are missing from the keyring; enroll again".into(),
                    )
                })?;
                let device = seeds.keys()?;
                let (genesis, rest) = controls
                    .split_first()
                    .ok_or_else(|| MattersError::Validation("no genesis".into()))?;
                let mut authority = Authority::bootstrap_by_root_id(genesis, route.clone(), root)?;
                for control in rest {
                    match super::super::membership::apply(&mut authority, control) {
                        Ok(_) | Err(authority::Error::Fork) => {}
                        Err(error) => return Err(error.into()),
                    }
                }
                if !authority
                    .members()?
                    .iter()
                    .any(|m| m.id == device.id())
                {
                    return Err(MattersError::Validation(
                        "the admin has not added this device yet".into(),
                    ));
                }
                let mut opened = Vec::new();
                for envelope in envelopes {
                    let key = authority.unwrap_history_key(envelope, &device)?;
                    opened.push((key.manifest(), envelope));
                }
                if !opened.iter().any(|(m, _)| *m == authority.manifest()) {
                    return Err(MattersError::Validation(
                        "no key envelope for the current epoch".into(),
                    ));
                }
                let tx = conn.transaction()?;
                let doc = super::super::sync::turn_on(&tx)?;
                tx.execute(
                    "INSERT INTO sync_device (singleton, application, tenant, collection,
                         document, root, device, key_name)
                     VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        route.application.as_slice(),
                        route.tenant.as_slice(),
                        route.collection.as_slice(),
                        HOME_DOCUMENT.as_slice(),
                        root.as_slice(),
                        device.id().as_slice(),
                        key_name
                    ],
                )?;
                for control in controls {
                    tx.execute(
                        "INSERT INTO sync_controls (bytes) VALUES (?1)",
                        params![control],
                    )?;
                }
                for (manifest, envelope) in &opened {
                    tx.execute(
                        "INSERT OR REPLACE INTO sync_envelopes (manifest, bytes) VALUES (?1, ?2)",
                        params![manifest.as_slice(), envelope],
                    )?;
                }
                tx.execute("DELETE FROM sync_enrolling", [])?;
                super::super::sync::crash_point("enrollment-before-commit");
                tx.commit()?;
                let actor = doc.peer();
                *engine = Some(doc);
                sync.settle(
                    conn,
                    actor,
                    Joined {
                        route,
                        document: HOME_DOCUMENT,
                        seeds,
                        device,
                        authority,
                    },
                )
            })
        }
    }
}
