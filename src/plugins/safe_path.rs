//! Open rule paths without following filesystem links outside the granted location.

use std::{
    fs::{self, File},
    io,
    path::Path,
};

/// Report a link without opening its target.
#[cfg(any(windows, target_os = "linux"))]
fn link_denied() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "Rule file paths cannot follow filesystem links",
    )
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::{
        os::windows::fs::{MetadataExt, OpenOptionsExt},
        path::{Component, PathBuf},
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_NO_RECALL,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    /// A path whose parent handles prevent replacing checked components while it is used.
    pub(in crate::plugins) struct PinnedPath {
        path: PathBuf,
        directories: Vec<File>,
    }

    /// Opens a regular named directory or file without following the final reparse point.
    fn open_path(path: &Path, read: bool) -> io::Result<File> {
        let mut options = fs::OpenOptions::new();
        if read {
            options.read(true);
        } else {
            options.access_mode(FILE_READ_ATTRIBUTES);
        }
        let file = options
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(
                FILE_FLAG_BACKUP_SEMANTICS
                    | FILE_FLAG_OPEN_REPARSE_POINT
                    | FILE_FLAG_OPEN_NO_RECALL,
            )
            .open(path)?;
        if file.metadata()?.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(link_denied());
        }
        Ok(file)
    }

    impl PinnedPath {
        /// Open each absolute directory component while its checked ancestors are held.
        pub(in crate::plugins) fn new(path: &Path) -> io::Result<Self> {
            let mut parts = path.components().peekable();
            let mut current = PathBuf::new();
            let mut rooted = false;
            for part in parts.by_ref() {
                match part {
                    Component::Prefix(prefix)
                        if matches!(
                            prefix.kind(),
                            std::path::Prefix::Disk(_)
                                | std::path::Prefix::UNC(..)
                                | std::path::Prefix::VerbatimDisk(_)
                                | std::path::Prefix::VerbatimUNC(..)
                        ) =>
                    {
                        current.push(part.as_os_str())
                    }
                    Component::RootDir => {
                        current.push(part.as_os_str());
                        rooted = true;
                        break;
                    }
                    _ => return Err(io::Error::from(io::ErrorKind::InvalidInput)),
                }
            }
            if !rooted || !current.is_absolute() {
                return Err(io::Error::from(io::ErrorKind::InvalidInput));
            }
            let mut directories = Vec::new();
            while let Some(part) = parts.next() {
                let Component::Normal(name) = part else {
                    return Err(io::Error::from(io::ErrorKind::InvalidInput));
                };
                current.push(name);
                if parts.peek().is_some() {
                    let directory = open_path(&current, false)?;
                    if !directory.metadata()?.is_dir() {
                        return Err(io::Error::from(io::ErrorKind::NotADirectory));
                    }
                    directories.push(directory);
                }
            }
            Ok(Self {
                path: current,
                directories,
            })
        }

        /// Return the leaf metadata without opening its data stream.
        pub(in crate::plugins) fn metadata(&self) -> io::Result<fs::Metadata> {
            // Metadata-only lookup preserves Windows sharing-error retry at the actual read.
            let metadata = fs::symlink_metadata(&self.path)?;
            if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                return Err(link_denied());
            }
            Ok(metadata)
        }

        /// Open the leaf file while checked directories are still held.
        pub(in crate::plugins) fn open(&self) -> io::Result<File> {
            open_path(&self.path, true)
        }

        /// Hold the leaf directory so enumeration cannot be redirected during iteration.
        pub(in crate::plugins) fn read_dir(
            &self,
        ) -> io::Result<impl Iterator<Item = io::Result<std::ffi::OsString>> + use<>> {
            let directory = open_path(&self.path, false)?;
            let entries = fs::read_dir(&self.path)?;
            Ok(entries.map(move |entry| {
                let _ = &directory;
                entry.map(|entry| entry.file_name())
            }))
        }

        /// Duplicate the exact checked parent handle, including a volume or UNC share root.
        pub(in crate::plugins) fn parent(&self) -> io::Result<File> {
            self.directories.last().map_or_else(
                || {
                    open_path(
                        self.path.parent().ok_or(io::ErrorKind::InvalidInput)?,
                        false,
                    )
                },
                File::try_clone,
            )
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod platform {
    use super::*;
    use rustix::fs::{CWD, Dir, Mode, OFlags, openat};
    use std::{ffi::OsString, path::Component};

    /// Parent handle and exact leaf for operations relative to a verified directory.
    pub(in crate::plugins) struct PinnedPath {
        parent: File,
        name: OsString,
    }

    impl PinnedPath {
        /// Open each absolute directory without following symbolic links.
        pub(in crate::plugins) fn new(path: &Path) -> io::Result<Self> {
            #[cfg(target_os = "linux")]
            let access = OFlags::PATH;
            #[cfg(target_os = "macos")]
            let access = OFlags::RDONLY;
            let flags = access | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
            let mut parent = File::from(openat(CWD, "/", flags, Mode::empty())?);
            let mut parts = path.components();
            if parts.next() != Some(Component::RootDir) {
                return Err(io::Error::from(io::ErrorKind::InvalidInput));
            }
            let mut parts = parts.peekable();
            while let Some(part) = parts.next() {
                let Component::Normal(name) = part else {
                    return Err(io::Error::from(io::ErrorKind::InvalidInput));
                };
                if parts.peek().is_none() {
                    return Ok(Self {
                        parent,
                        name: name.to_owned(),
                    });
                }
                parent = File::from(openat(&parent, name, flags, Mode::empty())?);
            }
            Ok(Self {
                parent,
                name: ".".into(),
            })
        }

        /// Return metadata relative to the pinned parent. Linux metadata needs no data access.
        pub(in crate::plugins) fn metadata(&self) -> io::Result<fs::Metadata> {
            #[cfg(target_os = "linux")]
            {
                let file = File::from(openat(
                    &self.parent,
                    &self.name,
                    OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )?);
                let metadata = file.metadata()?;
                if metadata.file_type().is_symlink() {
                    return Err(link_denied());
                }
                Ok(metadata)
            }
            #[cfg(target_os = "macos")]
            self.open()?.metadata()
        }

        /// Open a leaf without following a symlink or waiting on a named pipe.
        pub(in crate::plugins) fn open(&self) -> io::Result<File> {
            let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
            Ok(File::from(openat(
                &self.parent,
                &self.name,
                flags,
                Mode::empty(),
            )?))
        }

        /// Enumerate names from the opened directory handle.
        pub(in crate::plugins) fn read_dir(
            &self,
        ) -> io::Result<impl Iterator<Item = io::Result<OsString>> + use<>> {
            use std::os::unix::ffi::OsStrExt;
            let directory = self.open()?;
            let entries = Dir::read_from(directory)?;
            Ok(entries.filter_map(|entry| match entry {
                Ok(entry) if matches!(entry.file_name().to_bytes(), b"." | b"..") => None,
                Ok(entry) => Some(Ok(std::ffi::OsStr::from_bytes(
                    entry.file_name().to_bytes(),
                )
                .to_owned())),
                Err(error) => Some(Err(error.into())),
            }))
        }

        /// Duplicate the checked parent handle for comparison with an installed grant.
        pub(in crate::plugins) fn parent(&self) -> io::Result<File> {
            self.parent.try_clone()
        }
    }
}

pub(super) use platform::PinnedPath;
