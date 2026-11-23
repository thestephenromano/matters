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
    self, Authority, Body, Closing, Hash, Id, Invitation, InvitationSecret, Member, Membership,
    Message, SignedRecord, MAX_FRONTIER,
};
use matters_sync::ContentKey;
use rusqlite::{params, Connection, OptionalExtension};

use super::db::Database;
use super::membership::{Joined, SyncStanding, HOME_DOCUMENT};
use super::sealing::{random, random32};
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
            // 128 bits, all of them from the OS.
            Ok(joined.authority.invite(InvitationSecret::new(random()))?)
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
        self.with_sealing(|conn, _, sync| {
            let joined = sync.membership.as_ref().ok_or_else(not_resumed)?;
            joined.require_admin()?;
            let (mut roster, newcomer) = change(joined)?;
            roster.sort_by_key(|m| m.id);
            let a = &joined.authority;
            let device = &joined.device;
            let leaving: Vec<Hash> = a
                .members()?
                .iter()
                .map(|m| m.id)
                .filter(|id| !roster.iter().any(|m| m.id == *id))
                .collect();
            let keep: Vec<Hash> = roster.iter().map(|m| m.id).collect();
            let frontier = closing_frontier(conn, a, &keep, &leaving, &[])?;
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
                    frontier,
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
            sync.settle(conn, joined)?;
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

/// The closing frontier a transition to the roster `keep` signs (#45,
/// docs/SYNC_KEY_DISTRIBUTION.md "Revocation"): each writer of a device
/// outside `keep` that no signed prefix closes yet, closed at the exact
/// head of the chain this device holds. A writer of a device in `leaving`
/// (removed by this transition) is always closed, at null if nothing of it
/// is held. Any other, a writer a recovery left open, is closed only once
/// something of it is held, so that nothing is closed before this device
/// has read it. Writers past the frontier budget wait for the next
/// transition.
pub(super) fn closing_frontier(
    conn: &Connection,
    authority: &Authority,
    keep: &[Hash],
    leaving: &[Hash],
    records: &[Vec<u8>],
) -> Result<Vec<Closing>> {
    let held = held_records(conn, authority, records)?;
    let mut frontier = Vec::new();
    for (device, incarnation) in authority.unclosed_writers(HOME_DOCUMENT, keep) {
        let head = held_head(conn, &held, &incarnation)?;
        if head.is_some() || leaving.contains(&device) {
            frontier.push(Closing {
                device,
                document: HOME_DOCUMENT,
                incarnation,
                head,
            });
        }
    }
    within_budget(frontier, leaving)
}

/// At most `MAX_FRONTIER` closings, the leaving devices' first, so an
/// overflow defers only writers a recovery left open, to the next
/// transition. More leaving writers than the budget refuse the transition.
fn within_budget(mut frontier: Vec<Closing>, leaving: &[Hash]) -> Result<Vec<Closing>> {
    frontier.sort_by_key(|c| !leaving.contains(&c.device));
    if frontier
        .iter()
        .filter(|c| leaving.contains(&c.device))
        .count()
        > MAX_FRONTIER
    {
        return Err(MattersError::Validation(
            "too many writers to close in one transition".into(),
        ));
    }
    frontier.truncate(MAX_FRONTIER);
    frontier.sort();
    Ok(frontier)
}

/// The signed content records this device holds but has not applied: the
/// ones it keeps waiting, and `records` supplied by the caller (a
/// recovering device's), each verified as signed by its author.
fn held_records(
    conn: &Connection,
    authority: &Authority,
    records: &[Vec<u8>],
) -> Result<Vec<SignedRecord>> {
    let mut stmt = conn.prepare("SELECT bytes FROM sync_pending")?;
    let pending = stmt
        .query_map([], |r| r.get::<_, Vec<u8>>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut held: Vec<SignedRecord> = pending
        .iter()
        .chain(records)
        .filter_map(|bytes| authority.verify_signed_record(bytes, &HOME_DOCUMENT).ok())
        .collect();
    held.sort_by_key(|r| r.hash);
    held.dedup_by_key(|r| r.hash);
    Ok(held)
}

/// The head of `incarnation`'s chain this device holds: what it applied
/// (or the checkpoint it started from covered), followed through the held
/// records while each step has exactly one signed successor.
fn held_head(conn: &Connection, held: &[SignedRecord], incarnation: &Id) -> Result<Option<Hash>> {
    let mut stored: Option<Vec<u8>> = None;
    for sql in [
        "SELECT head FROM sync_receive_heads WHERE incarnation = ?1",
        "SELECT base_head FROM sync_receive_base WHERE incarnation = ?1",
        "SELECT head FROM sync_writers WHERE incarnation = ?1 AND head IS NOT NULL",
    ] {
        stored = conn
            .query_row(sql, params![incarnation.as_slice()], |r| r.get(0))
            .optional()?;
        if stored.is_some() {
            break;
        }
    }
    let mut head: Option<Hash> = stored
        .map(|h| h.try_into())
        .transpose()
        .map_err(|_| MattersError::Validation("stored chain head".into()))?;
    // Each step moves to a held record further along the chain, so the walk
    // takes at most one step per held record.
    for _ in 0..held.len() {
        let mut next = held
            .iter()
            .filter(|r| &r.incarnation == incarnation && r.previous == head);
        match (next.next(), next.next()) {
            (Some(only), None) => head = Some(only.hash),
            _ => break,
        }
    }
    Ok(head)
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

pub use joining::Recovered;

mod joining {
    use super::*;
    use crate::storage::membership::{DeviceSeeds, KeyStore};
    use matters_sync::authority::Route;

    /// What a recovery restored, and what it could not.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Recovered {
        pub standing: SyncStanding,
        /// Manifests of earlier epochs with no recovery envelope: their
        /// records cannot be read on this device, and nothing can bring
        /// their keys back.
        pub unrecoverable: Vec<Hash>,
    }

    /// A stored enrollment's application, tenant, collection and device.
    type EnrollingRow = (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>);

    /// A stored enrollment: application, tenant, collection, root, request,
    /// key name.
    type Pending = (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>, String);

    /// Whether two enrollment requests answer the same invitation: the
    /// same manifest and binding (the binding is an HMAC under the
    /// invitation's secret over the device's keys, so it is the same for a
    /// re-signed request and differs for another invitation).
    fn same_invitation(a: &[u8], b: &[u8]) -> Result<bool> {
        let facts = |bytes: &[u8]| -> Result<(Hash, Hash)> {
            match authority::parse(bytes)?.message.body {
                Body::EnrollmentRequest(r) => Ok((r.manifest, r.binding)),
                _ => Err(MattersError::Validation("stored enrollment".into())),
            }
        };
        Ok(facts(a)? == facts(b)?)
    }

    /// Replay a received control log, genesis first, keeping what it
    /// accepts; see [`replay_into`](super::super::membership::replay_into).
    /// Returns the authority, the controls to store (genesis included) and
    /// the manifest of each epoch in order.
    fn replay_received(
        mut authority: Authority,
        controls: &[Vec<u8>],
    ) -> (Authority, Vec<Vec<u8>>, Vec<Hash>) {
        let mut epochs = vec![authority.manifest()];
        let mut kept = controls[..1].to_vec();
        for control in &controls[1..] {
            let before = authority.manifest();
            if !super::super::membership::replay_into(&mut authority, [control]).is_empty() {
                kept.push(control.clone());
            }
            // Writer claims change no epoch; transitions do.
            if authority.manifest() != before {
                epochs.push(authority.manifest());
            }
        }
        (authority, kept, epochs)
    }

    impl Database {
        /// A new device's first step: generate its keys and sign a request
        /// to join under `invitation`, for the admin. Kept until the
        /// admin's transition arrives; asking again under the same
        /// invitation returns the same bytes. A new invitation into the
        /// same collection (another secret or manifest, say after the
        /// first one was missed) replaces the request, under the same
        /// device keys (#45). One into another collection is refused until
        /// [`Self::cancel_enrollment`].
        /// The database must not sync yet and hold no rows: a device with
        /// its own data sets it aside first (`set_aside_local_data`) and
        /// imports it once joined (`import_missing_from`, #41).
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
                        "SELECT application, tenant, collection, root, request, key_name
                         FROM sync_enrolling",
                        [],
                        |r| {
                            Ok((
                                r.get(0)?,
                                r.get(1)?,
                                r.get(2)?,
                                r.get(3)?,
                                r.get(4)?,
                                r.get(5)?,
                            ))
                        },
                    )
                    .optional()?;
                if let Some((application, tenant, collection, root, request, key_name)) = pending {
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
                    let device = store
                        .load(&key_name)?
                        .ok_or_else(|| {
                            MattersError::Validation(
                                "this device's sync keys are missing from the keyring; \
                                 cancel and enroll again"
                                    .into(),
                            )
                        })?
                        .keys()?;
                    let fresh = authority::request_enrollment(invitation, &device, random32())?;
                    if same_invitation(&request, &fresh)? {
                        return Ok(request);
                    }
                    conn.execute("UPDATE sync_enrolling SET request = ?1", params![fresh])?;
                    return Ok(fresh);
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
        /// sync yet and hold no rows, as for joining (#41).
        pub fn recover_collection(
            &self,
            store: &dyn KeyStore,
            words: &str,
            route: Route,
            controls: &[Vec<u8>],
            recovery_envelopes: &[Vec<u8>],
        ) -> Result<Recovered> {
            self.recover_collection_holding(store, words, route, controls, recovery_envelopes, &[])
        }

        /// As [`Self::recover_collection`], also given the content
        /// `records` the relay holds. The root recovery then closes every
        /// writer of the superseded generation at the exact head of the
        /// chain those records hold (null where they hold nothing of it),
        /// so a lost device's later records are never admitted (#45). With
        /// no records, the recovery closes nothing yet: the old writers'
        /// records wait until this device, as admin, signs its next
        /// transition (`rotate_epoch`, or an admission) once it has read
        /// them, which closes each at what it then holds.
        ///
        /// Controls and envelopes that fail verification are skipped, not
        /// fatal: one forged object cannot block a recovery.
        pub fn recover_collection_holding(
            &self,
            store: &dyn KeyStore,
            words: &str,
            route: Route,
            controls: &[Vec<u8>],
            recovery_envelopes: &[Vec<u8>],
            records: &[Vec<u8>],
        ) -> Result<Recovered> {
            self.with_sealing(|conn, engine, sync| {
                if engine.is_some() {
                    return Err(MattersError::Validation("sync is already on".into()));
                }
                let keys = authority::RecoverySecret::from_words(words)?.keys(&route)?;
                let genesis = controls
                    .first()
                    .ok_or_else(|| MattersError::Validation("no genesis".into()))?;
                let (mut authority, kept, epochs) = replay_received(
                    Authority::bootstrap(genesis, route.clone(), keys.root_key())?,
                    controls,
                );
                // Every candidate is tried; one that does not open (forged,
                // misaddressed or of an unknown epoch) is skipped.
                let mut recovered: Vec<(Hash, ContentKey)> = Vec::new();
                for envelope in recovery_envelopes {
                    let Ok(key) = authority.unwrap_recovery_key(envelope, &keys) else {
                        continue;
                    };
                    if !recovered.iter().any(|(m, _)| *m == key.manifest()) {
                        recovered.push((key.manifest(), key.into_content_key()));
                    }
                }
                // The superseded generation's writers, closed at what this
                // device holds of them.
                let everyone: Vec<Hash> = if records.is_empty() {
                    Vec::new()
                } else {
                    authority
                        .unclosed_writers(HOME_DOCUMENT, &[])
                        .into_iter()
                        .map(|(device, _)| device)
                        .collect()
                };
                let frontier = closing_frontier(conn, &authority, &[], &everyone, records)?;
                let seeds = DeviceSeeds::generate();
                let device = seeds.keys()?;
                let first = ContentKey::new(random32());
                let recovery =
                    authority.recover_closing(&keys, &device, &first, frontier, random32())?;
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
                let doc = super::super::sync::turn_on_empty(&tx)?;
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
                for control in kept.iter().chain([&recovery]) {
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
                *engine = Some(doc);
                let standing = sync.settle(
                    conn,
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

        /// Give up the enrollment this device started and has not finished,
        /// so it can join another collection. Its keyring entry is left
        /// unused. Returns whether there was one.
        pub fn cancel_enrollment(&self) -> Result<bool> {
            self.with_sealing(|conn, engine, _| {
                if engine.is_some() {
                    return Err(MattersError::Validation("sync is already on".into()));
                }
                Ok(conn.execute("DELETE FROM sync_enrolling", [])? > 0)
            })
        }

        /// The route and device ID of the enrollment this device started
        /// and has not finished, if any.
        pub fn enrolling(&self) -> Result<Option<(Route, Hash)>> {
            self.with_conn(|conn| {
                let row: Option<EnrollingRow> = conn
                    .query_row(
                        "SELECT application, tenant, collection, device FROM sync_enrolling",
                        [],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                    )
                    .optional()?;
                let stored = || MattersError::Validation("stored enrollment".into());
                row.map(|(application, tenant, collection, device)| {
                    Ok((
                        Route {
                            application: application.try_into().map_err(|_| stored())?,
                            tenant: tenant.try_into().map_err(|_| stored())?,
                            collection: collection.try_into().map_err(|_| stored())?,
                        },
                        device.try_into().map_err(|_| stored())?,
                    ))
                })
                .transpose()
            })
        }

        /// Sign a relay session challenge as the joining device, with its
        /// keys from `store`. The relay grants the session only once the
        /// admin's transition adding this device is published.
        pub fn sign_enrolling_session(
            &self,
            store: &dyn KeyStore,
            challenge: &[u8; 32],
        ) -> Result<[u8; matters_sync::wire::SIGNATURE_BYTES]> {
            let (route, _) = self.enrolling()?.ok_or_else(|| {
                MattersError::Validation("this device has not started enrolling".into())
            })?;
            let key_name: String = self.with_conn(|conn| {
                Ok(conn.query_row("SELECT key_name FROM sync_enrolling", [], |r| r.get(0))?)
            })?;
            let device = store
                .load(&key_name)?
                .ok_or_else(|| {
                    MattersError::Validation(
                        "this device's sync keys are missing from the keyring; enroll again".into(),
                    )
                })?
                .keys()?;
            Ok(authority::sign_session(
                &device,
                &route,
                challenge,
                random32(),
            )?)
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
                let genesis = controls
                    .first()
                    .ok_or_else(|| MattersError::Validation("no genesis".into()))?;
                // A control that fails verification is skipped, not fatal:
                // one forged claim cannot keep a device from joining.
                let (authority, kept, _) = replay_received(
                    Authority::bootstrap_by_root_id(genesis, route.clone(), root)?,
                    controls,
                );
                if !authority
                    .members()?
                    .iter()
                    .any(|m| m.id == device.id())
                {
                    return Err(MattersError::Validation(
                        "the admin has not added this device yet".into(),
                    ));
                }
                // Every candidate is tried; one that does not open for this
                // device (forged, or another's) is skipped.
                let mut opened: Vec<(Hash, &Vec<u8>)> = Vec::new();
                for envelope in envelopes {
                    let Ok(key) = authority.unwrap_history_key(envelope, &device) else {
                        continue;
                    };
                    if !opened.iter().any(|(m, _)| *m == key.manifest()) {
                        opened.push((key.manifest(), envelope));
                    }
                }
                if !opened.iter().any(|(m, _)| *m == authority.manifest()) {
                    return Err(MattersError::Validation(
                        "no key envelope for the current epoch".into(),
                    ));
                }
                let tx = conn.transaction()?;
                let doc = super::super::sync::turn_on_empty(&tx)?;
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
                for control in &kept {
                    tx.execute(
                        "INSERT OR IGNORE INTO sync_controls (bytes) VALUES (?1)",
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
                *engine = Some(doc);
                sync.settle(
                    conn,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn closing(device: u8, n: u8) -> Closing {
        Closing {
            device: [device; 48],
            document: HOME_DOCUMENT,
            incarnation: [n; 32],
            head: None,
        }
    }

    fn record(incarnation: u8, previous: Option<u8>, hash: u8) -> SignedRecord {
        SignedRecord {
            author: [9; 48],
            incarnation: [incarnation; 32],
            previous: previous.map(|p| [p; 48]),
            hash: [hash; 48],
        }
    }

    /// The held head follows the chain from what this device applied, one
    /// signed successor at a time, and stops at a gap, a fork or another
    /// writer's records.
    #[test]
    fn the_held_head_follows_only_this_writers_unbroken_chain() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE sync_receive_heads (incarnation BLOB, head BLOB);
             CREATE TABLE sync_receive_base (incarnation BLOB, base_head BLOB);
             CREATE TABLE sync_writers (incarnation BLOB, head BLOB);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sync_receive_heads VALUES (?1, ?2)",
            params![[1u8; 32].as_slice(), [10u8; 48].as_slice()],
        )
        .unwrap();
        let writer = [1u8; 32];
        let head = |held: &[SignedRecord]| held_head(&conn, held, &writer).unwrap();

        assert_eq!(head(&[]), Some([10; 48]), "nothing held: the applied head");
        let chain = [record(1, Some(10), 11), record(1, Some(11), 12)];
        assert_eq!(head(&chain), Some([12; 48]), "followed to its end");
        assert_eq!(
            head(&[record(1, Some(11), 12)]),
            Some([10; 48]),
            "a gap stops the walk"
        );
        assert_eq!(
            head(&[record(1, Some(10), 11), record(1, Some(10), 13)]),
            Some([10; 48]),
            "a fork stops the walk"
        );
        assert_eq!(
            head(&[record(2, Some(10), 11)]),
            Some([10; 48]),
            "another writer's record is not followed"
        );
        let other = [2u8; 32];
        assert_eq!(
            held_head(&conn, &[record(2, None, 20)], &other).unwrap(),
            Some([20; 48]),
            "a writer with nothing applied starts from its first record"
        );
    }

    /// The budget keeps every leaving device's closing, deferring only
    /// writers a recovery left open, and refuses one leaving writer more
    /// than it holds.
    #[test]
    fn the_frontier_budget_keeps_leaving_writers_first() {
        let leaving = [[1; 48]];
        let mut frontier: Vec<Closing> = (0..60).map(|n| closing(1, n)).collect();
        frontier.extend((0..10).map(|n| closing(2, n)));
        let kept = within_budget(frontier, &leaving).unwrap();
        assert_eq!(kept.len(), MAX_FRONTIER);
        assert_eq!(kept.iter().filter(|c| c.device == [1; 48]).count(), 60);
        assert!(kept.windows(2).all(|w| w[0] <= w[1]), "sorted");

        let exactly: Vec<Closing> = (0..MAX_FRONTIER as u8).map(|n| closing(1, n)).collect();
        assert_eq!(
            within_budget(exactly, &leaving).unwrap().len(),
            MAX_FRONTIER
        );
        let over: Vec<Closing> = (0..=MAX_FRONTIER as u8).map(|n| closing(1, n)).collect();
        assert!(matches!(
            within_budget(over, &leaving),
            Err(MattersError::Validation(_))
        ));
    }
}
