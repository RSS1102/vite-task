use std::{
    borrow::Borrow,
    fmt::{Debug, Display},
    hash::Hash,
    ops::Deref,
    path::{Path, PathBuf},
};

use camino::{Utf8Path, Utf8PathBuf};
use ref_cast::{RefCastCustom, ref_cast_custom};
use serde::{Deserialize, Serialize};

#[cfg(feature = "absolute-redaction")]
use crate::AbsolutePath;
use crate::{
    AbsolutePathBuf,
    absolute::StripPrefixError,
    relative::{FromPathError, RelativePathBuf},
};

/// An error returned when a path cannot become an absolute UTF-8 path.
#[derive(Debug, thiserror::Error)]
pub enum AbsoluteUtf8PathError {
    /// The path is not absolute on the current platform.
    #[error("path is not absolute: {}", .0.display())]
    NonAbsolute(PathBuf),
    /// The path contains data that Rust cannot represent as UTF-8.
    #[error("path is not valid UTF-8: {}", .0.display())]
    NonUtf8(PathBuf),
}

impl AbsoluteUtf8PathError {
    /// Return the path that failed validation.
    #[must_use]
    pub fn path(&self) -> &Path {
        match self {
            Self::NonAbsolute(path) | Self::NonUtf8(path) => path.as_path(),
        }
    }

    /// Return the original path.
    #[must_use]
    pub fn into_path_buf(self) -> PathBuf {
        match self {
            Self::NonAbsolute(path) | Self::NonUtf8(path) => path,
        }
    }
}

/// A UTF-8 path that is guaranteed to be absolute.
#[derive(RefCastCustom, PartialEq, Eq, PartialOrd, Ord)]
#[repr(transparent)]
pub struct AbsoluteUtf8Path(Utf8Path);

impl AbsoluteUtf8Path {
    /// Create an absolute path from a UTF-8 path.
    ///
    /// # Errors
    ///
    /// Returns [`AbsoluteUtf8PathError::NonAbsolute`] when `path` is relative.
    pub fn new(path: &Utf8Path) -> Result<&Self, AbsoluteUtf8PathError> {
        if path.is_absolute() {
            // SAFETY: We verified that the UTF-8 path is absolute.
            Ok(unsafe { Self::assume_absolute(path) })
        } else {
            Err(AbsoluteUtf8PathError::NonAbsolute(path.as_std_path().to_path_buf()))
        }
    }

    /// Convert a standard path into an absolute UTF-8 path.
    ///
    /// # Errors
    ///
    /// Returns an error when the path is relative or contains invalid UTF-8.
    pub fn try_from_path(path: &Path) -> Result<&Self, AbsoluteUtf8PathError> {
        let Some(path) = Utf8Path::from_path(path) else {
            return Err(AbsoluteUtf8PathError::NonUtf8(path.to_path_buf()));
        };
        Self::new(path)
    }

    #[ref_cast_custom]
    unsafe fn assume_absolute(path: &Utf8Path) -> &Self;

    /// Return the underlying UTF-8 path.
    #[must_use]
    pub const fn as_utf8_path(&self) -> &Utf8Path {
        &self.0
    }

    /// Return the path as a UTF-8 string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// Return the path as a standard path.
    #[must_use]
    pub fn as_std_path(&self) -> &Path {
        self.0.as_std_path()
    }

    /// Return this path as an OS-native absolute path.
    #[must_use]
    pub fn as_absolute_path(&self) -> &crate::AbsolutePath {
        // SAFETY: `self` already guarantees absoluteness.
        unsafe { crate::AbsolutePath::assume_absolute(self.as_std_path()) }
    }

    /// Convert this path to an owned absolute UTF-8 path.
    #[must_use]
    pub fn to_absolute_utf8_path_buf(&self) -> AbsoluteUtf8PathBuf {
        // SAFETY: `self` already guarantees both invariants.
        unsafe { AbsoluteUtf8PathBuf::assume_absolute(self.0.to_path_buf()) }
    }

    /// Return a relative path after removing `base` from this path.
    ///
    /// On Windows, this ignores path namespace prefixes before matching.
    ///
    /// # Errors
    ///
    /// Returns an error if the stripped value cannot become a portable
    /// [`RelativePathBuf`].
    pub fn strip_prefix<P: AsRef<Self>>(
        &self,
        base: P,
    ) -> Result<Option<RelativePathBuf>, StripPrefixError<'_>> {
        let base = base.as_ref();
        let Ok(stripped_path) = crate::strip_path_prefix(
            self.as_std_path().as_os_str(),
            base.as_std_path().as_os_str(),
        ) else {
            return Ok(None);
        };
        match RelativePathBuf::new(stripped_path) {
            Ok(relative_path) => Ok(Some(relative_path)),
            Err(FromPathError::NonRelative) => {
                unreachable!("stripped path should always be relative")
            }
            Err(FromPathError::InvalidPathData(invalid_path_data_error)) => {
                Err(StripPrefixError { stripped_path, invalid_path_data_error })
            }
        }
    }

    /// Join a relative or absolute UTF-8 path onto this path.
    #[must_use]
    pub fn join<P: AsRef<Utf8Path>>(&self, path: P) -> AbsoluteUtf8PathBuf {
        let mut result = self.to_absolute_utf8_path_buf();
        result.push(path);
        result
    }

    /// Return the parent directory, or `None` for a root path.
    #[must_use]
    pub fn parent(&self) -> Option<&Self> {
        let parent = self.0.parent()?;
        // SAFETY: The parent of an absolute path is absolute.
        Some(unsafe { Self::assume_absolute(parent) })
    }

    /// Return the final path component.
    #[must_use]
    pub fn file_name(&self) -> Option<&str> {
        self.0.file_name()
    }

    /// Return the path extension.
    #[must_use]
    pub fn extension(&self) -> Option<&str> {
        self.0.extension()
    }

    /// Return this path with a replacement extension.
    #[must_use]
    pub fn with_extension(&self, extension: impl AsRef<str>) -> AbsoluteUtf8PathBuf {
        // SAFETY: Changing an extension preserves absoluteness and UTF-8.
        unsafe { AbsoluteUtf8PathBuf::assume_absolute(self.0.with_extension(extension)) }
    }

    /// Return whether this path ends with `path`.
    #[must_use]
    pub fn ends_with(&self, path: impl AsRef<Path>) -> bool {
        self.0.ends_with(path)
    }

    /// Normalize `.` and `..` components without accessing the filesystem.
    ///
    /// # Errors
    ///
    /// Returns an error if the normalized path does not preserve the UTF-8 or
    /// absolute-path invariant.
    pub fn clean(&self) -> Result<AbsoluteUtf8PathBuf, AbsoluteUtf8PathError> {
        use path_clean::PathClean as _;

        let cleaned = self.as_std_path().clean();
        AbsoluteUtf8PathBuf::try_from_path_buf(cleaned)
    }

    #[cfg(feature = "absolute-redaction")]
    #[expect(
        clippy::disallowed_types,
        reason = "redaction uses the existing standard path implementation"
    )]
    fn try_redact(&self) -> Result<Option<String>, String> {
        AbsolutePath::new(self.as_std_path())
            .expect("an absolute UTF-8 path is also an absolute standard path")
            .try_redact()
    }
}

impl Debug for AbsoluteUtf8Path {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Debug::fmt(&self.0, f)
    }
}

impl Display for AbsoluteUtf8Path {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&self.0, f)
    }
}

impl Hash for AbsoluteUtf8Path {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

impl Serialize for AbsoluteUtf8Path {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        #[cfg(feature = "absolute-redaction")]
        if let Some(redacted_path) = self.try_redact().map_err(serde::ser::Error::custom)? {
            return serializer.serialize_str(&redacted_path);
        }
        serializer.serialize_str(self.as_str())
    }
}

impl AsRef<Self> for AbsoluteUtf8Path {
    fn as_ref(&self) -> &Self {
        self
    }
}

impl AsRef<Utf8Path> for AbsoluteUtf8Path {
    fn as_ref(&self) -> &Utf8Path {
        self.as_utf8_path()
    }
}

impl AsRef<Path> for AbsoluteUtf8Path {
    fn as_ref(&self) -> &Path {
        self.as_std_path()
    }
}

/// An owned UTF-8 path that is guaranteed to be absolute.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AbsoluteUtf8PathBuf(Utf8PathBuf);

impl AbsoluteUtf8PathBuf {
    /// Create an absolute path from an owned UTF-8 path.
    ///
    /// # Errors
    ///
    /// Returns [`AbsoluteUtf8PathError::NonAbsolute`] when `path` is relative.
    pub fn new(path: Utf8PathBuf) -> Result<Self, AbsoluteUtf8PathError> {
        if path.is_absolute() {
            // SAFETY: We verified that the UTF-8 path is absolute.
            Ok(unsafe { Self::assume_absolute(path) })
        } else {
            Err(AbsoluteUtf8PathError::NonAbsolute(path.into_std_path_buf()))
        }
    }

    /// Convert an owned standard path into an absolute UTF-8 path.
    ///
    /// # Errors
    ///
    /// Returns an error when the path is relative or contains invalid UTF-8.
    pub fn try_from_path_buf(path: PathBuf) -> Result<Self, AbsoluteUtf8PathError> {
        let path = Utf8PathBuf::from_path_buf(path).map_err(AbsoluteUtf8PathError::NonUtf8)?;
        Self::new(path)
    }

    /// Create an absolute UTF-8 path without checking absoluteness.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `path` is absolute.
    #[must_use]
    pub const unsafe fn assume_absolute(path: Utf8PathBuf) -> Self {
        Self(path)
    }

    /// Return the borrowed absolute path.
    #[must_use]
    pub fn as_path(&self) -> &AbsoluteUtf8Path {
        // SAFETY: `self` guarantees both invariants.
        unsafe { AbsoluteUtf8Path::assume_absolute(self.0.as_path()) }
    }

    /// Return the path as a UTF-8 string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// Return the path as a standard path.
    #[must_use]
    pub fn as_std_path(&self) -> &Path {
        self.0.as_std_path()
    }

    /// Convert this path into a standard path buffer.
    #[must_use]
    pub fn into_path_buf(self) -> PathBuf {
        self.0.into_std_path_buf()
    }

    /// Convert this path into an OS-native absolute path.
    #[must_use]
    pub fn into_absolute_path_buf(self) -> AbsolutePathBuf {
        // SAFETY: `self` already guarantees absoluteness.
        unsafe { AbsolutePathBuf::assume_absolute(self.into_path_buf()) }
    }

    /// Extend this path with a relative or absolute UTF-8 path.
    pub fn push(&mut self, path: impl AsRef<Utf8Path>) {
        self.0.push(path);
    }
}

impl Serialize for AbsoluteUtf8PathBuf {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.as_path().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for AbsoluteUtf8PathBuf {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let path = Utf8PathBuf::deserialize(deserializer)?;
        Self::new(path).map_err(serde::de::Error::custom)
    }
}

impl PartialEq<AbsoluteUtf8PathBuf> for AbsoluteUtf8Path {
    fn eq(&self, other: &AbsoluteUtf8PathBuf) -> bool {
        self.0 == other.0
    }
}

impl PartialEq<AbsoluteUtf8PathBuf> for &AbsoluteUtf8Path {
    fn eq(&self, other: &AbsoluteUtf8PathBuf) -> bool {
        self.0 == other.0
    }
}

impl PartialEq<AbsoluteUtf8Path> for AbsoluteUtf8PathBuf {
    fn eq(&self, other: &AbsoluteUtf8Path) -> bool {
        self.as_path() == other
    }
}

impl PartialEq<&AbsoluteUtf8Path> for AbsoluteUtf8PathBuf {
    fn eq(&self, other: &&AbsoluteUtf8Path) -> bool {
        self.as_path() == *other
    }
}

impl AsRef<AbsoluteUtf8Path> for AbsoluteUtf8PathBuf {
    fn as_ref(&self) -> &AbsoluteUtf8Path {
        self.as_path()
    }
}

impl AsRef<Utf8Path> for AbsoluteUtf8PathBuf {
    fn as_ref(&self) -> &Utf8Path {
        self.0.as_path()
    }
}

impl AsRef<Path> for AbsoluteUtf8PathBuf {
    fn as_ref(&self) -> &Path {
        self.as_std_path()
    }
}

impl Borrow<AbsoluteUtf8Path> for AbsoluteUtf8PathBuf {
    fn borrow(&self) -> &AbsoluteUtf8Path {
        self.as_path()
    }
}

impl Deref for AbsoluteUtf8PathBuf {
    type Target = AbsoluteUtf8Path;

    fn deref(&self) -> &Self::Target {
        self.as_path()
    }
}

impl ToOwned for AbsoluteUtf8Path {
    type Owned = AbsoluteUtf8PathBuf;

    fn to_owned(&self) -> Self::Owned {
        self.to_absolute_utf8_path_buf()
    }
}

impl TryFrom<PathBuf> for AbsoluteUtf8PathBuf {
    type Error = AbsoluteUtf8PathError;

    fn try_from(path: PathBuf) -> Result<Self, Self::Error> {
        Self::try_from_path_buf(path)
    }
}

impl TryFrom<Utf8PathBuf> for AbsoluteUtf8PathBuf {
    type Error = AbsoluteUtf8PathError;

    fn try_from(path: Utf8PathBuf) -> Result<Self, Self::Error> {
        Self::new(path)
    }
}

impl TryFrom<AbsolutePathBuf> for AbsoluteUtf8PathBuf {
    type Error = AbsoluteUtf8PathError;

    fn try_from(path: AbsolutePathBuf) -> Result<Self, Self::Error> {
        Self::try_from_path_buf(path.into_path_buf())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn absolute(value: &str) -> Utf8PathBuf {
        let mut path = Utf8PathBuf::from(if cfg!(windows) { r"C:\" } else { "/" });
        path.push(value);
        path
    }

    #[test]
    fn accepts_absolute_utf8_path() {
        let path = AbsoluteUtf8PathBuf::new(absolute("测试/🚀")).unwrap();

        assert_eq!(path.as_str(), absolute("测试/🚀").as_str());
        assert_eq!(path.as_std_path(), absolute("测试/🚀").as_std_path());
    }

    #[test]
    fn rejects_relative_path() {
        let error = AbsoluteUtf8PathBuf::new(Utf8PathBuf::from("relative/path")).unwrap_err();

        assert!(matches!(error, AbsoluteUtf8PathError::NonAbsolute(_)));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_non_utf8_path() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};

        let error = AbsoluteUtf8PathBuf::try_from_path_buf(PathBuf::from(OsString::from_vec(
            b"/invalid/\xff".to_vec(),
        )))
        .unwrap_err();

        assert!(matches!(error, AbsoluteUtf8PathError::NonUtf8(_)));
        assert_eq!(error.path().as_os_str().as_encoded_bytes(), b"/invalid/\xff");
    }

    #[test]
    fn joins_and_preserves_invariants() {
        let base = AbsoluteUtf8PathBuf::new(absolute("workspace")).unwrap();
        let joined = base.join("packages/app");

        assert!(joined.as_utf8_path().is_absolute());
        assert_eq!(joined.file_name(), Some("app"));
    }

    #[test]
    fn serde_round_trip() {
        let path = AbsoluteUtf8PathBuf::new(absolute("测试/🚀")).unwrap();
        let json = serde_json::to_string(&path).unwrap();
        let decoded: AbsoluteUtf8PathBuf = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded, path);
    }
}
