//! `vtt write-garbage-fspy-frame` — writes a frame that is not a valid path
//! access into the fspy channel of the current task (issue 544).
//!
//! The frame goes through the channel's own `Sender` API — the same interface
//! every traced process writes through — so the channel-level frame is
//! perfectly well formed; only its content doesn't decode as a `PathAccess`.
//! This reconstructs the in-bounds variant of the frame-stream corruption
//! described in issue 544 (a misparsed frame yielding garbage path-access
//! records): collecting the traced accesses after the task succeeded must not
//! crash the runner, and the incomplete data must not produce a cache entry.

#[cfg(any(all(unix, not(target_env = "musl")), windows))]
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    use std::num::NonZeroUsize;

    // Obtain the task's channel configuration from the payload fspy hands
    // every traced process, and connect a sender to it like any tracked
    // writer would.
    #[cfg(all(unix, not(target_env = "musl")))]
    let sender = {
        let payload = fspy_shared_unix::payload::decode_payload_from_env()
            .map_err(|err| format!("failed to decode fspy payload: {err}"))?;
        payload.payload.ipc_channel_conf.sender()?
    };
    #[cfg(windows)]
    let sender = {
        let mut payload_len: u32 = 0;
        // SAFETY: FFI call; `PAYLOAD_ID` is a static GUID and `payload_len` a
        // valid out-pointer.
        let payload_ptr = unsafe {
            fspy_detours_sys::DetourFindPayloadEx(
                &fspy_shared::windows::PAYLOAD_ID,
                &raw mut payload_len,
            )
        }
        .cast::<u8>();
        if payload_ptr.is_null() {
            return Err("fspy Detours payload not found; is this process traced by fspy?".into());
        }
        // SAFETY: `DetourFindPayloadEx` returned a non-null pointer to
        // `payload_len` bytes that live for the rest of the process.
        let payload_bytes =
            unsafe { std::slice::from_raw_parts(payload_ptr, payload_len.try_into()?) };
        let payload: fspy_shared::windows::Payload<'_> = wincode::deserialize_exact(payload_bytes)
            .map_err(|err| format!("failed to decode fspy payload: {err}"))?;
        payload.channel_conf.sender()?
    };

    // A single-byte frame: the byte decodes as an `AccessMode`, after which
    // the path is missing, so `PathAccess` deserialization always fails.
    let frame_size = NonZeroUsize::new(1).expect("1 is non-zero");
    let mut frame = sender.claim_frame(frame_size).ok_or("no space left in the fspy channel")?;
    frame.copy_from_slice(&[1]);
    // Dropping the claimed frame marks it as completely written.
    drop(frame);

    println!("wrote a garbage frame to the fspy channel");
    Ok(())
}

#[cfg(not(any(all(unix, not(target_env = "musl")), windows)))]
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    Err("vtt write-garbage-fspy-frame requires the fspy shared-memory channel, \
         which does not exist on musl targets"
        .into())
}
