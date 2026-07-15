# garbage_frame

Repro for issue 544: the task connects a sender to its own fspy channel — the same writer API every traced process uses — and writes a frame that doesn't decode as a path access, reconstructing the in-bounds variant of the frame-stream corruption a dying or racing writer leaves behind. Collecting the traced accesses after the task succeeded must not crash the runner, and the run must not be cached from the incomplete data (the second run must stay a cache miss).

## `vt run -v garbage-frame`

```
$ vtt write-garbage-fspy-frame
wrote a garbage frame to the fspy channel


━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
    Vite+ Task Runner • Execution Summary
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

Statistics:   1 tasks • 0 cache hits • 1 cache misses
Performance:  0% cache hit rate

Task Details:
────────────────────────────────────────────────
  [1] fspy-garbage-frame#garbage-frame: $ vtt write-garbage-fspy-frame ✓
      → Not cached: file access tracking data was incomplete
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
```

## `vt run -v garbage-frame`

```
$ vtt write-garbage-fspy-frame
wrote a garbage frame to the fspy channel


━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
    Vite+ Task Runner • Execution Summary
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

Statistics:   1 tasks • 0 cache hits • 1 cache misses
Performance:  0% cache hit rate

Task Details:
────────────────────────────────────────────────
  [1] fspy-garbage-frame#garbage-frame: $ vtt write-garbage-fspy-frame ✓
      → Not cached: file access tracking data was incomplete
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
```
