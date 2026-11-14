//! Resolve the plugin installation and local state for each editor invocation.

mod migration;

use super::Trace;
use crate::plugins::{
    package::{HostCompatibility, InstallStore, read_limited},
    query::resolve_prefix,
};
use anyhow::{Context, Result, ensure};
use std::{
    ffi::OsString,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

/// Plugin storage and SDK policy selected for this editor invocation.
pub struct Options {
    /// Explicit profile for all plugin data, when supplied on the command line.
    root: Option<PathBuf>,
    /// Enables development builds and experimental interfaces in the explicit profile.
    development: bool,
    /// Whether to start plugins automatically in this session.
    pub enabled: bool,
    /// Optional destination for the startup trace.
    trace: Option<PathBuf>,
}

impl Options {
    /// Consume plugin flags in `args`, resolving profile and trace paths relative to `cwd`.
    pub fn take(args: &mut Vec<OsString>, cwd: &Path) -> Result<Self> {
        let mut root = None;
        let mut development = false;
        let mut enabled = true;
        let mut trace = None;
        let mut index = 0;
        while index < args.len() {
            match args[index].to_str() {
                Some("--plugin-profile" | "--plugin-dev-profile") => {
                    ensure!(root.is_none(), "Specify only one plugin profile");
                    development = args.remove(index) == "--plugin-dev-profile";
                    ensure!(index < args.len(), "Expected a plugin profile directory");
                    root = Some(cwd.join(args.remove(index)));
                }
                Some("--no-plugins") => {
                    args.remove(index);
                    enabled = false;
                }
                Some("--plugin-trace") => {
                    args.remove(index);
                    ensure!(index < args.len(), "Expected a plugin trace file");
                    trace = Some(cwd.join(args.remove(index)));
                }
                Some("--") => {
                    args.remove(index);
                    break;
                }
                _ => index += 1,
            }
        }
        Ok(Self {
            root,
            development,
            enabled,
            trace,
        })
    }

    /// Explicit profiles keep editor session history out of the ordinary environment.
    pub fn isolated(&self) -> bool {
        self.root.is_some()
    }

    /// Take the opt-in performance destination before opening the workspace.
    pub fn start_trace(&mut self) -> Option<Trace> {
        self.trace.take().map(Trace::new)
    }

    /// Open the plugin installation and its companion state directory after core startup.
    pub fn store(self) -> Result<InstallStore> {
        // An explicit ordinary profile has its own installation and needs no default lookup.
        if let Some(root) = &self.root
            && !self.development
        {
            return self.open_profile(root);
        }
        let legacy = dirs::data_local_dir().map(|path| path.join("Nanobug").join("plugins-v1"));
        self.store_for_editor(
            &std::env::current_exe()?,
            legacy.as_deref(),
            cfg!(debug_assertions),
        )
    }

    /// Open a self-contained profile at `root` using the invocation's SDK policy.
    fn open_profile(&self, root: &Path) -> Result<InstallStore> {
        Ok(InstallStore::with_compatibility(
            bind_profile_mode(root, self.development)?,
            HostCompatibility::current(self.development),
        ))
    }

    /// Open the installation beside `editor`, importing any previous state from `legacy`.
    /// `debug` keeps plugin data in its own `plugins` directory.
    fn store_for_editor(
        self,
        editor: &Path,
        legacy: Option<&Path>,
        debug: bool,
    ) -> Result<InstallStore> {
        let directory = editor
            .parent()
            .context("Editor installation directory is unavailable")?;
        let installation = directory.join("plugins");
        if debug && self.root.is_none() {
            return self.open_profile(&installation);
        }
        let installation = resolve_prefix(&installation)?;
        let data = resolve_prefix(&crate::app_data::directory_for(editor)?)?;
        let state = resolve_prefix(&data.join("plugins"))?;
        if let Some(root) = &self.root {
            let root = resolve_prefix(root)?;
            let legacy = legacy.map(resolve_prefix).transpose()?;
            ensure!(
                !overlap(&installation, &root)
                    && !overlap(&data, &root)
                    && !overlap(&state, &root)
                    && legacy.as_ref().is_none_or(|state| !overlap(state, &root)),
                "Choose a development directory separate from the regular plugin profile"
            );
            return self.open_profile(&root);
        }

        ensure!(
            !overlap(&state, &installation),
            "Plugin data directory overlaps the installation"
        );
        let root = bind_profile_mode(&installation, false)
            .with_context(|| format!("Open plugin installation at {}", installation.display()))?;
        let state = bind_profile_mode(&state, false)
            .with_context(|| format!("Open plugin data at {}", state.display()))?;
        let store = InstallStore::new(root).with_state_root(state);
        migration::import(legacy, &store)
            .with_context(|| format!("Import plugin data into {}", store.state_root().display()))?;
        Ok(store)
    }
}

/// Accept the `legacy` store if its optional profile mode matches the old release format.
fn check_legacy_mode(legacy: &Path) -> Result<()> {
    let mode = legacy.join("profile.mode");
    if mode.try_exists()? {
        ensure!(
            read_limited(&mode, 32)? == b"regular\n",
            "Legacy plugin data uses a development profile"
        );
    }
    Ok(())
}

/// Check whether `a` and `b` share a directory or one is contained in the other.
fn overlap(a: &Path, b: &Path) -> bool {
    // Missing directory components may still have the caller's case on Windows.
    #[cfg(windows)]
    let (a_name, b_name) = (
        a.as_os_str().to_ascii_lowercase(),
        b.as_os_str().to_ascii_lowercase(),
    );
    #[cfg(windows)]
    let (a, b) = (Path::new(&a_name), Path::new(&b_name));
    a.starts_with(b) || b.starts_with(a)
}

/// Assign `root` to ordinary or `development` mode on first use and return its canonical path.
fn bind_profile_mode(root: &Path, development: bool) -> Result<PathBuf> {
    fs::create_dir_all(root)?;
    let root = root.canonicalize()?;
    let path = root.join("profile.mode");
    let expected: &[u8] = if development {
        b"development\n"
    } else {
        b"regular\n"
    };
    let read = || read_limited(&path, 32);
    let actual = match read() {
        Ok(mode) => mode,
        Err(error)
            if error
                .downcast_ref::<io::Error>()
                .is_some_and(|error| error.kind() == io::ErrorKind::NotFound) =>
        {
            let mut staged = tempfile::NamedTempFile::new_in(&root)?;
            staged.write_all(expected)?;
            staged.as_file().sync_all()?;
            match staged.persist_noclobber(&path) {
                Ok(_) => return Ok(root),
                Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => read()?,
                Err(error) => return Err(error.into()),
            }
        }
        Err(error) => return Err(error),
    };
    ensure!(
        actual == expected,
        "Plugin profile belongs to another editor mode. Choose a separate profile directory."
    );
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::package::Installed;
    use std::thread;
    use zip::{ZipWriter, write::SimpleFileOptions};

    /// Resolve flags for a fixture editor outside the repository and its current working directory.
    fn options(cwd: &Path, flags: &[&str]) -> Result<Options> {
        let mut arguments = flags.iter().map(OsString::from).collect();
        Options::take(&mut arguments, cwd)
    }

    /// Install a real ZIP with the specified semantic version in the fixture's old store.
    fn install_fixture(old: &InstallStore, archive: &Path, version: &str) -> Result<Installed> {
        let mut zip = ZipWriter::new(fs::File::create(archive)?);
        zip.start_file("nanobug-plugin.toml", SimpleFileOptions::default())?;
        write!(
            zip,
            "name = 'Legacy fixture'\nid = 'legacy'\nversion = '{version}'\n[engines]\nnanobug = '^0.1.0'\n"
        )?;
        zip.start_file("assets/example.txt", SimpleFileOptions::default())?;
        zip.write_all(version.as_bytes())?;
        zip.finish()?;
        old.install(old.prepare(archive)?, &Default::default())
    }

    /// Concurrent first launches reuse one import, including verified files and rollback history.
    #[test]
    fn installed_editor_imports_legacy_packages_once() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let legacy = temp.path().join("user/plugins-v1");
        let installation = temp.path().join("install");
        fs::create_dir(&installation)?;
        let editor = installation.join("Nanobug.exe");
        let old = InstallStore::new(bind_profile_mode(&legacy, false)?);
        let archive = temp.path().join("source.zip");
        let previous = install_fixture(&old, &archive, "1.0.0")?;
        let installed = install_fixture(&old, &archive, "1.1.0")?;
        let identity = installed.release.manifest.identity().namespace();
        old.set_enabled(&identity, false)?;

        let open = || options(temp.path(), &[])?.store_for_editor(&editor, Some(&legacy), false);
        let (store, other) = thread::scope(|scope| -> Result<_> {
            let other = scope.spawn(open);
            Ok((open()?, other.join().expect("concurrent store open")?))
        })?;
        assert_eq!(store.root(), installation.canonicalize()?.join("plugins"));
        assert_eq!(other.root(), store.root());
        assert_eq!(
            store.state_root(),
            installation.canonicalize()?.join("data/plugins")
        );
        let rows = store.list()?;
        let [record] = rows.as_slice() else {
            anyhow::bail!("Expected the single legacy plugin");
        };
        assert!(!record.enabled);
        assert_eq!(record.release.digest, installed.release.digest);
        assert_eq!(
            record.previous.as_ref().unwrap().release.digest,
            previous.release.digest
        );
        let asset = store.verified_asset(&record.release, "assets/example.txt")?;
        assert!(asset.starts_with(store.root().join("packages")));
        assert_eq!(fs::read(asset)?, b"1.1.0");
        assert_eq!(
            fs::read(store.verified_asset(&previous.release, "assets/example.txt")?)?,
            b"1.0.0"
        );

        store.uninstall(&identity)?;
        assert!(open()?.list()?.is_empty());
        assert_eq!(old.list()?.len(), 1);
        assert!(
            old.verified_asset(&installed.release, "assets/example.txt")?
                .is_file()
        );
        assert!(store.retained()?.contains_key(&identity));
        assert!(old.retained()?.is_empty());
        Ok(())
    }

    /// A missing verified asset leaves the old index available and can be retried after repair.
    #[test]
    fn an_incomplete_import_does_not_publish_the_new_registry() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let legacy = temp.path().join("old");
        let editor = temp.path().join("Nanobug.exe");
        let old = InstallStore::new(bind_profile_mode(&legacy, false)?);
        let installed = install_fixture(&old, &temp.path().join("source.zip"), "1.0.0")?;
        let asset = old.verified_asset(&installed.release, "assets/example.txt")?;
        let bytes = fs::read(&asset)?;
        fs::write(&asset, b"altered")?;
        let open = || options(temp.path(), &[])?.store_for_editor(&editor, Some(&legacy), false);
        assert!(open().is_err());
        assert!(!temp.path().join("plugins/installed.json").exists());
        assert_eq!(old.list()?.len(), 1);
        fs::write(&asset, bytes)?;
        assert_eq!(open()?.list()?.len(), 1);
        Ok(())
    }

    /// Source builds and explicit profiles have independent stores and retain their modes.
    #[test]
    fn debug_and_explicit_profiles_are_isolated_from_the_installed_editor() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        let legacy = root.join("user/plugins-v1");
        let editor = root.join("dist/Nanobug.exe");
        let debug_editor = root.join("target/debug/nanobug.exe");
        let debug = options(&root, &[])?.store_for_editor(&debug_editor, Some(&legacy), true)?;
        assert_eq!(debug.root(), root.join("target/debug/plugins"));
        assert_eq!(debug.state_root(), debug.root());
        assert!(!legacy.exists());

        let dev = options(&root, &["--plugin-dev-profile", "test-output/dev-profile"])?;
        assert!(dev.isolated());
        let dev = dev.store_for_editor(&editor, Some(&legacy), false)?;
        assert_eq!(dev.root(), root.join("test-output/dev-profile"));
        assert_eq!(dev.state_root(), dev.root());
        assert!(dev.compatibility().experiments.contains_key("ui"));
        assert!(!root.join("dist/plugins").exists());
        assert!(
            options(&root, &["--plugin-profile", "test-output/dev-profile"])?
                .store()
                .is_err()
        );
        for path in [
            "dist/plugins",
            "dist/PLUGINS/sub",
            "dist/data",
            "dist/data/plugins",
            "user/plugins-v1",
            "dist",
        ] {
            #[cfg(not(windows))]
            if path == "dist/PLUGINS/sub" {
                continue;
            }
            assert!(
                options(&root, &["--plugin-dev-profile", path])?
                    .store_for_editor(&editor, Some(&legacy), false)
                    .is_err()
            );
        }
        Ok(())
    }
}
