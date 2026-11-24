//! Untrusted engine bytes are decoded in a child process first (#45).
//!
//! Loro bounds neither the memory nor the processor time of a decode, and
//! no input cap supplies a bound: a member-signed update under 100 bytes
//! can decode to gigabytes (docs/SYNC_CHECKPOINTS.md, "Limits"). On stable
//! Rust a failed allocation aborts the process, so no bound can be kept
//! inside it, and an out-of-memory kill records nothing: the device would
//! meet the same record on every start.
//!
//! So before the receiver decodes a member's update or a checkpoint's
//! snapshot, a [`Checker`] decodes it the same way elsewhere. The one
//! production binaries install, [`ChildProcess`], re-executes the running
//! binary in check mode ([`run_if_requested`], called first thing in
//! `main`). The child limits its own address space (`RLIMIT_AS`) and
//! processor time (`RLIMIT_CPU`) for each blob, from a [`Budget`], and the
//! parent kills it past a wall-clock deadline. Only a blob that decoded
//! within its budget is decoded in this process. One that did not is
//! refused, for good, with a plain reason.
//!
//! The child does the receiver's own decoding ([`decode`]): an update's
//! metadata and its import into a scratch document; a snapshot's version,
//! its import, its projection, its whole state and every block of its
//! history, which Loro otherwise decodes lazily. It reports when each blob
//! is done, so when one stops the child, the blobs before it have passed
//! and the culprit is known: the rest go on in a new child.
//!
//! Matters core cannot know which executable to run, so the checker is
//! installed by the binary ([`install`]). With none installed, nothing
//! untrusted is decoded: receiving fails, as a failure of this device that
//! refuses no record, until one is. Builds with the `test-support` feature,
//! which no application enables, decode in-process by default instead
//! ([`InProcess`]).
//!
//! Verdicts are kept, by the hash of the bytes, for the life of the
//! process (up to [`CACHED`] of them): the receiver checks a page's
//! updates in one batch before their turn, and a record kept waiting is
//! not checked again when it applies.

use std::collections::{BTreeMap, VecDeque};
use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::Duration;

use rustix::process::{Resource, Rlimit, Signal};

use super::Document;
use crate::error::{MattersError, Result};

/// The argument that starts a binary in check mode, first after the
/// program name.
pub const CHECK_ARG: &str = "--matters-engine-check";

/// What a child in check mode writes first: the parent decodes nothing on
/// the word of a process that is not one.
pub const HELLO: &[u8] = b"matters-engine-check 1\n";

/// What the child writes after each blob it decoded within its budget.
const PASSED: u8 = b'+';

/// The exit status of a child that cannot check here: its arguments, its
/// limits or its pipes failed. That is no verdict on any blob.
const UNUSABLE: i32 = 3;

/// The verdicts kept, oldest forgotten first.
pub const CACHED: usize = 4096;

/// What the bytes are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Blob {
    /// An engine update from a data record.
    Update,
    /// A checkpoint's full-history snapshot.
    Snapshot,
}

impl Blob {
    fn as_arg(self) -> &'static str {
        match self {
            Blob::Update => "update",
            Blob::Snapshot => "snapshot",
        }
    }

    fn from_arg(arg: &OsStr) -> Option<Self> {
        match arg.to_str()? {
            "update" => Some(Blob::Update),
            "snapshot" => Some(Blob::Snapshot),
            _ => None,
        }
    }
}

/// A checker's answer for one blob. A refusal is final: the bytes never
/// decode in this process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    /// Why, in plain words.
    Refuse(String),
}

/// Decodes untrusted engine bytes as the receiver will, where running out
/// of memory or time cannot harm this process.
pub trait Checker: Send + Sync {
    /// One verdict for each blob, in order. `Err` means the blobs could not
    /// be checked, through no fault of theirs: the caller refuses nothing
    /// and tries again later.
    fn check(&self, kind: Blob, blobs: &[&[u8]]) -> Result<Vec<Verdict>>;
}

/// No isolation: every blob passes, and the receiver's own decoding runs
/// in this process without limits. For tests and tools, never for an
/// application.
pub struct InProcess;

impl Checker for InProcess {
    fn check(&self, _: Blob, blobs: &[&[u8]]) -> Result<Vec<Verdict>> {
        Ok(vec![Verdict::Pass; blobs.len()])
    }
}

/// The decoding the receiver does with `blob`, its outcome discarded: what
/// a checker must see finish within its budget. A decoding error or an
/// engine panic is no failure here (the receiver refuses those itself);
/// running out of memory or time is.
pub fn decode(kind: Blob, blob: &[u8]) {
    match kind {
        Blob::Update => {
            let _ = super::update_peers(blob);
            if let Ok(mut doc) = Document::new(1) {
                let _ = doc.missing_dependency(blob);
                let _ = doc.merge_unindexed(blob);
            }
        }
        Blob::Snapshot => {
            let _ = super::snapshot_version(blob);
            if let Ok(mut doc) = Document::import(blob, 1) {
                let _ = doc.settle();
                doc.read_history();
            }
        }
    }
}

/// What one blob's decoding may use in a checker's child. The memory and
/// processor time grow with the blob's size, for snapshots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// Address space the decoding may add, in bytes...
    pub memory: u64,
    /// ...and this many more for each MiB of the blob, or part of one.
    pub memory_per_mib: u64,
    /// Processor time it may take, in whole seconds...
    pub cpu_seconds: u64,
    /// ...and this many more milliseconds for each MiB of the blob.
    pub cpu_millis_per_mib: u64,
    /// Wall-clock time to start the child, and for each blob, beyond twice
    /// its processor time.
    pub wall: Duration,
}

const MIB: u64 = 1 << 20;

/// An update is at most 1 MiB (`MAX_PLAINTEXT_BYTES`). The most an honest
/// one took in the measurements (`examples/engine_budget.rs`) is 48.5 MiB
/// of address space and 0.42 s, for one commit renaming a task again and
/// again: this allows about 5 times that memory and 24 times that time
/// (docs/SYNC_DURABLE_RECEIVE.md, "Engine resources").
pub const UPDATE_BUDGET: Budget = Budget {
    memory: 256 * MIB,
    memory_per_mib: 0,
    cpu_seconds: 10,
    cpu_millis_per_mib: 0,
    wall: Duration::from_secs(20),
};

/// A snapshot is at most 256 MiB. Its decoding grows with the collection,
/// so the budget grows with its size. The honest snapshot that took the
/// most memory for its size, 100,000 tasks without notes (16.4 MiB),
/// took 793 MiB and 12.3 s: this allows 2.7 times that memory and 3.6
/// times that time (docs/SYNC_CHECKPOINTS.md, "Limits").
pub const SNAPSHOT_BUDGET: Budget = Budget {
    memory: 512 * MIB,
    memory_per_mib: 96 * MIB,
    cpu_seconds: 10,
    cpu_millis_per_mib: 2000,
    wall: Duration::from_secs(20),
};

fn mibs(len: usize) -> u64 {
    (len as u64).div_ceil(MIB)
}

impl Budget {
    fn memory_for(&self, len: usize) -> u64 {
        self.memory
            .saturating_add(self.memory_per_mib.saturating_mul(mibs(len)))
    }

    fn cpu_for(&self, len: usize) -> u64 {
        self.cpu_seconds.saturating_add(
            self.cpu_millis_per_mib
                .saturating_mul(mibs(len))
                .div_ceil(1000),
        )
    }

    fn wall_for(&self, len: usize) -> Duration {
        self.wall
            .saturating_add(Duration::from_secs(self.cpu_for(len).saturating_mul(2)))
    }

    fn to_args(self) -> [String; 4] {
        [
            self.memory.to_string(),
            self.memory_per_mib.to_string(),
            self.cpu_seconds.to_string(),
            self.cpu_millis_per_mib.to_string(),
        ]
    }

    fn from_args(args: &[OsString]) -> Option<Self> {
        let n: Vec<u64> = args
            .iter()
            .map(|a| a.to_str()?.parse().ok())
            .collect::<Option<_>>()?;
        let [memory, memory_per_mib, cpu_seconds, cpu_millis_per_mib] = n[..] else {
            return None;
        };
        Some(Self {
            memory,
            memory_per_mib,
            cpu_seconds,
            cpu_millis_per_mib,
            wall: Duration::ZERO,
        })
    }
}

/// Checks blobs in a child process: `program`, with `args`, then
/// [`CHECK_ARG`], the kind and its budget. The program must call
/// [`run_if_requested`] first thing.
#[derive(Debug, Clone)]
pub struct ChildProcess {
    program: PathBuf,
    args: Vec<OsString>,
    update: Budget,
    snapshot: Budget,
}

/// The failure to check at all, as opposed to a verdict.
fn unavailable(what: impl std::fmt::Display) -> MattersError {
    MattersError::Engine(format!("the engine checker is unavailable: {what}"))
}

/// How one child's run ended.
struct Run {
    /// Blobs that passed, from the first.
    passed: usize,
    /// Why the blob after them failed, if one did.
    stopped: Option<String>,
}

/// What the parent hears from the child.
enum Heard {
    Hello,
    Passed,
}

impl ChildProcess {
    /// Run `program` with the default budgets.
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            update: UPDATE_BUDGET,
            snapshot: SNAPSHOT_BUDGET,
        }
    }

    /// The running binary itself, through `/proc/self/exe`: the same file
    /// even if an update replaced it on disk since it started. Inside a
    /// Flatpak this is the app's own binary in the sandbox.
    pub fn this_executable() -> Self {
        Self::new("/proc/self/exe")
    }

    /// Arguments before [`CHECK_ARG`].
    pub fn with_args(mut self, args: impl IntoIterator<Item = impl Into<OsString>>) -> Self {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_budgets(mut self, update: Budget, snapshot: Budget) -> Self {
        self.update = update;
        self.snapshot = snapshot;
        self
    }

    fn budget(&self, kind: Blob) -> Budget {
        match kind {
            Blob::Update => self.update,
            Blob::Snapshot => self.snapshot,
        }
    }

    /// One child for `blobs`, until it ends or one of them stops it.
    fn run(&self, kind: Blob, blobs: &[&[u8]]) -> Result<Run> {
        let budget = self.budget(kind);
        let mut child = Command::new(&self.program)
            .args(&self.args)
            .arg(CHECK_ARG)
            .arg(kind.as_arg())
            .args(budget.to_args())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| unavailable(format!("{}: {e}", self.program.display())))?;
        let stdin = child.stdin.take().expect("stdin is piped");
        let stdout = child.stdout.take().expect("stdout is piped");
        let (tx, rx) = mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(move || feed(stdin, blobs));
            scope.spawn(move || listen(stdout, tx, blobs.len()));
            let watched = watch(rx, blobs, &budget);
            // Whatever it is doing now, its answers are in. Killing it also
            // ends the threads' reads and writes.
            let _ = child.kill();
            let status = child.wait().map_err(unavailable)?;
            match watched {
                Watched::Silent => Err(unavailable("the child did not start checking")),
                Watched::Ended(passed) if passed == blobs.len() => Ok(Run {
                    passed,
                    stopped: None,
                }),
                Watched::Ended(passed) => Ok(Run {
                    passed,
                    stopped: Some(stopped(status)?),
                }),
                Watched::Late(passed) => Ok(Run {
                    passed,
                    stopped: Some("it ran out of time".into()),
                }),
            }
        })
    }
}

/// Why a child that did not finish its blobs stopped, as a verdict on the
/// blob it was decoding; or `Err` if that is no verdict.
fn stopped(status: ExitStatus) -> Result<String> {
    match (status.signal(), status.code()) {
        // Not this process's kill (that is `Late`), nor a limit's: the
        // system's out-of-memory killer or a user. Not the blob's doing.
        (Some(s), _) if s == Signal::KILL.as_raw() => Err(unavailable("the child was killed")),
        (Some(s), _) if s == Signal::XCPU.as_raw() => Ok("it ran out of processor time".into()),
        // Out of address space, a failed allocation aborts.
        (Some(s), _) => Ok(format!("it stopped the decoder (signal {s})")),
        (None, Some(UNUSABLE)) => Err(unavailable("the child could not limit itself")),
        (None, _) => Ok(format!("it stopped the decoder ({status})")),
    }
}

/// How the parent's watch of a child ended.
enum Watched {
    /// No greeting in time.
    Silent,
    /// The child's output ended, after this many blobs passed.
    Ended(usize),
    /// A blob was still decoding at its deadline, after this many passed.
    Late(usize),
}

fn watch(rx: Receiver<Heard>, blobs: &[&[u8]], budget: &Budget) -> Watched {
    if !matches!(rx.recv_timeout(budget.wall), Ok(Heard::Hello)) {
        return Watched::Silent;
    }
    for (passed, blob) in blobs.iter().enumerate() {
        match rx.recv_timeout(budget.wall_for(blob.len())) {
            Ok(_) => {}
            Err(RecvTimeoutError::Timeout) => return Watched::Late(passed),
            Err(RecvTimeoutError::Disconnected) => return Watched::Ended(passed),
        }
    }
    Watched::Ended(blobs.len())
}

/// Write each blob, length first. A child that stopped reading ends it.
fn feed(mut stdin: std::process::ChildStdin, blobs: &[&[u8]]) {
    for blob in blobs {
        let written = stdin
            .write_all(&(blob.len() as u64).to_le_bytes())
            .and_then(|()| stdin.write_all(blob));
        if written.is_err() {
            return;
        }
    }
}

/// Pass on the child's greeting, then one message for each blob it passed.
fn listen(stdout: std::process::ChildStdout, tx: Sender<Heard>, blobs: usize) {
    let mut stdout = std::io::BufReader::new(stdout);
    let mut hello = vec![0; HELLO.len()];
    if stdout.read_exact(&mut hello).is_err() || hello != HELLO || tx.send(Heard::Hello).is_err() {
        return;
    }
    for _ in 0..blobs {
        let mut byte = [0];
        if stdout.read_exact(&mut byte).is_err() || byte[0] != PASSED {
            return;
        }
        if tx.send(Heard::Passed).is_err() {
            return;
        }
    }
}

impl Checker for ChildProcess {
    fn check(&self, kind: Blob, blobs: &[&[u8]]) -> Result<Vec<Verdict>> {
        let mut verdicts = Vec::with_capacity(blobs.len());
        // Each run settles at least one blob, so this many always suffice.
        for _ in 0..blobs.len() {
            let rest = &blobs[verdicts.len()..];
            if rest.is_empty() {
                break;
            }
            let run = self.run(kind, rest)?;
            verdicts.extend(std::iter::repeat_n(Verdict::Pass, run.passed));
            if let Some(why) = run.stopped {
                verdicts.push(Verdict::Refuse(format!(
                    "the engine could not decode it within its budget: {why}"
                )));
            }
        }
        if verdicts.len() != blobs.len() {
            return Err(unavailable("the child settled too few blobs"));
        }
        Ok(verdicts)
    }
}

/// If this process was started as a checker's child, check the blobs on
/// its standard input and exit; otherwise return at once. A binary that
/// installs [`ChildProcess::this_executable`] calls this first thing in
/// `main`, before any interface or runtime starts.
pub fn run_if_requested() {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if args.first().map(OsString::as_os_str) == Some(OsStr::new(CHECK_ARG)) {
        std::process::exit(child(&args[1..]));
    }
}

/// The child: greet, then for each blob on standard input, limit itself,
/// decode it and say so. Its exit status is 0 once the input ends.
fn child(args: &[OsString]) -> i32 {
    let Some((kind, budget)) = args
        .split_first()
        .and_then(|(kind, budget)| Some((Blob::from_arg(kind)?, Budget::from_args(budget)?)))
    else {
        return UNUSABLE;
    };
    if prepare().is_err() {
        return UNUSABLE;
    }
    let mut input = std::io::stdin().lock();
    let mut output = std::io::stdout().lock();
    if output
        .write_all(HELLO)
        .and_then(|()| output.flush())
        .is_err()
    {
        return UNUSABLE;
    }
    loop {
        let mut len = [0; 8];
        match input.read(&mut len[..1]) {
            Ok(0) => return 0,
            Ok(_) => {}
            Err(_) => return UNUSABLE,
        }
        // Reading the blob is not part of its budget.
        if input.read_exact(&mut len[1..]).is_err() || lift().is_err() {
            return UNUSABLE;
        }
        let Ok(len) = usize::try_from(u64::from_le_bytes(len)) else {
            return UNUSABLE;
        };
        let mut blob = vec![0; len];
        if input.read_exact(&mut blob).is_err() || limit(&budget, len).is_err() {
            return UNUSABLE;
        }
        decode(kind, &blob);
        if output
            .write_all(&[PASSED])
            .and_then(|()| output.flush())
            .is_err()
        {
            return UNUSABLE;
        }
    }
}

/// No core file, death with the parent, and first in line for the
/// out-of-memory killer.
fn prepare() -> std::io::Result<()> {
    rustix::process::setrlimit(
        Resource::Core,
        Rlimit {
            current: Some(0),
            maximum: Some(0),
        },
    )?;
    rustix::process::set_parent_process_death_signal(Some(Signal::KILL))?;
    // Best effort: an older kernel or a locked-down /proc refuses it.
    let _ = std::fs::write("/proc/self/oom_score_adj", "1000");
    Ok(())
}

/// The soft limit of `resource` set to `want`, within its hard limit.
fn set_soft(resource: Resource, want: Option<u64>) -> std::io::Result<()> {
    let hard = rustix::process::getrlimit(resource).maximum;
    let current = match (want, hard) {
        (Some(w), Some(h)) => Some(w.min(h)),
        (Some(w), None) => Some(w),
        (None, h) => h,
    };
    Ok(rustix::process::setrlimit(
        resource,
        Rlimit {
            current,
            maximum: hard,
        },
    )?)
}

/// Room to read the next blob.
fn lift() -> std::io::Result<()> {
    set_soft(Resource::As, None)
}

/// The next blob's budget, on top of what this process already uses: its
/// memory never more than half the machine's.
fn limit(budget: &Budget, len: usize) -> std::io::Result<()> {
    let memory = within(budget.memory_for(len), physical_memory());
    set_soft(Resource::As, Some(address_space()?.saturating_add(memory)))?;
    // From the next whole second.
    let used = rustix::time::clock_gettime(rustix::time::ClockId::ProcessCPUTime);
    let seconds = (used.tv_sec as u64).saturating_add(1);
    set_soft(
        Resource::Cpu,
        Some(seconds.saturating_add(budget.cpu_for(len))),
    )
}

/// `memory`, but at most half of `physical`: a budget the machine cannot
/// afford would only bring in its out-of-memory killer.
fn within(memory: u64, physical: Option<u64>) -> u64 {
    physical.map_or(memory, |p| memory.min(p / 2))
}

/// The machine's memory, in bytes (`MemTotal`).
fn physical_memory() -> Option<u64> {
    kilobytes(&std::fs::read_to_string("/proc/meminfo").ok()?, "MemTotal:")
}

/// This process's address space, in bytes (`VmSize`).
fn address_space() -> std::io::Result<u64> {
    kilobytes(&std::fs::read_to_string("/proc/self/status")?, "VmSize:")
        .ok_or_else(|| std::io::Error::other("no VmSize in /proc/self/status"))
}

/// The value of `field`, given in kB in a /proc file, in bytes.
fn kilobytes(text: &str, field: &str) -> Option<u64> {
    text.lines()
        .find_map(|line| line.strip_prefix(field))
        .and_then(|kb| kb.trim().strip_suffix("kB")?.trim().parse::<u64>().ok())
        .map(|kb| kb * 1024)
}

type Key = (Blob, [u8; 48]);

/// Verdicts by the hash of their bytes, the oldest forgotten first.
struct Cache {
    verdicts: BTreeMap<Key, Verdict>,
    order: VecDeque<Key>,
}

impl Cache {
    const fn new() -> Self {
        Self {
            verdicts: BTreeMap::new(),
            order: VecDeque::new(),
        }
    }

    fn insert(&mut self, key: Key, verdict: Verdict) {
        if self.verdicts.insert(key, verdict).is_none() {
            self.order.push_back(key);
        }
        if self.order.len() > CACHED {
            if let Some(oldest) = self.order.pop_front() {
                self.verdicts.remove(&oldest);
            }
        }
    }
}

/// The installed checker and its verdicts.
struct Registry {
    checker: RwLock<Option<Arc<dyn Checker>>>,
    cache: Mutex<Cache>,
    /// With no checker installed, check in-process rather than refuse.
    in_process_by_default: bool,
}

static REGISTRY: Registry = Registry::new(cfg!(feature = "test-support"));

impl Registry {
    const fn new(in_process_by_default: bool) -> Self {
        Self {
            checker: RwLock::new(None),
            cache: Mutex::new(Cache::new()),
            in_process_by_default,
        }
    }

    fn install(&self, checker: Arc<dyn Checker>) {
        *self.checker.write().unwrap_or_else(PoisonError::into_inner) = Some(checker);
        // Another checker's verdicts are not this one's.
        *self.cache.lock().unwrap_or_else(PoisonError::into_inner) = Cache::new();
    }

    fn checker(&self) -> Result<Arc<dyn Checker>> {
        let installed = self
            .checker
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        match installed {
            Some(checker) => Ok(checker),
            None if self.in_process_by_default => Ok(Arc::new(InProcess)),
            None => Err(unavailable(
                "none is installed, so untrusted engine bytes are not decoded",
            )),
        }
    }

    fn vet(&self, kind: Blob, blobs: &[&[u8]]) -> Result<Vec<Verdict>> {
        let keys: Vec<Key> = blobs
            .iter()
            .map(|b| (kind, matters_sync::record_hash(b)))
            .collect();
        let mut verdicts: Vec<Option<Verdict>> = {
            let cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
            keys.iter()
                .map(|k| cache.verdicts.get(k).cloned())
                .collect()
        };
        let unknown: Vec<usize> = (0..blobs.len())
            .filter(|&i| verdicts[i].is_none())
            .collect();
        if !unknown.is_empty() {
            let asked: Vec<&[u8]> = unknown.iter().map(|&i| blobs[i]).collect();
            let answered = self.checker()?.check(kind, &asked)?;
            if answered.len() != asked.len() {
                return Err(unavailable("it answered for another number of blobs"));
            }
            let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
            for (i, verdict) in unknown.into_iter().zip(answered) {
                cache.insert(keys[i], verdict.clone());
                verdicts[i] = Some(verdict);
            }
        }
        Ok(verdicts.into_iter().flatten().collect())
    }
}

/// Install the checker this process uses from now on, in place of any
/// before it. Production binaries install [`ChildProcess::this_executable`]
/// at startup.
pub fn install(checker: Arc<dyn Checker>) {
    REGISTRY.install(checker);
}

/// The installed checker's verdicts on `blobs`, kept ones first.
pub(crate) fn vet(kind: Blob, blobs: &[&[u8]]) -> Result<Vec<Verdict>> {
    REGISTRY.vet(kind, blobs)
}

/// The installed checker's verdict on one blob.
pub(crate) fn vet_one(kind: Blob, blob: &[u8]) -> Result<Verdict> {
    vet(kind, &[blob])?
        .pop()
        .ok_or_else(|| unavailable("no verdict"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Refuses blobs that start with `!`, counting what it is asked.
    #[derive(Default)]
    struct Counting {
        calls: AtomicUsize,
        blobs: AtomicUsize,
    }

    impl Checker for Counting {
        fn check(&self, _: Blob, blobs: &[&[u8]]) -> Result<Vec<Verdict>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.blobs.fetch_add(blobs.len(), Ordering::SeqCst);
            Ok(blobs
                .iter()
                .map(|b| match b.first() {
                    Some(b'!') => Verdict::Refuse("bang".into()),
                    _ => Verdict::Pass,
                })
                .collect())
        }
    }

    struct Short;

    impl Checker for Short {
        fn check(&self, _: Blob, _: &[&[u8]]) -> Result<Vec<Verdict>> {
            Ok(vec![Verdict::Pass])
        }
    }

    #[test]
    fn with_none_installed_nothing_is_decoded_unless_by_default() {
        let refusing = Registry::new(false);
        let err = refusing.vet(Blob::Update, &[b"x"]).unwrap_err();
        assert!(err.to_string().contains("none is installed"), "{err}");
        let permissive = Registry::new(true);
        assert_eq!(
            permissive.vet(Blob::Update, &[b"x", b"!"]).unwrap(),
            vec![Verdict::Pass, Verdict::Pass]
        );
    }

    #[test]
    fn verdicts_are_asked_once_in_one_batch_and_kept() {
        let registry = Registry::new(false);
        let counting = Arc::new(Counting::default());
        registry.install(counting.clone());
        let verdicts = registry.vet(Blob::Update, &[b"a", b"!b", b"c"]).unwrap();
        assert_eq!(
            verdicts,
            vec![Verdict::Pass, Verdict::Refuse("bang".into()), Verdict::Pass]
        );
        assert_eq!(counting.calls.load(Ordering::SeqCst), 1);
        // Kept: only the new one is asked, and the answers stay in order.
        let verdicts = registry.vet(Blob::Update, &[b"!b", b"d", b"a"]).unwrap();
        assert_eq!(
            verdicts,
            vec![Verdict::Refuse("bang".into()), Verdict::Pass, Verdict::Pass]
        );
        assert_eq!(counting.calls.load(Ordering::SeqCst), 2);
        assert_eq!(counting.blobs.load(Ordering::SeqCst), 4);
        // All kept: the checker is not asked.
        registry.vet(Blob::Update, &[b"a", b"d"]).unwrap();
        assert_eq!(counting.calls.load(Ordering::SeqCst), 2);
        // Kept per kind.
        registry.vet(Blob::Snapshot, &[b"a"]).unwrap();
        assert_eq!(counting.calls.load(Ordering::SeqCst), 3);
        // A new checker starts afresh.
        let fresh = Arc::new(Counting::default());
        registry.install(fresh.clone());
        registry.vet(Blob::Update, &[b"a"]).unwrap();
        assert_eq!(fresh.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_checker_that_answers_for_too_few_blobs_is_no_answer() {
        let registry = Registry::new(false);
        registry.install(Arc::new(Short));
        assert!(registry.vet(Blob::Update, &[b"a", b"b"]).is_err());
        assert_eq!(
            registry.vet(Blob::Update, &[b"a"]).unwrap(),
            vec![Verdict::Pass]
        );
    }

    #[test]
    fn the_oldest_verdicts_are_forgotten_first() {
        let mut cache = Cache::new();
        let key = |n: usize| {
            (
                Blob::Update,
                [(n % 256) as u8, (n / 256) as u8]
                    .repeat(24)
                    .try_into()
                    .unwrap(),
            )
        };
        for n in 0..=CACHED {
            cache.insert(key(n), Verdict::Pass);
        }
        // The same key again neither grows it nor forgets another.
        cache.insert(key(CACHED), Verdict::Pass);
        assert_eq!(cache.verdicts.len(), CACHED);
        assert_eq!(cache.order.len(), CACHED);
        assert!(!cache.verdicts.contains_key(&key(0)));
        assert!(cache.verdicts.contains_key(&key(1)));
        assert!(cache.verdicts.contains_key(&key(CACHED)));
    }

    #[test]
    fn the_budgets_are_those_the_documents_state() {
        // docs/SYNC_DURABLE_RECEIVE.md and docs/SYNC_CHECKPOINTS.md give
        // the arithmetic.
        assert_eq!(UPDATE_BUDGET.memory_for(1 << 20), 268_435_456);
        assert_eq!(UPDATE_BUDGET.cpu_for(1 << 20), 10);
        assert_eq!(UPDATE_BUDGET.wall_for(1 << 20), Duration::from_secs(40));
        assert_eq!(SNAPSHOT_BUDGET.memory_for(0), 536_870_912);
        assert_eq!(SNAPSHOT_BUDGET.memory_for(16 << 20), 2_147_483_648);
        assert_eq!(SNAPSHOT_BUDGET.cpu_for(16 << 20), 42);
        assert_eq!(SNAPSHOT_BUDGET.wall_for(16 << 20), Duration::from_secs(104));
        assert_eq!(MIB, 1_048_576);
    }

    #[test]
    fn budgets_grow_with_the_blob_and_travel_as_arguments() {
        let budget = Budget {
            memory: 100,
            memory_per_mib: 10,
            cpu_seconds: 2,
            cpu_millis_per_mib: 1500,
            wall: Duration::from_secs(3),
        };
        assert_eq!(budget.memory_for(0), 100);
        assert_eq!(budget.memory_for(1), 110);
        assert_eq!(budget.memory_for(MIB as usize), 110);
        assert_eq!(budget.memory_for(MIB as usize + 1), 120);
        assert_eq!(budget.cpu_for(0), 2);
        // 1.5 s rounds up to 2.
        assert_eq!(budget.cpu_for(1), 4);
        assert_eq!(budget.cpu_for(2 * MIB as usize), 5);
        assert_eq!(budget.wall_for(1), Duration::from_secs(3 + 8));
        let args: Vec<OsString> = budget.to_args().into_iter().map(Into::into).collect();
        assert_eq!(
            Budget::from_args(&args),
            Some(Budget {
                wall: Duration::ZERO,
                ..budget
            })
        );
        assert_eq!(Budget::from_args(&args[..3]), None);
        let mut bad = args.clone();
        bad[2] = "two".into();
        assert_eq!(Budget::from_args(&bad), None);
        for kind in [Blob::Update, Blob::Snapshot] {
            assert_eq!(Blob::from_arg(OsStr::new(kind.as_arg())), Some(kind));
        }
        assert_eq!(Blob::from_arg(OsStr::new("other")), None);
    }

    #[test]
    fn this_process_reads_its_address_space_and_the_machines_memory() {
        let size = address_space().unwrap();
        assert!(size > MIB, "{size}");
        let physical = physical_memory().unwrap();
        assert!(physical > size, "{physical}");
        assert_eq!(
            kilobytes("A: 1\nVmSize:\t  12 kB\n", "VmSize:"),
            Some(12 * 1024)
        );
        assert_eq!(kilobytes("VmSize: 12 MB", "VmSize:"), None);
        assert_eq!(kilobytes("Other: 12 kB", "VmSize:"), None);
    }

    #[test]
    fn a_budget_is_at_most_half_the_machines_memory() {
        assert_eq!(within(100, None), 100);
        assert_eq!(within(100, Some(1000)), 100);
        assert_eq!(within(100, Some(150)), 75);
    }
}
