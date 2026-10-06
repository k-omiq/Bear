//! What is loaded **right now**, and who may ask for it back.
//!
//! The pressure ladder already gives sessions back when the kernel asks. This
//! module answers the other two questions the ladder never had to: *what is
//! resident at this moment*, and *may the user give one back by hand*. Both
//! are questions from outside the process - the interface's bottom-left tab is
//! the only reader - so what lives here is deliberately thin:
//!
//! - **This registry owns no session.** An `ort::session::Session` is owned by
//!   the type that opens it ([`crate::detect::Detector`],
//!   [`crate::engines::lama::Inpainter`], …) and dropped by that type's own
//!   scope. What is registered is a *row about* one - a kind, a size, a device
//!   and a clock - held under a [`Lease`] whose `Drop` removes the row. A
//!   session cannot be resident without a row, and a row cannot outlive its
//!   session, because the lease is a field of the thing that holds the session.
//! - **An unload is a request, not a kill.** [`request_unload`] sets a flag;
//!   the owner reads it at a point of its own choosing - between regions, where
//!   the last region's buffers are gone and the next one's do not exist - and
//!   drops the session there. Nothing here can interrupt an inference, which is
//!   the whole reason it cannot corrupt one. A run that still needs the model
//!   opens it again on the next region; that is stated in the interface rather
//!   than hidden, because a button that silently degrades the rest of a
//!   chapter is worse than one that frees memory now and pays for it later.
//! - **A poison is neither a request nor a kill.** [`Lease::poison`] says the
//!   session behind a row is dead - a GPU that was reset under it, not a person
//!   asking for the memory back - and it sets a flag of its own rather than
//!   borrowing the unload one, because the two are answers to different
//!   questions and only one of them belongs in the interface's "unloading"
//!   column. Its own documentation says why.
//!
//! ## The size is an estimate, and says which kind
//!
//! [`Basis`] is [`crate::memory::Basis`]'s distinction applied to a different
//! number: a figure someone measured on a running process is not the same
//! thing as the size of the file the session was built from, and rung 2 is the
//! proof - 207 MB of weights, **510 MB resident**. Every row says which it is
//! carrying, so a later measurement can replace a `Weights` row without anyone
//! having to guess whether the old number was one.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

/// How long an **ONNX session** may sit with nothing asking for it before its
/// owner is entitled to give it back.
///
/// **Five minutes, and the number is a ruling about the user rather than about
/// a run.** A session is now owned by [`crate::residency`] and not by the run
/// that opened it, so this clock is no longer "long enough not to fire mid-run" -
/// it is *how long a person may put the application down and come back to it
/// without paying for a session build again*. Reading a page, drawing a
/// rectangle, answering a message and coming back to clean the next balloon all
/// happen inside five minutes; a chapter left open over lunch does not. The
/// price of being wrong in one direction is a few hundred megabytes held on an
/// idle machine, and in the other a two-second stall on a click the user
/// experienced as instant a minute ago.
pub const ONNX_IDLE_GRACE: Duration = Duration::from_secs(300);

/// The same clock for [`Kind::Sidecar`], and **shorter on purpose**.
///
/// Two minutes. Rung 3a is not a session in this process at all: it is a
/// separate program holding several gigabytes, which is an order of
/// magnitude more than every ONNX session here put together. The reload it
/// saves is also the most expensive one in the application - ten to sixty
/// seconds - so holding it at all is worth doing, and holding it as long as a
/// 94 MB detector is not.
pub const SIDECAR_IDLE_GRACE: Duration = Duration::from_secs(120);

/// What kind of thing is loaded. The interface names these; nothing routes on
/// them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// `comictextdetector.onnx`.
    TextDetector,
    /// The speech-balloon detector, [`crate::balloon`].
    BalloonDetector,
    /// The script gate, [`crate::gate`].
    ScriptGate,
    /// The gate's rescue reader, [`crate::gate::ocr`]. **One row for two
    /// sessions**: the encoder and the decoder are opened together, dropped
    /// together, and useless apart, so a tab that listed them separately would
    /// offer two buttons for one decision.
    Ocr,
    /// Rung 2, [`crate::engines::lama`].
    Inpainter,
    /// Rung 3a's child process, [`crate::sidecar`]. Not a session in this
    /// process at all, which is exactly why it is worth a row: it is the
    /// largest thing the application can be holding and the only one that
    /// survives this process crashing.
    Sidecar,
}

impl Kind {
    /// The i18n key the interface names it under. No English crosses the seam.
    pub fn label_key(self) -> &'static str {
        match self {
            Kind::TextDetector => "models.kind.textDetector",
            Kind::BalloonDetector => "models.kind.balloonDetector",
            Kind::ScriptGate => "models.kind.scriptGate",
            Kind::Ocr => "models.kind.ocr",
            Kind::Inpainter => "models.kind.inpainter",
            Kind::Sidecar => "models.kind.sidecar",
        }
    }

    /// How long this kind may sit unused before it is given back:
    /// [`ONNX_IDLE_GRACE`] for a session in this process, [`SIDECAR_IDLE_GRACE`]
    /// for the child.
    pub fn idle_grace(self) -> Duration {
        match self {
            Kind::Sidecar => SIDECAR_IDLE_GRACE,
            _ => ONNX_IDLE_GRACE,
        }
    }
}

/// How the size on a row was arrived at. See the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Basis {
    /// Somebody ran the thing and read the process's resident set. The only
    /// figure that is actually the answer to "how much memory is this using".
    Measured,
    /// The size of the weights file the session was built from. A floor: a
    /// session is the weights plus the provider's arena, and the arena is not
    /// in this number.
    Weights,
    /// The thing itself said so, which is the sidecar's `MemoryReport`.
    Reported,
}

/// A size and how it was arrived at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Footprint {
    pub bytes: u64,
    pub basis: Basis,
}

impl Footprint {
    /// A figure from a measurement.
    pub fn measured(bytes: u64) -> Footprint {
        Footprint { bytes, basis: Basis::Measured }
    }

    /// The weights on disk. Zero where the file cannot be stat'd, which is a
    /// row that says "loaded, size unknown" rather than no row at all - the tab
    /// exists to say what is resident, and a model missing from it because its
    /// file moved is the one failure it must not have.
    pub fn weights(model: &Path) -> Footprint {
        let bytes = std::fs::metadata(model).map(|meta| meta.len()).unwrap_or(0);
        Footprint { bytes, basis: Basis::Weights }
    }

    /// The same floor for a session built from **more than one file**, which is
    /// the rescue reader: an encoder and a decoder opened together are one row
    /// ([`Kind::Ocr`]), and one row wants one number. A file that cannot be
    /// stat'd contributes nothing, for the reason [`Footprint::weights`] gives.
    pub fn weights_of(models: &[&Path]) -> Footprint {
        let bytes = models
            .iter()
            .filter_map(|model| std::fs::metadata(model).ok().map(|meta| meta.len()))
            .sum();
        Footprint { bytes, basis: Basis::Weights }
    }

    /// A figure the loaded thing reported about itself.
    pub fn reported(bytes: u64) -> Footprint {
        Footprint { bytes, basis: Basis::Reported }
    }
}

/// Where a loaded thing is running, as the interface says it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Device {
    /// An i18n key - [`crate::accel::Accelerator::label_key`], or
    /// `accel.sidecar` for the child process.
    pub label_key: &'static str,
    /// Whether it is holding graphics memory rather than ordinary RAM. On
    /// Apple silicon this is the same pool, which is why the tab says where
    /// rather than how much of which.
    pub gpu: bool,
}

impl Device {
    pub fn accelerator(accelerator: crate::accel::Accelerator) -> Device {
        use crate::accel::Accelerator::*;
        Device {
            label_key: accelerator.label_key(),
            gpu: matches!(accelerator, CoreMl | DirectMl | Cuda | TensorRt | Rocm | WebGpu),
        }
    }

    /// Rung 3a. Its device is the child's business and the child is on the
    /// machine's accelerator by construction ([`crate::sidecar::hardware`]).
    pub fn sidecar() -> Device {
        Device { label_key: "accel.sidecar", gpu: true }
    }
}

/// One row, as a reader outside this module sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loaded {
    pub id: u64,
    pub kind: Kind,
    pub model_name: Option<String>,
    pub bytes: u64,
    pub basis: Basis,
    pub device: Device,
    /// How long since anything used it.
    pub idle_ms: u64,
    /// Whether somebody has asked for it back and the owner has not reached a
    /// safe point yet.
    pub unload_requested: bool,
}

struct Entry {
    id: u64,
    kind: Kind,
    model_name: Option<String>,
    bytes: u64,
    basis: Basis,
    device: Device,
    used: Instant,
    unload: bool,
    /// The session this row describes has failed in a way it cannot come back
    /// from. Set by [`Lease::poison`] and never cleared - see there for why it
    /// is not the `unload` flag.
    poisoned: bool,
}

fn table() -> MutexGuard<'static, Vec<Entry>> {
    static TABLE: OnceLock<Mutex<Vec<Entry>>> = OnceLock::new();
    // A poisoned lock is a panic somewhere else, and losing the whole tab to it
    // would turn one failure into two. The rows are plain data; there is no
    // invariant a panicking writer could have left half-applied.
    TABLE.get_or_init(|| Mutex::new(Vec::new())).lock().unwrap_or_else(|e| e.into_inner())
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// The row's handle, held by whoever holds the session.
///
/// It is not `Clone`: two leases for one session would be two rows, and the
/// second `Drop` would remove a row that no longer described anything.
#[derive(Debug)]
pub struct Lease {
    id: u64,
}

/// Put a row up. The returned [`Lease`] must be stored beside the session it
/// describes - dropping it takes the row down.
pub fn register(kind: Kind, footprint: Footprint, device: Device) -> Lease {
    register_named(kind, footprint, device, None)
}

/// Register the exact checkpoint when a kind can run several models.
pub fn register_named(kind: Kind, footprint: Footprint, device: Device, model_name: Option<String>) -> Lease {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    table().push(Entry {
        id,
        kind,
        model_name,
        bytes: footprint.bytes,
        basis: footprint.basis,
        device,
        used: Instant::now(),
        unload: false,
        poisoned: false,
    });
    Lease { id }
}

impl Lease {
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Say that the session was just used. Called from the inference path, so
    /// the idle clock measures *work* rather than wall time since the open.
    pub fn touch(&self) {
        if let Some(entry) = table().iter_mut().find(|entry| entry.id == self.id) {
            entry.used = Instant::now();
        }
    }

    /// Whether somebody has asked for this one back.
    pub fn unload_requested(&self) -> bool {
        table().iter().find(|entry| entry.id == self.id).is_some_and(|entry| entry.unload)
    }

    /// How long since [`Lease::touch`].
    pub fn idle(&self) -> Duration {
        table()
            .iter()
            .find(|entry| entry.id == self.id)
            .map(|entry| entry.used.elapsed())
            .unwrap_or_default()
    }

    /// Whether the owner should give this session back at its next safe point:
    /// somebody asked, the session was poisoned, or nothing has wanted it for
    /// this kind's own [`Kind::idle_grace`].
    pub fn spent(&self) -> bool {
        let table = table();
        let Some(entry) = table.iter().find(|entry| entry.id == self.id) else {
            return false;
        };
        entry.unload || entry.poisoned || entry.used.elapsed() >= entry.kind.idle_grace()
    }

    /// Mark this row's session **unusable**, permanently: [`Lease::spent`]
    /// answers `true` from here on and never stops.
    ///
    /// The caller is a run that has just watched an inference fail with a
    /// device-removed status. On Windows the display driver's watchdog resets
    /// the adapter when one operation does not return inside its timeout - two
    /// seconds by default - and every session that was live across that reset
    /// is dead with it: the next run on the same session fails the same way,
    /// and the failure is a property of the device rather than of the model or
    /// the page, so retrying it is a loop. Without this the faulted session is
    /// checked back into [`crate::residency`] at the end of the run and handed
    /// to the next one, which then fails for a reason it did not cause.
    ///
    /// **That timeout is Windows' documented behaviour and not a measurement.**
    /// Nothing in this repository has ever executed on Windows; what is
    /// implemented here does not depend on the number, only on the rule that a
    /// session which has once reported the device gone is not run again.
    ///
    /// It sets a **second flag** rather than reusing `unload`, and the two are
    /// not the same thing. `unload` is a request from a person: it reaches the
    /// interface as [`Loaded::unload_requested`], which the loaded-models tab
    /// draws as an unload in progress beside the button that was pressed. A
    /// reset adapter is nobody's request, and a row that lit that state on its
    /// own would be the tab reporting an action the user did not take. What the
    /// two do share is the only thing they need to: [`Lease::spent`] is `true`
    /// for either, so every path that already gives a spent session back - the
    /// owner's own check between regions, and [`crate::residency`]'s checkin,
    /// checkout and sweep - drops a poisoned session with no new path added for
    /// it.
    ///
    /// There is no un-poison. A row is a session, and the session behind a
    /// poisoned row is one this process will not run again; the next open is a
    /// new session under a new row.
    pub fn poison(&self) {
        if let Some(entry) = table().iter_mut().find(|entry| entry.id == self.id) {
            entry.poisoned = true;
        }
    }

    /// What this row is. [`crate::residency`] reads it to sweep by kind.
    pub fn kind(&self) -> Option<Kind> {
        table().iter().find(|entry| entry.id == self.id).map(|entry| entry.kind)
    }

    /// Replace the size on the row. For the sidecar, which is the one loaded
    /// thing that reports its own resident set as it runs
    /// ([`crate::sidecar::wire::MemoryReport`]).
    pub fn resize(&self, footprint: Footprint) {
        if let Some(entry) = table().iter_mut().find(|entry| entry.id == self.id) {
            entry.bytes = footprint.bytes;
            entry.basis = footprint.basis;
        }
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        table().retain(|entry| entry.id != self.id);
    }
}

/// Every row, oldest first.
pub fn loaded() -> Vec<Loaded> {
    table()
        .iter()
        .map(|entry| Loaded {
            id: entry.id,
            kind: entry.kind,
            model_name: entry.model_name.clone(),
            bytes: entry.bytes,
            basis: entry.basis,
            device: entry.device,
            idle_ms: entry.used.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
            unload_requested: entry.unload,
        })
        .collect()
}

/// Whether anything of this kind is resident **anywhere** - held by a run, or
/// parked in [`crate::residency`] between two of them.
///
/// One question over both, which is what the callers want: the pressure
/// ladder asks it about [`Kind::Sidecar`] to decide whether refusing rung 3a is
/// a step it can take, and [`crate::detect`] asks it about
/// [`Kind::TextDetector`] to decide whether a remembered detection still has a
/// session behind it.
pub fn any_loaded(kind: Kind) -> bool {
    table().iter().any(|entry| entry.kind == kind)
}

/// Ask for one back. Answers whether there was a row to ask about - a `false`
/// is a tab that was showing something the process had already dropped, which
/// is a race the interface reports as "already gone" rather than as a failure.
pub fn request_unload(id: u64) -> bool {
    match table().iter_mut().find(|entry| entry.id == id) {
        Some(entry) => {
            entry.unload = true;
            true
        }
        None => false,
    }
}

/// Pretend a row was last used `by` ago. **For tests, and only for tests.**
///
/// The idle grace is minutes long and the thing it decides - whether a session
/// is still there when the user comes back - is the one property
/// [`crate::residency`] exists for. A test that waited five minutes to check it
/// would be a test nobody runs, and a clock behind a trait would put an
/// indirection into the hot path of every `touch` to buy a mock. So the row's
/// own timestamp is moved instead, which is the smallest thing that can be
/// wrong: the arithmetic under test is `used.elapsed() >= grace`, and this is
/// `used`.
///
/// Answers whether there was a row. A `by` larger than the process's uptime
/// saturates at "as old as this process", which is older than any grace.
#[doc(hidden)]
pub fn age(id: u64, by: Duration) -> bool {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    match table().iter_mut().find(|entry| entry.id == id) {
        Some(entry) => {
            entry.used = entry
                .used
                .checked_sub(by)
                .unwrap_or(*EPOCH.get_or_init(Instant::now));
            true
        }
        None => false,
    }
}

/// Ask for **everything** back. Nothing in the application calls this today; it
/// is what a "free memory" command would be built on, and it is here so that
/// the flag is only ever set through this module.
pub fn request_unload_all() {
    for entry in table().iter_mut() {
        entry.unload = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The lease *is* the row. This is the property the whole module rests on:
    /// a session cannot be resident without a row, because the lease is a field
    /// of the type that owns the session, and it cannot outlive one, because
    /// dropping the lease takes the row down.
    ///
    /// Written against the row's own id rather than against the table's
    /// length: `cargo test` runs this file's tests on threads of one process,
    /// so the table has other tests' rows in it and a count would be a race.
    #[test]
    fn a_row_lives_exactly_as_long_as_its_lease() {
        let id = {
            let lease = register(
                Kind::TextDetector,
                Footprint::measured(10),
                Device::accelerator(crate::accel::Accelerator::Cpu),
            );
            let rows = loaded();
            let row = rows.iter().find(|row| row.id == lease.id()).expect("no row for the lease");
            assert_eq!(row.kind, Kind::TextDetector);
            assert_eq!(row.bytes, 10);
            assert_eq!(row.basis, Basis::Measured);
            assert!(!row.unload_requested);
            lease.id()
        };
        assert!(!loaded().iter().any(|row| row.id == id), "the row outlived its lease");
    }

    /// An unload is a **request**. It sets a flag the owner reads at a safe
    /// point; nothing here reaches into a session, which is why nothing here
    /// can interrupt an inference.
    #[test]
    fn an_unload_is_a_flag_the_owner_reads_and_not_a_kill() {
        let lease = register(
            Kind::Inpainter,
            Footprint::weights(Path::new("/nonexistent")),
            Device::accelerator(crate::accel::Accelerator::Cpu),
        );
        assert!(!lease.unload_requested());
        assert!(!lease.spent(), "a fresh session is not spent");

        assert!(request_unload(lease.id()));
        assert!(lease.unload_requested());
        assert!(lease.spent(), "the owner was not told to give it back");

        // The row is still there: the session is still loaded until its owner
        // reaches a point where dropping it is safe.
        assert!(loaded().iter().any(|row| row.id == lease.id() && row.unload_requested));
    }

    /// A poison is permanent, and it is **not** an unload request: the row goes
    /// on saying nobody asked for it back, because nobody did. The interface
    /// draws `unload_requested` as an unload the user started, and a session
    /// whose device was reset under it did not start one.
    #[test]
    fn a_poisoned_row_is_spent_for_good_without_claiming_a_user_asked() {
        let lease = register(
            Kind::BalloonDetector,
            Footprint::measured(94),
            Device::accelerator(crate::accel::Accelerator::Cpu),
        );
        assert!(!lease.spent(), "a fresh session is not spent");

        lease.poison();
        assert!(lease.spent(), "the owner was not told to give the session back");
        assert!(!lease.unload_requested(), "a poison was reported as a user's unload");
        assert!(loaded().iter().any(|row| row.id == lease.id() && !row.unload_requested));

        // Permanent: touching a session is what un-spends an idle one, and it
        // does not un-spend this.
        lease.touch();
        assert!(lease.spent(), "a poison wore off");
    }

    /// A missing file is a row with an unknown size, not a missing row.
    #[test]
    fn weights_that_cannot_be_stated_are_zero_rather_than_absent() {
        let footprint = Footprint::weights(Path::new("/no/such/model.onnx"));
        assert_eq!(footprint, Footprint { bytes: 0, basis: Basis::Weights });
        // The same for the several-file form: an absent file is nothing added,
        // not a row that fails to exist.
        assert_eq!(
            Footprint::weights_of(&[Path::new("/no/such/a.onnx"), Path::new("/no/such/b.onnx")]),
            Footprint { bytes: 0, basis: Basis::Weights }
        );
    }

    /// Two files, one row, one number: the size of a [`Kind::Ocr`] row is the
    /// encoder plus the decoder, because the session it describes is both.
    #[test]
    fn several_weight_files_add_up_to_one_row() {
        let dir = std::env::temp_dir().join(format!("mc-registry-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("a.onnx"), dir.join("b.onnx"));
        std::fs::write(&a, vec![0u8; 300]).unwrap();
        std::fs::write(&b, vec![0u8; 45]).unwrap();
        assert_eq!(
            Footprint::weights_of(&[a.as_path(), b.as_path()]),
            Footprint { bytes: 345, basis: Basis::Weights }
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Asking about a row that has already gone is not a failure. The tab is a
    /// poll away from the truth by construction.
    #[test]
    fn unloading_something_that_has_already_gone_answers_false() {
        let id = {
            let lease = register(
                Kind::Sidecar,
                Footprint::reported(1024),
                Device::sidecar(),
            );
            lease.id()
        };
        assert!(!request_unload(id));
    }

    /// Every kind names a key, and the device does too. A row the interface
    /// cannot name is a row it draws blank.
    #[test]
    fn every_kind_and_device_names_a_key() {
        for kind in [
            Kind::TextDetector,
            Kind::BalloonDetector,
            Kind::ScriptGate,
            Kind::Ocr,
            Kind::Inpainter,
            Kind::Sidecar,
        ] {
            assert!(kind.label_key().starts_with("models.kind."), "{kind:?}");
        }
        assert!(Device::sidecar().label_key.starts_with("accel."));
        assert!(Device::accelerator(crate::accel::Accelerator::CoreMl).gpu);
        assert!(!Device::accelerator(crate::accel::Accelerator::Cpu).gpu);
    }

    /// Aging past process uptime saturates at the process epoch rather than
    /// failing to modify `used`.
    #[test]
    fn aging_past_process_uptime_saturates_at_epoch() {
        let lease = register(
            Kind::TextDetector,
            Footprint::measured(10),
            Device::accelerator(crate::accel::Accelerator::Cpu),
        );
        let id = lease.id();
        assert!(age(id, Duration::from_secs(1_000_000_000)));
        assert!(!age(u64::MAX, Duration::from_secs(1_000_000_000)));
    }
}
