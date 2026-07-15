//! Provides lock-free concurrent writing and reading of frames in a shared memory region.

use core::iter::from_fn;
use std::{
    cell::Cell,
    num::NonZeroUsize,
    ops::{Deref, DerefMut},
    ptr::slice_from_raw_parts_mut,
    sync::atomic::{AtomicI32, AtomicUsize, Ordering, fence},
};

use bytemuck::must_cast;
use fspy_shm::Shm;
use wincode::{SchemaWrite, Serialize as _, config::DefaultConfig};

// `ShmWriter` writes headers using atomic operations to prevent partial writes due to crashes,
// while `ShmReader` reads headers by simple pointer dereferences.
// This is safe because `ShmReader` is only used after all writing is done and visible to the calling thread (see docs of `ShmReader::new`).
// To ensure that the layouts of atomic types and their non-atomic counterparts are the same:
const _: () = {
    assert!(size_of::<usize>() == size_of::<AtomicUsize>());
    assert!(align_of::<usize>() == align_of::<AtomicUsize>());
    assert!(size_of::<i32>() == size_of::<AtomicI32>());
    assert!(align_of::<i32>() == align_of::<AtomicI32>());
};

/// A trait to borrow a raw memory region.
pub trait AsRawSlice {
    fn as_raw_slice(&self) -> *mut [u8];
}

impl AsRawSlice for Shm {
    fn as_raw_slice(&self) -> *mut [u8] {
        slice_from_raw_parts_mut(self.as_ptr(), self.len())
    }
}

/// A concurrent shared memory writer.
///
/// It's lock-free and safe to use across multiple threads/processes at the same time.
/// Internally it uses atomic operations to ensure that multiple writers can write to the shared memory without
/// overwriting each other's data.
pub struct ShmWriter<M> {
    /*
    Layout of the whole shared memory:
    | total byte size of frames(AtomicUsize) | frame 1 | frame 2 | ..... |

    Possible layout states of each frame:
    - | 0(AtomicI32) | 0000...... | all zero. This happens when the thread/process crashed right after the frame is claimed.
    - | byte size of the frame (AtomicI32) | partially written data | extra 0s to align to next frame header | This happens when the thread/process crashed during writing.
    - | negative byte size of the frame (AtomicI32) | fully written data | extra 0s to align to next frame header | This is the normal case (negative size indicates completion).
    */
    mem: M,

    #[cfg(test)]
    fail_on_claim: bool,
}

// unsafe impl<M: AsRawMemory> Send for ShmWriter<M> {}
// unsafe impl<M: AsRawMemory> Sync for ShmWriter<M> {}

#[track_caller]
fn assert_alignment(ptr: *const u8) {
    // Assert that the header of the shm is aligned to usize
    assert_eq!(ptr as usize % align_of::<usize>(), 0);
    // Assert that the content after whole shm header is aligned to i32
    assert_eq!((ptr as usize + size_of::<usize>()) % align_of::<i32>(), 0);
}

const fn roundup_to_align_frame_header(mut size: usize) -> usize {
    // round up new_end so that the next frame header is aligned
    const FRAME_HEADER_ALIGN: usize = align_of::<AtomicI32>();
    if !size.is_multiple_of(FRAME_HEADER_ALIGN) {
        size += FRAME_HEADER_ALIGN - (size % FRAME_HEADER_ALIGN);
    }
    size
}

pub struct FrameMut<'a> {
    header: &'a AtomicI32,
    content: &'a mut [u8],
}
impl Deref for FrameMut<'_> {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.content
    }
}
impl DerefMut for FrameMut<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.content
    }
}

impl Drop for FrameMut<'_> {
    fn drop(&mut self) {
        // Prevents compiler from ordering memory operations. Ensure the data is visible before marking as fully written
        fence(Ordering::Release);

        // Mark as fully written (negative size indicates completion)
        let frame_size_i32 =
            i32::try_from(self.content.len()).expect("frame size checked in `append_frame`");
        self.header.store(-frame_size_i32, Ordering::Relaxed);
    }
}

#[derive(thiserror::Error, Debug)]
pub enum WriteEncodedError {
    #[error("Failed to encode value into shared memory")]
    EncodeError(#[from] wincode::error::WriteError),
    #[error("Tried to write a frame of zero size into shared memory")]
    ZeroSizedFrame,
    #[error("Not enough space in shared memory to write the encoded frame")]
    InsufficientSpace,
}

impl<M: AsRawSlice> ShmWriter<M> {
    /// Create a new `ShmWriter` backed by a shared memory region.
    ///
    /// # Safety
    /// - `mem.as_raw_slice()` must return a stable valid pointer to a memory region of `total` bytes,
    /// - the memory region must only be accessed via `ShmWriter` across all the processes.
    /// - The unused region of the shared memory must be initialized to zero.
    pub unsafe fn new(mem: M) -> Self {
        assert_alignment(mem.as_raw_slice() as *const u8);
        Self {
            mem,
            #[cfg(test)]
            fail_on_claim: false,
        }
    }

    // Unwrap `self` and return the underlying memory.
    #[cfg(test)]
    pub fn into_memory(self) -> M {
        self.mem
    }

    #[cfg(test)]
    const fn set_fail_on_claim(&mut self, fail_on_claim: bool) {
        self.fail_on_claim = fail_on_claim;
    }

    /// Claim a frame of size `frame_size`.
    ///
    /// Returns `None` if there is no sufficient remaining space (or simulated crash in tests)
    /// `frame_size` must be non-zero because frame header being 0 would be ambiguous.
    pub fn claim_frame(&self, frame_size: NonZeroUsize) -> Option<FrameMut<'_>> {
        let shm_slice: *mut [u8] = self.mem.as_raw_slice();
        let shm_ptr = shm_slice.cast::<u8>();
        let shm_len = self.mem.as_raw_slice().len();

        let frame_size = frame_size.get();
        let Ok(frame_size_i32) = i32::try_from(frame_size) else {
            // The frame header uses a signed 32-bit integer (i32) to store the frame size.
            // Negative values are reserved to indicate completion, so only positive values are valid.
            // Therefore, the maximum allowed frame size is i32::MAX (2^31-1), approximately 2GB.
            // Attempting to claim a frame larger than this will fail.
            return None;
        };

        // Get the atomic value of the end position (first 8 bytes of shared memory)
        // SAFETY: `shm_ptr` points to the start of the shared memory region, which is properly
        // aligned to `usize` (verified by `assert_alignment` in `new`), and the allocation is
        // large enough to contain at least a `usize` header.
        let atomic_header = unsafe { AtomicUsize::from_ptr(shm_ptr.cast()) };

        let frame_with_header_size = size_of::<i32>() + frame_size;

        // Try to atomically claim the space
        // Different writers only share the header, not each other's content. so relaxed ordering is sufficient.
        let current_end =
            atomic_header.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current_end| {
                let new_end = roundup_to_align_frame_header(current_end + frame_with_header_size);

                // Check if we have enough space
                if size_of::<usize>() + new_end > shm_len {
                    return None;
                }

                Some(new_end)
            });

        let Ok(current_end) = current_end else {
            return None; // Not enough space
        };

        #[cfg(test)]
        if self.fail_on_claim {
            // Simulate crash right after claiming the space
            return None;
        }

        // Successfully claimed the space, now write the data

        // SAFETY: The atomic fetch_update above guaranteed that `size_of::<usize>() + current_end`
        // is within the shared memory bounds, so this pointer arithmetic stays within the allocation.
        let frame_start = unsafe {
            shm_ptr.add(/* shm header */ size_of::<usize>() + current_end)
        };

        // SAFETY: `frame_start` is properly aligned to `i32` (ensured by `roundup_to_align_frame_header`)
        // and points within the shared memory allocation (bounds checked by the atomic fetch_update).
        let frame_header = unsafe { AtomicI32::from_ptr(frame_start.cast()) };

        // Mark as partially written with positive size
        // Atomic operations on the frame header is only for preventing partial writes of the frame header itself (possibly due to crashes),
        // not for synchronization of frame contents, so relaxed ordering is sufficient
        frame_header.store(frame_size_i32, Ordering::Relaxed);

        // Prevents compiler from re-ordering memory operations. Ensure the size is visible before writing the data
        fence(Ordering::Release);

        // SAFETY: `frame_start` is within bounds and adding `size_of::<i32>()` skips the frame
        // header to reach the content area, which is still within the claimed space.
        let frame_content_ptr = unsafe { frame_start.add(size_of::<i32>()) }; // skip the frame header
        Some(FrameMut {
            header: frame_header,
            // SAFETY: `frame_content_ptr` is valid for `frame_size` bytes (guaranteed by the
            // atomic space claim), properly aligned for `u8`, and no other writer will access
            // this region because each writer atomically claims a unique range.
            content: unsafe { std::slice::from_raw_parts_mut(frame_content_ptr, frame_size) },
        })
    }

    /// Append an encoded value into the shared memory.
    pub fn write_encoded<T: SchemaWrite<DefaultConfig, Src = T>>(
        &self,
        value: &T,
    ) -> Result<(), WriteEncodedError> {
        let serialized_size =
            usize::try_from(T::serialized_size(value)?).expect("serialized size exceeds usize");

        let Some(frame_size) = NonZeroUsize::new(serialized_size) else {
            return Err(WriteEncodedError::ZeroSizedFrame);
        };
        let Some(mut frame) = self.claim_frame(frame_size) else {
            return Err(WriteEncodedError::InsufficientSpace);
        };

        let mut writer: &mut [u8] = &mut frame;
        T::serialize_into(&mut writer, value)?;
        assert_eq!(writer.len(), 0);

        Ok(())
    }

    #[cfg(test)]
    pub fn try_write_frame(&self, frame: &[u8]) -> bool {
        let Some(frame_size) = NonZeroUsize::new(frame.len()) else {
            return false;
        };
        let Some(mut frame_mut) = self.claim_frame(frame_size) else {
            return false;
        };
        frame_mut.copy_from_slice(frame);
        true
    }
}

/// Reader of frames in shared memory created by `ShmWriter`.
pub struct ShmReader<M: AsRef<[u8]>> {
    mem: M,
    /// Set when [`Self::iter_frames`] found a frame whose recorded extent
    /// doesn't fit in the used region and stopped early. See
    /// [`Self::is_tail_truncated`].
    tail_truncated: Cell<bool>,
}

impl<M: AsRef<[u8]>> ShmReader<M> {
    /// The content of `mem` should be created by `ShmWriter`.
    /// Failing to do so may result in missing frames (see [`Self::is_tail_truncated`]), but won't trigger undefined behavior.
    ///
    /// The `ShmReader` must be created after all writing to the shared memory is done and visible to the calling thread.
    /// This is guaranteed by `M: AsRef<[u8]>`, which means the memory region is immutable during the lifetime of `ShmReader`,
    /// so no need to mark `ShmReader::new` as unsafe, but care must be taken to create a safe `M` from the shared memory.
    pub fn new(mem: M) -> Self {
        assert_alignment(mem.as_ref().as_ptr());
        Self { mem, tail_truncated: Cell::new(false) }
    }

    /// Whether a [`Self::iter_frames`] iteration ended early because the
    /// remaining buffer contents were inconsistent — a frame size pointing
    /// past the used region.
    ///
    /// That state means a writer broke the quiescence contract (it died or
    /// was still writing between claiming a frame and completing it — see
    /// issue 544), so every frame from the inconsistency on is unreliable
    /// and is not yielded. Callers that need complete data must treat the
    /// whole read as untrustworthy when this returns `true`.
    pub const fn is_tail_truncated(&self) -> bool {
        self.tail_truncated.get()
    }

    /// Iterate over all the frames in the shared memory.
    ///
    /// A frame whose recorded extent doesn't fit in the used region ends the
    /// iteration early instead of panicking; [`Self::is_tail_truncated`]
    /// reports that afterwards.
    pub fn iter_frames(&self) -> impl Iterator<Item = &[u8]> {
        let mem = self.mem.as_ref();
        let (header, content) = mem
            .split_first_chunk::<{ size_of::<usize>() }>()
            .expect("mem too small to contain header");
        let content_size: usize = must_cast(*header);
        let mut remaining_content = content.get(..content_size).unwrap_or_else(|| {
            // The recorded end offset overruns the mapping. No frame can be
            // trusted; yield nothing.
            self.tail_truncated.set(true);
            &[]
        });

        from_fn(move || {
            let frame_size = loop {
                // looking for the next valid frame
                let (frame_header, next_remaining_content) =
                    remaining_content.split_first_chunk::<{ size_of::<i32>() }>()?;
                remaining_content = next_remaining_content;
                let frame_header: i32 = must_cast(*frame_header);
                match frame_header {
                    0 => {
                        // frame was claimed but never written (crashed process)
                        // Keep reading until we find a non-zero header
                    }
                    1.. => {
                        // Partially written frame - skip it and continue
                        let size = usize::try_from(frame_header).unwrap();
                        let Some(rest) =
                            remaining_content.get(roundup_to_align_frame_header(size)..)
                        else {
                            // The skip overruns the used region: these bytes
                            // were never a frame header (e.g. frame content
                            // reached by walking over a claimed-but-unwritten
                            // frame that a dying or racing writer later
                            // filled — issue 544). Stop; the tail is lost.
                            self.tail_truncated.set(true);
                            return None;
                        };
                        remaining_content = rest;
                    }
                    ..0 => {
                        // Fully written frame (negative size indicates completion)
                        break usize::try_from(-frame_header).unwrap();
                    }
                }
            };

            let padded_size = roundup_to_align_frame_header(frame_size);
            if padded_size > remaining_content.len() {
                // Same misparse as above, with the garbage bytes read as a
                // "complete" frame header.
                self.tail_truncated.set(true);
                return None;
            }
            let (frame_with_padding, next_remaining_content) =
                remaining_content.split_at(padded_size);
            remaining_content = next_remaining_content;

            Some(&frame_with_padding[..frame_size])
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{
        process::{Child, Command},
        sync::Arc,
        thread,
    };

    use assert2::assert;
    use bstr::BStr;
    use rustc_hash::FxHashSet;

    use super::*;

    /// A mocked shared memory region for testing.
    ///
    /// To be testable for miri, the shared memory is allocated using `Arc` instead of real shared memory APIs.
    #[derive(Clone)]
    struct MockedShm {
        // Why usize: to ensure alignment
        //
        // Why not Arc<[usize]>:
        // According to miri, from the perspective of data racing, incrementing ref count of Arc<[T]>
        // is considered the same as reading the content of [T], which conflicts with writing to [T] by `ShmWriter`.
        // This problem is unrelated to real shared memory.
        mem: Arc<Vec<usize>>,
        /// The actual requested byte length.
        ///
        /// over-allocation might happen to ensure alignment of `usize`, so `mem.len()` might be inaccurate.
        len: usize,
    }
    // SAFETY: `MockedShm` uses `Arc<Vec<usize>>` for its backing memory, which is safe to send
    // across threads. The raw pointer access through `AsRawSlice` is synchronized by `ShmWriter`'s
    // atomic operations.
    unsafe impl Send for MockedShm {}
    // SAFETY: Concurrent access to the shared memory is synchronized by `ShmWriter`'s atomic
    // operations. The `Arc` wrapper ensures the allocation remains valid.
    unsafe impl Sync for MockedShm {}
    impl MockedShm {
        fn alloc(len: usize) -> Self {
            // allocates this many of usize to fit the requested byte size
            let size_in_usize = len / size_of::<usize>() + 1;

            let mem: Vec<usize> = std::iter::repeat_n(0usize, size_in_usize).collect();

            Self { mem: Arc::new(mem), len }
        }
    }
    impl AsRef<[u8]> for MockedShm {
        fn as_ref(&self) -> &[u8] {
            // SAFETY: `Vec::as_ptr` returns a valid pointer to the vec's buffer. The vec is
            // allocated with enough `usize` elements to cover `self.len` bytes, and the pointer
            // is valid for reads of `self.len` bytes. The `Arc` ensures the allocation is alive.
            unsafe { std::slice::from_raw_parts(Vec::as_ptr(&self.mem).cast(), self.len) }
        }
    }

    impl AsRawSlice for MockedShm {
        fn as_raw_slice(&self) -> *mut [u8] {
            slice_from_raw_parts_mut(Vec::as_ptr(&self.mem).cast::<u8>().cast_mut(), self.len)
        }
    }

    /// Reproduce the byte-level state from issue 544 through raw writes: bump
    /// the end offset to cover a frame extent, leave the frame header zero,
    /// and fill in content bytes. This is what a writer that dies — or races
    /// the reader — between claiming a frame and completing it leaves behind;
    /// the writer API can never produce it on its own (the header is always
    /// stored before any content).
    fn append_torn_frame(shm: &MockedShm, content: &[u8]) {
        let base = shm.as_raw_slice().cast::<u8>();
        let extent = roundup_to_align_frame_header(size_of::<i32>() + content.len());
        // SAFETY: the mapping starts with the writers' shared `AtomicUsize`
        // end offset; the allocation is valid and aligned for `usize`.
        let end_offset = unsafe { AtomicUsize::from_ptr(base.cast()) };
        let frame_offset = end_offset.fetch_add(extent, Ordering::Relaxed);
        // SAFETY: the extent claimed above is exclusively ours and within the
        // allocation (the callers stay far below the capacity).
        unsafe {
            let frame = base.add(size_of::<usize>() + frame_offset);
            std::ptr::copy_nonoverlapping(
                content.as_ptr(),
                frame.add(size_of::<i32>()),
                content.len(),
            );
        }
    }

    #[test]
    fn single_thread_basic() {
        // SAFETY: `MockedShm::alloc` provides a valid, properly-sized, zero-initialized allocation.
        let writer = unsafe { ShmWriter::new(MockedShm::alloc(1024)) };
        assert!(writer.try_write_frame(b"hello"));
        assert!(writer.try_write_frame(b"world"));
        assert!(writer.try_write_frame(b"this is a test"));
        assert!(!writer.try_write_frame(&vec![0u8; 2048])); // too large

        let reader = ShmReader::new(writer.into_memory());
        let mut frames = reader.iter_frames();
        assert_eq!(frames.next().unwrap(), b"hello");
        assert_eq!(frames.next().unwrap(), b"world");
        assert_eq!(frames.next().unwrap(), b"this is a test");
        assert_eq!(frames.next(), None);
        assert!(!reader.is_tail_truncated());
    }

    #[test]
    fn torn_frame_truncates_tail_as_partial_frame() {
        // Walking over the torn frame's zero header lands on its content:
        // b"\0/pr" reads as the positive (partially-written) frame header
        // 0x7270_2F00, whose skip length overruns the used region. The exact
        // state — including the header value — observed in issue 544.
        let shm = MockedShm::alloc(1024);
        // SAFETY: `MockedShm::alloc` provides a valid, properly-sized, zero-initialized allocation.
        let writer = unsafe { ShmWriter::new(shm.clone()) };
        assert!(writer.try_write_frame(b"foo"));
        append_torn_frame(&shm, b"\0/private/tmp/some/staged/path");
        assert!(writer.try_write_frame(b"bar"));

        let reader = ShmReader::new(shm);
        let mut frames = reader.iter_frames();
        assert_eq!(frames.next().unwrap(), b"foo");
        // The torn frame ends the iteration: everything behind it (even the
        // valid "bar" frame) is unreachable because the walk lost its frame
        // alignment.
        assert_eq!(frames.next(), None);
        assert!(reader.is_tail_truncated());
    }

    #[test]
    fn torn_frame_truncates_tail_as_complete_frame() {
        // Content bytes with the high bit set read as a negative ("complete")
        // frame header whose extent overruns the used region.
        let shm = MockedShm::alloc(1024);
        // SAFETY: `MockedShm::alloc` provides a valid, properly-sized, zero-initialized allocation.
        let writer = unsafe { ShmWriter::new(shm.clone()) };
        assert!(writer.try_write_frame(b"foo"));
        append_torn_frame(&shm, b"\xF0\xF1\xF2\xF3 torn frame content");

        let reader = ShmReader::new(shm);
        let mut frames = reader.iter_frames();
        assert_eq!(frames.next().unwrap(), b"foo");
        assert_eq!(frames.next(), None);
        assert!(reader.is_tail_truncated());
    }
    #[test]
    fn single_thread_empty() {
        // SAFETY: `MockedShm::alloc` provides a valid, properly-sized, zero-initialized allocation.
        let writer = unsafe { ShmWriter::new(MockedShm::alloc(1024)) };
        assert!(writer.try_write_frame(b"hello"));
        assert!(!writer.try_write_frame(b""));
        assert!(writer.try_write_frame(b"this is a test"));

        let reader = ShmReader::new(writer.into_memory());
        let mut frames = reader.iter_frames();
        assert_eq!(frames.next().unwrap(), b"hello");
        assert_eq!(frames.next().unwrap(), b"this is a test");
        assert_eq!(frames.next(), None);
    }

    #[test]
    fn single_thread_crash_after_claim() {
        // SAFETY: `MockedShm::alloc` provides a valid, properly-sized, zero-initialized allocation.
        let mut writer = unsafe { ShmWriter::new(MockedShm::alloc(1024)) };
        assert!(writer.try_write_frame(b"foo"));

        // Simulate crash during writing
        writer.set_fail_on_claim(true);
        assert!(!writer.try_write_frame(b"hello"));

        writer.set_fail_on_claim(false);
        assert!(writer.try_write_frame(b"bar"));

        let reader = ShmReader::new(writer.into_memory());
        let mut frames = reader.iter_frames();
        assert_eq!(frames.next().unwrap(), b"foo");
        assert_eq!(frames.next().unwrap(), b"bar");
        assert_eq!(frames.next(), None);
    }

    #[test]
    fn single_thread_crash_partial_write() {
        // SAFETY: `MockedShm::alloc` provides a valid, properly-sized, zero-initialized allocation.
        let writer = unsafe { ShmWriter::new(MockedShm::alloc(1024)) };
        assert!(writer.try_write_frame(b"foo"));

        // Simulate crash during writing
        let mut frame = writer.claim_frame(5.try_into().unwrap()).unwrap();
        frame[..3].copy_from_slice(b"wor");
        std::mem::forget(frame);

        assert!(writer.try_write_frame(b"bar"));

        let reader = ShmReader::new(writer.into_memory());
        let mut frames = reader.iter_frames();
        assert_eq!(frames.next().unwrap(), b"foo");
        assert_eq!(frames.next().unwrap(), b"bar");
        assert_eq!(frames.next(), None);
    }

    #[test]
    fn single_thread_two_crashes_after_claim_and_partial_write() {
        // This test verifies that ShmReader::iter correctly handles MULTIPLE consecutive
        // invalid frames by continuing the loop. It's crucial for testing
        // that the reader doesn't stop at the first invalid frame but keeps processing
        // through multiple crash scenarios to find valid frames beyond them.

        // SAFETY: `MockedShm::alloc` provides a valid, properly-sized, zero-initialized allocation.
        let mut writer = unsafe { ShmWriter::new(MockedShm::alloc(1024)) };

        assert!(writer.try_write_frame(b"foo"));

        // First crash: AfterClaim (leaves frame header as 0)
        writer.set_fail_on_claim(true);
        assert!(!writer.try_write_frame(b"world"));
        writer.set_fail_on_claim(false);

        // Second crash: PartialWrite (leaves positive frame header)
        let mut frame = writer.claim_frame(5.try_into().unwrap()).unwrap();
        frame[..3].copy_from_slice(b"wor");
        std::mem::forget(frame);

        assert!(writer.try_write_frame(b"bar"));

        // ShmReader must skip BOTH invalid frames (0 header + partial header)
        // and find the valid frame beyond them - this tests the loop continuation

        let reader = ShmReader::new(writer.into_memory());
        let mut frames = reader.iter_frames();
        assert_eq!(frames.next().unwrap(), b"foo");
        assert_eq!(frames.next().unwrap(), b"bar");
        assert_eq!(frames.next(), None);
    }

    #[test]
    fn single_thread_two_crashes_partial_write_and_after_claim() {
        // SAFETY: `MockedShm::alloc` provides a valid, properly-sized, zero-initialized allocation.
        let mut writer = unsafe { ShmWriter::new(MockedShm::alloc(1024)) };
        // This test verifies the same loop continuation behavior but with crashes
        // in reverse order. This ensures the loop correctly handles different
        // sequences of invalid frame types (partial write -> after claim).

        assert!(writer.try_write_frame(b"foo"));

        // First crash: PartialWrite (leaves positive frame header)
        let mut frame = writer.claim_frame(5.try_into().unwrap()).unwrap();
        frame[..3].copy_from_slice(b"wor");
        std::mem::forget(frame);

        // Second crash: AfterClaim (leaves frame header as 0)
        writer.set_fail_on_claim(true);
        assert!(!writer.try_write_frame(b"world"));
        writer.set_fail_on_claim(false);

        assert!(writer.try_write_frame(b"bar"));

        let reader = ShmReader::new(writer.into_memory());
        // ShmReader must skip BOTH invalid frames in this order and continue
        // processing to find valid frames - tests loop robustness
        let mut frames = reader.iter_frames();
        assert_eq!(frames.next().unwrap(), b"foo");
        assert_eq!(frames.next().unwrap(), b"bar");
        assert_eq!(frames.next(), None);
    }

    #[test]
    fn concurrent() {
        let shm = MockedShm::alloc(1024 * 4);

        thread::scope(|s| {
            for _ in 0..4 {
                s.spawn(|| {
                    // SAFETY: `MockedShm::alloc` provides a valid, properly-sized, zero-initialized
                    // allocation. The clone shares the same backing memory, which is safe because
                    // `ShmWriter` uses atomic operations for concurrent access.
                    let writer = unsafe { ShmWriter::new(shm.clone()) };
                    for _ in 0..10 {
                        assert!(writer.try_write_frame(b"hello"));
                        assert!(writer.try_write_frame(b"foo"));
                        assert!(writer.try_write_frame(b"this is a test"));
                    }
                });
            }
        });
        let mut count = 0;
        let reader = ShmReader::new(shm);
        for frame in reader.iter_frames() {
            count += 1;
            let frame = BStr::new(frame);
            assert!(frame == b"hello" || frame == b"foo" || frame == b"this is a test");
        }
        assert_eq!(count, 120);
    }

    #[test]
    fn concurrent_exceeded_size() {
        // SAFETY: `MockedShm::alloc` provides a valid, properly-sized, zero-initialized allocation.
        let writer = unsafe { ShmWriter::new(MockedShm::alloc(1024)) };
        thread::scope(|s| {
            for _ in 0..4 {
                s.spawn(|| {
                    for _ in 0..10 {
                        writer.try_write_frame(b"hello");
                        writer.try_write_frame(b"foo");
                        writer.try_write_frame(b"this is a test");
                    }
                });
            }
        });
        let mut count = 0;
        let reader = ShmReader::new(writer.into_memory());
        for frame in reader.iter_frames() {
            count += 1;
            let frame = BStr::new(frame);
            assert!(frame == b"hello" || frame == b"foo" || frame == b"this is a test");
        }
        assert!(count > 50);
    }

    #[test]
    fn test_integer_overflow_space_calculation() {
        // Test case for potential integer overflow in space calculation

        // SAFETY: `MockedShm::alloc` provides a valid, properly-sized, zero-initialized allocation.
        let writer = unsafe { ShmWriter::new(MockedShm::alloc(1024)) };

        // Try to trigger integer overflow by using maximum values
        let large_frame = vec![0u8; (i32::MAX as usize) - 100];

        // This should fail safely, not cause overflow
        assert!(!writer.try_write_frame(&large_frame));

        // Small frame should still work
        assert!(writer.try_write_frame(b"test"));

        let reader = ShmReader::new(writer.into_memory());
        let mut frames = reader.iter_frames();
        assert_eq!(frames.next().unwrap(), b"test");
        assert_eq!(frames.next(), None);
    }

    #[test]
    fn test_space_calculation_race_condition() {
        // Test for race condition in space calculation where multiple threads
        // might calculate overlapping space requirements

        // SAFETY: `MockedShm::alloc` provides a valid, properly-sized, zero-initialized allocation.
        let writer = unsafe { ShmWriter::new(MockedShm::alloc(200)) };

        // Very small buffer
        thread::scope(|s| {
            for _ in 0..10 {
                s.spawn(|| {
                    // Many threads trying to write large-ish frames
                    writer
                        .try_write_frame(b"this_is_a_moderately_long_frame_that_might_cause_races");
                });
            }
        });

        // The exact count doesn't matter, but the reader should not panic
        // and should handle any race conditions gracefully

        let reader = ShmReader::new(writer.into_memory());
        let mut count = 0;
        for _frame in reader.iter_frames() {
            count += 1;
        }
        // At least some but not all writes should succeed
        assert!(count > 0);
        assert!(count < 10);
    }

    #[test]
    fn test_alignment_violation_detection() {
        struct Misaligned(MockedShm);
        impl AsRawSlice for Misaligned {
            fn as_raw_slice(&self) -> *mut [u8] {
                let raw_slice = self.0.as_raw_slice();
                slice_from_raw_parts_mut(
                    // SAFETY: Adding 1 byte to create a deliberately misaligned pointer for testing.
                    // The original allocation is large enough that adding 1 byte stays within bounds.
                    unsafe { raw_slice.cast::<u8>().add(1) },
                    raw_slice.len() - 1,
                )
            }
        }
        // Test that alignment violations are properly detected

        // Allocate memory with proper alignment first
        let shm = MockedShm::alloc(64);

        // Create a deliberately misaligned pointer by adding 1 byte
        // This ensures the pointer is NOT aligned to usize boundary
        let misaligned_shm = Misaligned(shm);

        // Verify the pointer is actually misaligned
        assert_ne!(misaligned_shm.as_raw_slice().cast::<u8>() as usize % align_of::<usize>(), 0);

        // This should panic due to alignment assertion
        let result = std::panic::catch_unwind(|| {
            // SAFETY: Intentionally passing a misaligned pointer to test that the alignment
            // assertion in `ShmWriter::new` correctly panics. This is expected to panic.
            unsafe { ShmWriter::new(misaligned_shm) };
        });

        // Verify that the alignment check properly caught the violation
        assert!(result.is_err(), "Should panic on misaligned pointer");
    }

    #[test]
    #[cfg(not(miri))]
    fn real_shm_across_processes() {
        use subprocess_test::command_for_fn;

        const CHILD_COUNT: usize = 12;
        const FRAME_COUNT_EACH_CHILD: usize = 100;

        const SHM_SIZE: usize = 1024 * 1024;

        // On Linux, `fspy_shm::create` spawns the mapping's broker onto the
        // ambient tokio runtime, which serves the child processes' opens.
        #[cfg(target_os = "linux")]
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_io()
            .enable_time()
            .build()
            .unwrap();
        #[cfg(target_os = "linux")]
        let _guard = runtime.enter();

        let shm = fspy_shm::create(SHM_SIZE).unwrap();
        let shm_name = shm.id().to_owned();

        let children: Vec<Child> = (0..CHILD_COUNT)
            .map(|child_index| {
                let cmd = command_for_fn!(
                    (shm_name.clone(), child_index),
                    |(shm_name, child_index): (String, usize)| {
                        let shm = fspy_shm::open(&shm_name).unwrap();
                        // SAFETY: `shm` is a freshly opened shared memory region with a valid
                        // pointer and size. Concurrent write access is safe because `ShmWriter`
                        // uses atomic operations.
                        let writer = unsafe { ShmWriter::new(shm) };
                        for i in 0..FRAME_COUNT_EACH_CHILD {
                            let frame_data = std::format!("{child_index} {i}");
                            assert!(writer.try_write_frame(frame_data.as_bytes()));
                        }
                    }
                );
                Command::from(cmd).spawn().unwrap()
            })
            .collect();

        for mut c in children {
            let status = c.wait().unwrap();
            assert!(status.success());
        }

        // SAFETY: All child processes have exited (waited above), so no concurrent writers exist.
        // The shared memory is valid and fully written.
        let shm = unsafe { shm.as_slice() };
        let reader = ShmReader::new(shm);
        let frames = reader.iter_frames().map(BStr::new).collect::<FxHashSet<&BStr>>();
        assert_eq!(frames.len(), CHILD_COUNT * FRAME_COUNT_EACH_CHILD);
        for child_index in 0..CHILD_COUNT {
            for i in 0..FRAME_COUNT_EACH_CHILD {
                let frame_data = format!("{child_index} {i}");
                assert!(frames.contains(&BStr::new(frame_data.as_bytes())));
            }
        }
    }
}
