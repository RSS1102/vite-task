# vt_path

Provides path types that encode path invariants:

- `AbsolutePath(Buf)` for OS-native absolute paths
- `AbsoluteUtf8Path(Buf)` for absolute UTF-8 paths
- `RelativePath(Buf)` for portable relative paths

The types provide checked conversions and joins that preserve their invariants.
