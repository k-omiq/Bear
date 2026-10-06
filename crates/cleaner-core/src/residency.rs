//! Where a loaded session actually lives, once nothing is using it.
//!
//! [`crate::registry`] answers *what is resident*; this answers *who is holding
//! it*, and the answer is deliberately not "the run". Until this module existed,
//! every session was a local of the thread that opened it: a [`crate::detect::Detector`]
//! belonged to the run's `Pipeline`, and the four region edits opened their own
//! bench and dropped it at the end of the click. That is correct for memory and
//! wrong for a person - **a model was reloaded for every run, every page turn
//! and every region edit**, and the reload is 2.1 s for rung 2 and ten to sixty
//! seconds for rung 3a.
//!
//! ## The ruling
//!
//! A model, once loaded, stays in memory until **one** of three things happens:
//!
//! - the user presses the close button on its row in the loaded-models tab;
//! - nothing has used it for its kind's own [`crate::registry::Kind::idle_grace`];
//! - the machine is under memory pressure and the pressure ladder takes it
//!   back.
//!
//! Finishing a run is not on that list. Finishing a region edit is not on that
//! list. Turning a page is not on that list.
//!
//! ## Checkout, not sharing
//!
//! A cached session is **moved out** to whoever wants it and moved back when
//! they are done ([`checkout`] and [`checkin`]). Nothing is shared, nothing is
//! locked for the length of an inference, and no session type has to be `Sync`.
//! The property that makes an eviction safe falls out of the same shape: what
//! this module can drop is exactly what is *checked in*, and a session in use is
//! not checked in. **Nothing here can interrupt an inference, because nothing
//! here can reach a session anybody is using.**
//!
//! ## The sweep
//!
//! Idle eviction needs somebody to notice the time. Three things call
//! [`sweep`] and they cover the cases between them:
//!
//! - the tab's own poll, every two seconds while the editor is on screen
//!   (`src-tauri/src/models.rs`);
//! - the run's region boundary, which is where a run already gives sessions back;
//! - a sweeper thread of this module's own, every [`SWEEP_INTERVAL`], so that an
//!   application sitting on the library screen with no editor open still gives
//!   4.6 GB back two minutes after the last click.
//!
//! The sweeper holds no lock while it drops anything: a [`crate::sidecar`] child
//! goes by way of an HTTP shutdown and a kill, and doing that under the table's
//! mutex would stall every other reader for the length of it.

use std::any::Any;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use crate::registry::Kind;

/// How often the sweeper thread looks.
///
/// Fifteen seconds. It is a fraction of the shortest grace
/// ([`crate::registry::SIDECAR_IDLE_GRACE`]) so that an eviction happens near
/// the time it was promised, and long enough that a sleeping application is
/// doing nothing measurable.
pub const SWEEP_INTERVAL: Duration = Duration::from_secs(15);

/// What may be parked here.
///
/// The one thing this module has to ask a cached value is whether it should
/// still be cached, and the answer is always the same sentence - *the registry
/// says somebody asked for it back, or nothing has used it for its grace* -
/// which every session type already answers as `spent()`.
pub trait Resident: Any + Send {
    fn spent(&self) -> bool;
}

/// Which cached session a caller means.
///
/// The kind alone is not enough: two model directories or two accelerator
/// preferences build different sessions, and handing back a session opened
/// against another directory would be answering a different question than the
/// one asked. The discriminator is the caller's - a model path and a preference
/// for an ONNX session, an install root, a backend and a model name for the
/// child.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Key {
    kind: Kind,
    of: String,
}

impl Key {
    pub fn new(kind: Kind, of: impl Into<String>) -> Key {
        Key { kind, of: of.into() }
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }
}

struct Slot {
    key: Key,
    value: Box<dyn Any + Send>,
    /// [`Resident::spent`], reached without knowing the type: the concrete type
    /// is captured at [`checkin`] and the pointer downcasts back to it. A
    /// `dyn Resident` would need trait upcasting to downcast afterwards, and
    /// this crate builds on a Rust older than that.
    spent: fn(&(dyn Any + Send)) -> bool,
}

fn spent_of<T: Resident>(value: &(dyn Any + Send)) -> bool {
    value.downcast_ref::<T>().is_some_and(Resident::spent)
}

fn table() -> MutexGuard<'static, Vec<Slot>> {
    static TABLE: OnceLock<Mutex<Vec<Slot>>> = OnceLock::new();
    // A poisoned lock is a panic somewhere else, and losing every cached session
    // to it would turn one failure into an application that reloads models
    // forever. The slots are plain ownership; there is no invariant a panicking
    // writer could have left half-applied.
    TABLE.get_or_init(|| Mutex::new(Vec::new())).lock().unwrap_or_else(|e| e.into_inner())
}

/// Take the session parked under this key, if one is parked and it has not been
/// asked for back.
///
/// A spent slot is **not** handed out: the user pressed close, or the grace
/// elapsed while nothing was looking, and giving the session to the next caller
/// would be quietly refusing the request. It is dropped here instead, and the
/// caller opens a new one.
pub fn checkout<T: Resident>(key: &Key) -> Option<T> {
    let taken = {
        let mut table = table();
        let at = table.iter().position(|slot| slot.key == *key)?;
        table.remove(at)
    };
    if (taken.spent)(taken.value.as_ref()) {
        // Dropped outside the lock, like everything else here.
        drop(taken);
        return None;
    }
    match taken.value.downcast::<T>() {
        Ok(value) => Some(*value),
        // A key claimed by another type is a bug in the caller, not a reason to
        // hand back something of the wrong shape. The session goes.
        Err(_) => None,
    }
}

/// Park a session under this key until something asks for it again.
///
/// A session that is **already spent** is dropped rather than parked: an unload
/// the user asked for during a run must not be undone by the run finishing.
pub fn checkin<T: Resident>(key: Key, value: T) {
    if value.spent() {
        return;
    }
    start_sweeper();
    let slot = Slot { key, value: Box::new(value), spent: spent_of::<T> };
    let replaced = {
        let mut table = table();
        let existing = table.iter().position(|held| held.key == slot.key);
        let replaced = existing.map(|at| table.remove(at));
        table.push(slot);
        replaced
    };
    drop(replaced);
}

/// Drop every parked session the registry says is spent. Answers how many went.
pub fn sweep() -> usize {
    let spent = {
        let mut table = table();
        let mut spent = Vec::new();
        let mut at = 0;
        while at < table.len() {
            if (table[at].spent)(table[at].value.as_ref()) {
                spent.push(table.remove(at));
            } else {
                at += 1;
            }
        }
        spent
    };
    let count = spent.len();
    drop(spent);
    // A remembered detection outlives no detector. See [`crate::detect::forget_unbacked`].
    crate::detect::forget_unbacked();
    count
}

/// Drop every parked session of one kind, whether or not it is spent.
///
/// The pressure ladder, which is the one caller entitled to take a session
/// nobody asked to give up: a step down it is the machine talking, not the
/// clock.
pub fn evict(kind: Kind) -> usize {
    let taken = {
        let mut table = table();
        let mut taken = Vec::new();
        let mut at = 0;
        while at < table.len() {
            if table[at].key.kind == kind {
                taken.push(table.remove(at));
            } else {
                at += 1;
            }
        }
        taken
    };
    let count = taken.len();
    drop(taken);
    count
}

/// Give SAM analysis room by dropping parked CTD, LaMa, and sidecar sessions.
/// Checked-out sessions remain with their owner until its next safe point.
pub fn evict_for_sam() -> usize {
    [Kind::TextDetector, Kind::Inpainter, Kind::Sidecar]
        .into_iter()
        .map(evict)
        .sum()
}

/// Whether [`evict_for_sam`] would drop anything right now. Read-only: a
/// status query can say room may be made without making it.
pub fn reclaimable_for_sam() -> bool {
    [Kind::TextDetector, Kind::Inpainter, Kind::Sidecar]
        .into_iter()
        .any(parked)
}

/// Whether anything of this kind is parked here right now.
pub fn parked(kind: Kind) -> bool {
    table().iter().any(|slot| slot.key.kind == kind)
}

/// Drop everything. For the tests, and for a "free memory" command.
pub fn clear() {
    let taken: Vec<Slot> = table().drain(..).collect();
    drop(taken);
}

/* ------------------------------------------------------------------ */
/* What may be parked                                                  */
/* ------------------------------------------------------------------ */

/// Every loaded thing in this crate, gathered here rather than spread over six
/// files: the trait says one thing about each of them and the one thing is
/// already written on each of them as `spent()`. A reader who wants to know
/// what can survive a run reads this list.
macro_rules! resident {
    ($($type:ty),* $(,)?) => {
        $(impl Resident for $type {
            fn spent(&self) -> bool {
                <$type>::spent(self)
            }
        })*
    };
}

resident!(
    crate::detect::Detector,
    crate::balloon::BalloonDetector,
    crate::gate::ScriptGate,
    crate::gate::Hayai,
    crate::engines::lama::Inpainter,
    crate::engines::flux::Inpainter,
);

/// The sweeper, started the first time anything is parked and never stopped.
///
/// It exists for the case no poll covers: the editor is closed, no run is
/// going, and a 4.6 GB child is sitting on the machine because the last thing
/// the user did before making a cup of tea was clean one region with rung 3a.
fn start_sweeper() {
    static SWEEPER: AtomicBool = AtomicBool::new(false);
    if !SWEEPER.swap(true, Ordering::Relaxed)
        && std::thread::Builder::new()
            .name("residency-sweep".to_owned())
            .spawn(|| {
                loop {
                    std::thread::sleep(SWEEP_INTERVAL);
                    sweep();
                }
            })
            .is_err()
    {
        SWEEPER.store(false, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{self, Device, Footprint};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    /// A stand-in for a session: it holds a registry row exactly as a real one
    /// does, and it counts its own drops so a test can say whether a *session*
    /// went rather than only whether a row did.
    struct Fake {
        lease: registry::Lease,
        dropped: Arc<AtomicU32>,
    }

    impl Fake {
        fn open(kind: Kind, dropped: &Arc<AtomicU32>) -> Fake {
            Fake {
                lease: registry::register(
                    kind,
                    Footprint::measured(1),
                    Device::accelerator(crate::accel::Accelerator::Cpu),
                ),
                dropped: Arc::clone(dropped),
            }
        }
    }

    impl Resident for Fake {
        fn spent(&self) -> bool {
            self.lease.spent()
        }
    }

    impl Drop for Fake {
        fn drop(&mut self) {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn key(of: &str) -> Key {
        Key::new(Kind::TextDetector, of)
    }

    fn test_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(())).lock().unwrap_or_else(|e| e.into_inner())
    }

    /// **The ruling, as a test.** A session checked in at the end of one piece
    /// of work is the *same session* the next piece of work gets: not a new
    /// row, not a rebuild.
    #[test]
    fn a_session_survives_the_work_that_opened_it() {
        let _guard = test_lock();
        clear();
        let dropped = Arc::new(AtomicU32::new(0));
        let key = key("survives");
        let id = {
            let session = Fake::open(Kind::TextDetector, &dropped);
            let id = session.lease.id();
            checkin(key.clone(), session);
            id
        };
        assert_eq!(dropped.load(Ordering::Relaxed), 0, "the session went with its owner");

        let again: Fake = checkout(&key).expect("nothing was parked");
        assert_eq!(again.lease.id(), id, "a different session came back");
        checkin(key.clone(), again);
        clear();
    }

    /// An unload asked for while the session was in use is honoured when it
    /// comes back, rather than being undone by the work finishing.
    #[test]
    fn a_session_somebody_asked_for_back_is_never_parked() {
        let _guard = test_lock();
        clear();
        let dropped = Arc::new(AtomicU32::new(0));
        let key = key("unload-request");
        let session = Fake::open(Kind::BalloonDetector, &dropped);
        assert!(registry::request_unload(session.lease.id()));
        checkin(key.clone(), session);
        assert_eq!(dropped.load(Ordering::Relaxed), 1, "a spent session was parked anyway");
        assert!(checkout::<Fake>(&key).is_none());
    }

    /// And one asked for back **after** it was parked goes on the next sweep,
    /// with nothing holding it and nobody having to call anything else.
    #[test]
    fn the_sweep_gives_back_what_the_user_asked_for() {
        let _guard = test_lock();
        clear();
        let dropped = Arc::new(AtomicU32::new(0));
        let key = key("unload-parked");
        let session = Fake::open(Kind::ScriptGate, &dropped);
        let id = session.lease.id();
        checkin(key.clone(), session);

        assert!(registry::request_unload(id));
        assert_eq!(dropped.load(Ordering::Relaxed), 0, "it went before the sweep");
        sweep();
        assert_eq!(dropped.load(Ordering::Relaxed), 1, "the sweep left it resident");
        assert!(!registry::loaded().iter().any(|row| row.id == id), "the row outlived the session");
        assert!(checkout::<Fake>(&key).is_none());
    }

    /// **The idle grace, which is the second of the three ways a model may
    /// go.** The row is aged past its kind's grace and the sweep takes it -
    /// and a row aged past the *sidecar's* shorter grace but not the ONNX one
    /// stays, which is the whole point of there being two numbers.
    #[test]
    fn the_sweep_gives_back_what_nothing_has_used() {
        let _guard = test_lock();
        clear();
        let dropped = Arc::new(AtomicU32::new(0));
        let onnx = key("idle-onnx");
        let child = Key::new(Kind::Sidecar, "idle-sidecar");
        let onnx_session = Fake::open(Kind::TextDetector, &dropped);
        let child_session = Fake::open(Kind::Sidecar, &dropped);
        let (onnx_id, child_id) = (onnx_session.lease.id(), child_session.lease.id());
        checkin(onnx.clone(), onnx_session);
        checkin(child.clone(), child_session);

        // Two minutes and a moment: past the child's grace, well inside the
        // session's.
        let elapsed = registry::SIDECAR_IDLE_GRACE + Duration::from_secs(1);
        assert!(registry::age(onnx_id, elapsed));
        assert!(registry::age(child_id, elapsed));
        assert_eq!(sweep(), 1, "the two graces are not being told apart");
        assert!(parked(Kind::TextDetector), "the session went on the child's clock");
        assert!(!parked(Kind::Sidecar), "the child outlived its grace");

        // And on past the session's own.
        assert!(registry::age(onnx_id, registry::ONNX_IDLE_GRACE));
        sweep();
        assert!(!parked(Kind::TextDetector));
        assert_eq!(dropped.load(Ordering::Relaxed), 2);
    }

    /// A session handed out is a session **taken**: nothing can evict it while
    /// it is in use, because it is not here to evict. That is the property that
    /// makes "never mid-inference" structural rather than a matter of timing.
    #[test]
    fn a_session_in_use_is_not_here_to_be_swept() {
        let _guard = test_lock();
        clear();
        let dropped = Arc::new(AtomicU32::new(0));
        let key = key("in-use");
        let session = Fake::open(Kind::TextDetector, &dropped);
        let id = session.lease.id();
        checkin(key.clone(), session);

        let borrowed: Fake = checkout(&key).expect("nothing was parked");
        assert!(registry::request_unload(id));
        sweep();
        assert_eq!(dropped.load(Ordering::Relaxed), 0, "the sweep reached a session in use");
        // The holder gives it back at its own safe point, which is where
        // `release_if_spent` lives in the callers.
        assert!(borrowed.spent());
        drop(borrowed);
        assert_eq!(dropped.load(Ordering::Relaxed), 1);
    }

    /// The ladder's own eviction: not a request and not a clock, so a session
    /// nobody asked about still goes.
    #[test]
    fn the_pressure_ladder_takes_a_kind_back_whether_it_is_spent_or_not() {
        let _guard = test_lock();
        clear();
        let dropped = Arc::new(AtomicU32::new(0));
        checkin(Key::new(Kind::Sidecar, "pressure"), Fake::open(Kind::Sidecar, &dropped));
        assert!(parked(Kind::Sidecar));
        assert_eq!(evict(Kind::Sidecar), 1);
        assert_eq!(dropped.load(Ordering::Relaxed), 1);
        assert!(!parked(Kind::Sidecar));
    }

    #[test]
    fn sam_admission_releases_only_parked_competing_sessions() {
        let _guard = test_lock();
        clear();
        let dropped = Arc::new(AtomicU32::new(0));
        for kind in [Kind::TextDetector, Kind::Inpainter, Kind::Sidecar, Kind::ScriptGate] {
            checkin(Key::new(kind, format!("sam-{kind:?}")), Fake::open(kind, &dropped));
        }
        assert_eq!(evict_for_sam(), 3);
        assert_eq!(dropped.load(Ordering::Relaxed), 3);
        assert!(parked(Kind::ScriptGate));
        assert!(!parked(Kind::TextDetector));
        assert!(!parked(Kind::Inpainter));
        assert!(!parked(Kind::Sidecar));
        clear();
    }

    /// Two model directories are two sessions. A key that ignored the
    /// discriminator would hand a run the wrong weights.
    #[test]
    fn a_key_is_the_kind_and_what_it_was_opened_against() {
        let _guard = test_lock();
        clear();
        let dropped = Arc::new(AtomicU32::new(0));
        checkin(key("/models/a"), Fake::open(Kind::TextDetector, &dropped));
        assert!(checkout::<Fake>(&key("/models/b")).is_none());
        let mine: Option<Fake> = checkout(&key("/models/a"));
        assert!(mine.is_some());
        drop(mine);
    }

    /// `start_sweeper` can be called multiple times idempotently.
    #[test]
    fn start_sweeper_is_safe_and_idempotent() {
        let _guard = test_lock();
        start_sweeper();
        start_sweeper();
    }
}
