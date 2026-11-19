//! Single-file domain + outbox experiment. Keys stay with trusted callbacks.
use crate::encrypted_outbox::{Binding, Outbox, Point, RecoveryIntent, Stored, Writer};
use crate::record_chain::Id;
use crate::support::Message;
use matters_sync_prototype::{
    wire, Fault, LocalOutbox, Projection, Record, Replica, Request, Result,
};
use rusqlite::Transaction;
use std::path::Path;

/// Injective encoding of a canonical local UUID into the 32-byte batch slot.
/// The zero suffix is padding, not entropy; this is not a record/key/nonce ID.
pub fn batch(request: &Request) -> Result<Id> {
    let mut id = [0; 32];
    id[..16].copy_from_slice(uuid::Uuid::parse_str(&request.id)?.as_bytes());
    Ok(id)
}
struct Participant {
    binding: Binding,
    writer: Writer,
}
impl LocalOutbox for Participant {
    fn actor(&self) -> u64 {
        self.writer.actor
    }
    fn check_owner(&self, tx: &Transaction<'_>) -> Result<()> {
        Outbox::check_owner(tx, &self.binding, self.writer)
    }
    fn enqueue(&self, tx: &Transaction<'_>, record: &Record) -> Result<()> {
        Outbox::require_reconciled(tx, self.writer)?;
        let request = Request {
            id: record.id.clone(),
            commands: vec![],
        };
        Outbox::enqueue_on(tx, self.writer, batch(&request)?, &record.bytes)?;
        Ok(())
    }
}
pub struct Sender {
    replica: Replica,
    outbox: Outbox,
}
impl Sender {
    pub fn open(
        path: &Path,
        binding: Binding,
        random: impl FnMut() -> Result<Id>,
        sign: impl FnOnce(&Message) -> Result<Vec<u8>>,
        mut hook: impl FnMut(Point) -> Result<()>,
    ) -> Result<Self> {
        let mut writer = None;
        let replica = Replica::open_with_outbox(path, |tx, exists| {
            let next = Outbox::start_writer(tx, &binding, random, Some(sign), exists)?;
            hook(Point::BeforeWriterCommit)?;
            writer = Some(next);
            Ok(Box::new(Participant {
                binding: binding.clone(),
                writer: next,
            }))
        })?;
        hook(Point::AfterWriterCommit)?;
        let outbox = Outbox::attach(path, binding, writer.expect("setup committed"))?;
        Ok(Self { replica, outbox })
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
