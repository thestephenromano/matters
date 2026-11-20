//! Single-file domain + outbox experiment. Keys stay with trusted callbacks.
use crate::encrypted_inbox::{Config, Entry, Inbox, Point as InPoint};
use crate::encrypted_outbox::{
    Binding, Composition, Outbox, Point, RecoveryIntent, Stored, Writer,
};
use crate::record_chain::Id;
use crate::support::{EpochKey, Message};
use matters_sync_prototype::{
    wire, Fault, LocalOutbox, Projection, Record, Replica, Request, Result,
};
use rusqlite::Transaction;
use std::path::Path;

/// Injective encoding of a canonical local request UUID into the 32-byte
/// batch slot. The zero suffix is padding, not entropy; this is not a
/// record/key/nonce ID.
pub fn batch(request_id: &str) -> Result<Id> {
    let mut id = [0; 32];
    id[..16].copy_from_slice(uuid::Uuid::parse_str(request_id)?.as_bytes());
    Ok(id)
}
/// The request a batch encodes ([`batch`] reversed).
fn request_id(batch: &Id) -> Result<String> {
    if batch[16..].iter().any(|b| *b != 0) {
        return Err(matters_sync_prototype::invalid(
            "batch does not encode a request",
        ));
    }
    Ok(uuid::Uuid::from_slice(&batch[..16])?.to_string())
}
struct Participant {
    writer: Writer,
}
impl LocalOutbox for Participant {
    fn actor(&self) -> u64 {
        self.writer.actor
    }
    fn check_owner(&self, tx: &Transaction<'_>) -> Result<()> {
        Outbox::check_owner(tx, self.writer)
    }
    fn enqueue(&self, tx: &Transaction<'_>, record: &Record) -> Result<()> {
        // Opening re-issues stranded work first, so this holds unless that
        // failed; new operations must not depend on unsendable ones.
        Outbox::require_reconciled(tx, self.writer)?;
        Outbox::enqueue_on(tx, self.writer, batch(&record.id)?, &record.bytes)?;
        Ok(())
    }
    fn stranded(&self, tx: &Transaction<'_>) -> Result<Vec<String>> {
        Outbox::stranded_on(tx, self.writer)?
            .iter()
            .map(request_id)
            .collect()
    }
    fn reissue(&self, tx: &Transaction<'_>, record: &Record) -> Result<()> {
        Outbox::reissue_on(tx, self.writer, batch(&record.id)?, &record.bytes)
    }
}
pub struct Sender {
    replica: Replica,
    outbox: Outbox,
    inbox: Option<Inbox>,
}
impl Sender {
    pub fn open(
        path: &Path,
        binding: Binding,
        random: impl FnMut() -> Result<Id>,
        sign: impl FnOnce(&Message) -> Result<Vec<u8>>,
        hook: impl FnMut(Point) -> Result<()>,
    ) -> Result<Self> {
        Self::open_inner(path, binding, None, random, sign, hook)
    }
    pub fn open_bidirectional(
        path: &Path,
        binding: Binding,
        config: Config,
        random: impl FnMut() -> Result<Id>,
        sign: impl FnOnce(&Message) -> Result<Vec<u8>>,
        hook: impl FnMut(Point) -> Result<()>,
    ) -> Result<Self> {
        let authority = config.authority()?;
        if !config.writers.is_empty()
            || config.route.app != binding.application
            || config.route.tenant != binding.tenant
            || config.route.collection != binding.collection
            || config.document != binding.document
            || authority.manifest().as_slice() != binding.manifest
            || authority.epoch()? != binding.epoch
            || !authority
                .members()?
                .iter()
                .any(|m| m.id.as_slice() == binding.author)
        {
            return Err(matters_sync_prototype::invalid(
                "sender/receiver authority binding mismatch",
            ));
        }
        Self::open_inner(path, binding, Some(config), random, sign, hook)
    }
    fn open_inner(
        path: &Path,
        binding: Binding,
        config: Option<Config>,
        random: impl FnMut() -> Result<Id>,
        sign: impl FnOnce(&Message) -> Result<Vec<u8>>,
        hook: impl FnMut(Point) -> Result<()>,
    ) -> Result<Self> {
        let mut writer = None;
        let hook = std::cell::RefCell::new(hook);
        let replica = Replica::open_with_outbox(
            path,
            |tx, exists| {
                let has_inbox: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='inbox_meta')",
                    [],
                    |r| r.get(0),
                )?;
                if exists && has_inbox != config.is_some() {
                    return Err(matters_sync_prototype::invalid(
                        "cannot change sender receive mode",
                    ));
                }
                if let Some(config) = &config {
                    Inbox::initialize_shared(tx, config)?;
                }
                let next = Outbox::start_writer(
                    tx,
                    &binding,
                    Composition::Atomic,
                    random,
                    Some(sign),
                    exists,
                )?;
                hook.borrow_mut()(Point::BeforeWriterCommit)?;
                writer = Some(next);
                Ok(Box::new(Participant { writer: next }))
            },
            // Right after the writer and its announcement commit, before the
            // rest of the open (projection, restore) can fail.
            || hook.borrow_mut()(Point::AfterWriterCommit),
        )?;
        let outbox = Outbox::attach(path, binding, writer.expect("setup committed"))?;
        let inbox = config
            .map(|config| Inbox::attach(path, config))
            .transpose()?;
        Ok(Self {
            replica,
            outbox,
            inbox,
        })
    }
    pub fn receive_writer(
        &mut self,
        bytes: &[u8],
        hook: impl FnMut(InPoint) -> Result<()>,
    ) -> Result<()> {
        self.inbox
            .as_mut()
            .ok_or_else(|| matters_sync_prototype::invalid("bidirectional sender required"))?
            .receive_writer(bytes, hook)
    }
    pub fn receive_record(
        &mut self,
        generation: Id,
        sequence: i64,
        bytes: &[u8],
        hook: impl FnMut(InPoint) -> Result<()>,
    ) -> Result<()> {
        self.inbox
            .as_mut()
            .ok_or_else(|| matters_sync_prototype::invalid("bidirectional sender required"))?
            .receive(generation, sequence, bytes, hook)
    }
    pub fn receive_cursor(&self) -> Result<i64> {
        self.inbox
            .as_ref()
            .ok_or_else(|| matters_sync_prototype::invalid("bidirectional sender required"))?
            .cursor()
    }
    pub fn received_entries(&self) -> Result<Vec<Entry>> {
        self.inbox
            .as_ref()
            .ok_or_else(|| matters_sync_prototype::invalid("bidirectional sender required"))?
            .entries()
    }
    pub fn process_incoming(
        &mut self,
        key: &EpochKey,
        fault: Fault,
        mut hook: impl FnMut(InPoint) -> Result<()>,
    ) -> Result<()> {
        let inbox = self
            .inbox
            .as_mut()
            .ok_or_else(|| matters_sync_prototype::invalid("bidirectional sender required"))?;
        let mut admitted = None;
        self.replica.apply_authenticated(
            |tx, doc| {
                let prepared = inbox.prepare_shared(tx, key, doc)?;
                admitted = Some(prepared.admitted);
                hook(InPoint::BeforeApply)?;
                Ok((prepared.doc, prepared.imports))
            },
            fault,
        )?;
        inbox.publish(admitted.expect("prepare completed"));
        hook(InPoint::AfterApply)
    }
    pub fn execute(&mut self, request: &Request, fault: Fault) -> Result<Record> {
        self.replica.execute_with_fault(request, fault)
    }
    pub fn projection(&self) -> Result<Projection> {
        self.replica.projection()
    }
    pub fn writer(&self) -> Writer {
        self.outbox.writer()
    }
    pub fn checkpoint(&mut self) -> Result<()> {
        self.replica.checkpoint()
    }
    pub fn rebuild(&mut self) -> Result<()> {
        self.replica.rebuild()
    }
    pub fn next_batch(&self) -> Result<Option<Id>> {
        self.outbox.next_batch()
    }
    pub fn recovery_required(&self) -> Result<Vec<RecoveryIntent>> {
        self.outbox.recovery_required()
    }
    pub fn seal(
        &mut self,
        batch: Id,
        random: impl FnMut() -> Result<Id>,
        seal: impl FnOnce(wire::Header<'_>, &[u8]) -> Result<Vec<u8>>,
        hook: impl FnMut(Point) -> Result<()>,
    ) -> Result<Stored> {
        self.outbox.seal(batch, random, seal, hook)
    }
}
