# garbage_frame

Repro for issue 544: the task connects a sender to its own fspy channel — the same writer API every traced process uses — and writes a frame that doesn't decode as a path access, reconstructing the in-bounds variant of the frame-stream corruption a dying or racing writer leaves behind. Collecting the traced accesses after the task succeeded must not crash the runner, and the run must not be cached from the incomplete data (the second run must stay a cache miss).

## `vt run -v garbage-frame`

**Exit code:** 101

```
$ vtt write-garbage-fspy-frame
wrote a garbage frame to the fspy channel

thread 'main' (<pid>) panicked at crates/fspy/src/ipc.rs:36:60:
called `Result::unwrap()` on an `Err` value: Io(ReadSizeLimit(8))
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
```

## `vt run -v garbage-frame`

**Exit code:** 101

```
$ vtt write-garbage-fspy-frame
wrote a garbage frame to the fspy channel

thread 'main' (<pid>) panicked at crates/fspy/src/ipc.rs:36:60:
called `Result::unwrap()` on an `Err` value: Io(ReadSizeLimit(8))
note: run with `RUST_BACKTRACE=1` environment variable to display a backtrace
```
