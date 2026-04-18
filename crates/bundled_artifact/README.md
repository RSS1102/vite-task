# bundled_artifact

Bundle a file into the executable and materialize it to disk on demand, for
APIs that need a filesystem path (`LoadLibrary`, `LD_PRELOAD`, helper
binaries). The on-disk filename is content-addressed so repeated calls skip
writing, multiple versions coexist, and stale files are never mistaken for
current ones. See crate-level docs for details.
