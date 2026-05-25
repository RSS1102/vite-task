# IPC Transport

Cross-platform IPC via `interprocess` crate:

| Platform           | Type               |
| ------------------ | ------------------ |
| Unix (macOS/Linux) | Unix domain socket |
| Windows            | Named pipe         |

The socket path or pipe name is passed to the task process via an env var shared between `vite_task_server` and `vite_task_client` (the specific name is an implementation detail). Clients check for its presence and skip IPC gracefully if absent.

## Server Model

One listener per task execution. The runner creates a new socket just before spawning the task and tears it down after the task exits.

The listener runs an accept loop and handles multiple concurrent clients — build tools may spawn worker processes or threads that each connect independently.

Platform differences are handled via `#[cfg(unix)]` / `#[cfg(windows)]`.
