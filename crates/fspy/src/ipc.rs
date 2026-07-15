use std::{cell::Cell, io};

use fspy_shared::ipc::{
    PathAccess,
    channel::{Receiver, ReceiverLockGuard},
};
use tokio::task::spawn_blocking;

// Shared memory size for storing path accesses.
// 4 GiB is large enough to store path accesses in almost any realistic scenario.
// This doesn't allocate physical memory until it's actually used.
pub const SHM_CAPACITY: usize = 4 * 1024 * 1024 * 1024;

#[ouroboros::self_referencing]
pub struct OwnedReceiverLockGuard {
    /// Owns the shared memory
    receiver: Receiver,
    /// Set when `iter_path_accesses` hit a frame that failed to decode and
    /// stopped early (see [`OwnedReceiverLockGuard::is_truncated`]).
    deserialize_failed: Cell<bool>,
    /// Borrows the shared memory and owns the file lock
    #[borrows(receiver)]
    #[covariant]
    lock_guard: ReceiverLockGuard<'this>,
}

impl OwnedReceiverLockGuard {
    pub fn lock(receiver: Receiver) -> io::Result<Self> {
        Self::try_new(receiver, Cell::new(false), fspy_shared::ipc::channel::Receiver::lock)
    }

    pub async fn lock_async(receiver: Receiver) -> io::Result<Self> {
        spawn_blocking(move || Self::lock(receiver)).await.expect("lock task panicked")
    }

    pub fn iter_path_accesses(&self) -> impl Iterator<Item = PathAccess<'_>> {
        self.borrow_lock_guard().iter_frames().map_while(|frame| {
            wincode::deserialize_exact(frame).map_or_else(
                |_| {
                    // An in-bounds frame with undecodable content is the same
                    // torn state as a truncated frame tail (issue 544): a
                    // writer died or raced the reader mid-frame. Stop instead
                    // of panicking; everything past this point is unreliable.
                    self.borrow_deserialize_failed().set(true);
                    None
                },
                Some,
            )
        })
    }

    /// Whether a previous [`Self::iter_path_accesses`] iteration ended early
    /// because the shared memory contents were inconsistent (a torn frame
    /// tail or an undecodable frame — see issue 544). When true, the yielded
    /// accesses may be missing entries and must not be treated as a complete
    /// record.
    pub fn is_truncated(&self) -> bool {
        self.borrow_deserialize_failed().get() || self.borrow_lock_guard().is_tail_truncated()
    }
}
