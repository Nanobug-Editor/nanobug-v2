//! Native launch commands for backend servers distributed without executables.

use super::Server;
use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::{
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

/// Resolve an installed backend distribution that has only a script launcher.
pub(super) fn command(server: Server, root: &Path) -> Result<Command> {
    match server {
        Server::Java => java_server(root),
        Server::Kotlin => kotlin_server(root),
        Server::Php => php_server(root),
        Server::Ruby => {
            let ruby = super::super::language::executable("ruby")
                .context("Ruby is required to start Solargraph")?;
            let script = path_entries()
                .map(|bin| bin.join("solargraph"))
                .find(|script| script.is_file())
                .context("The Solargraph RubyGems binstub is unavailable on PATH")?;
            let mut command = super::super::command(&ruby, root);
            command.arg(script);
            Ok(command)
        }
        _ => bail!("{} is unavailable on PATH", server.name()),
    }
}

/// Run Phpactor where available, otherwise use Intelephense's supported Node entry.
fn php_server(root: &Path) -> Result<Command> {
    use super::super::language::{executable, npm_entry};
    if let Some(path) = executable("phpactor") {
        let mut command = super::super::command(&path, root);
        command.arg("language-server");
        return Ok(command);
    }
    if let Some(path) = executable("intelephense") {
        let mut command = super::super::command(&path, root);
        command.arg("--stdio");
        return Ok(command);
    }
    let entry = "node_modules/intelephense/lib/intelephense.js";
    let local = root.join(entry);
    let script = local
        .is_file()
        .then_some(local)
        .or_else(|| npm_entry(entry));
    let (script, node) = script.zip(executable("node")).context(
        "Install Intelephense with npm and add Node to PATH, or install Phpactor on a POSIX system",
    )?;
    let mut command = super::super::command(&node, root);
    command.arg(script).arg("--stdio");
    Ok(command)
}

/// Reuse the official JDT LS application, selecting a distinct store per opened project.
fn java_server(root: &Path) -> Result<Command> {
    let (distribution, launcher) = path_entries()
        .filter(|bin| bin.join("jdtls.py").is_file())
        .filter_map(|bin| bin.parent().map(Path::to_owned))
        .find_map(|distribution| equinox_launcher(&distribution).map(|jar| (distribution, jar)))
        .context("JDT LS is unavailable. Add its installation's bin directory to PATH")?;
    let os = match env::consts::OS {
        "windows" => "win",
        "macos" => "mac",
        _ => "linux",
    };
    let shared = distribution.join(format!("config_{os}"));
    let cache = env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(dirs::cache_dir)
        .context("The user cache directory is unavailable")?
        .join("Nanobug")
        .join("jdtls")
        .join(format!(
            "{:x}",
            Sha256::digest(root.as_os_str().as_encoded_bytes())
        ));
    let mut shared_config = OsString::from("-Dosgi.sharedConfiguration.area=");
    shared_config.push(shared);
    let mut command = java(root, "JDTLS_JAVA_HOME")?;
    command.args([
        "-Declipse.application=org.eclipse.jdt.ls.core.id1",
        "-Dosgi.bundles.defaultStartLevel=4",
        "-Declipse.product=org.eclipse.jdt.ls.core.product",
        "-Dosgi.checkConfiguration=true",
        "-Dosgi.sharedConfiguration.area.readOnly=true",
        "-Dosgi.configuration.cascaded=true",
        "--add-modules=ALL-SYSTEM",
        "--add-opens=java.base/java.util=ALL-UNNAMED",
        "--add-opens=java.base/java.lang=ALL-UNNAMED",
    ]);
    command
        .arg(shared_config)
        .arg("-jar")
        .arg(launcher)
        .arg("-data")
        .arg(cache.join("workspace"))
        .arg("-configuration")
        .arg(cache.join("config"));
    Ok(command)
}

/// Locate the handler's normal versioned JAR, including Mason's stable launcher filename.
fn equinox_launcher(distribution: &Path) -> Option<PathBuf> {
    let plugins = distribution.join("plugins");
    let stable = plugins.join("org.eclipse.equinox.launcher.jar");
    if stable.is_file() {
        return Some(stable);
    }
    fs::read_dir(plugins)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|entry| {
            entry.is_file()
                && entry
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name.starts_with("org.eclipse.equinox.launcher_") && name.ends_with(".jar")
                    })
        })
        .max()
}

/// Run the unpacked Kotlin Gradle distribution directly through Java.
fn kotlin_server(root: &Path) -> Result<Command> {
    let libraries = path_entries()
        .filter(|bin| bin.join("kotlin-language-server.bat").is_file())
        .filter_map(|bin| bin.parent().map(|parent| parent.join("lib")))
        .find(|libraries| libraries.is_dir())
        .context(
            "Kotlin Language Server is unavailable. Add its installation's bin directory to PATH",
        )?;
    let mut classpath = libraries.into_os_string();
    classpath.push(format!("{}*", std::path::MAIN_SEPARATOR));
    let mut command = java(root, "KOTLIN_JAVA_HOME")?;
    command
        .args(["-classpath"])
        .arg(classpath)
        .arg("org.javacs.kt.MainKt");
    Ok(command)
}

/// Prefer the JDK selected by `override_key`, then the common Java installation.
fn java(root: &Path, override_key: &str) -> Result<Command> {
    let executable = format!("java{}", env::consts::EXE_SUFFIX);
    let java = [override_key, "JAVA_HOME"]
        .into_iter()
        .filter_map(env::var_os)
        .map(PathBuf::from)
        .filter(|home| home.is_absolute())
        .map(|jdk| jdk.join("bin").join(&executable))
        .filter(|java| java.is_file())
        .next()
        .or_else(|| super::super::language::executable("java"))
        .with_context(|| {
            format!(
                "Install a supported JDK and set {override_key} or JAVA_HOME, or add java to PATH"
            )
        })?;
    Ok(super::super::command(&java, root))
}

/// Restrict discovery to the same absolute PATH directories used by native tools.
fn path_entries() -> impl Iterator<Item = PathBuf> {
    let path = env::var_os("PATH").unwrap_or_default();
    let entries = env::split_paths(&path)
        .filter(|entry| entry.is_absolute())
        .collect::<Vec<_>>();
    entries.into_iter()
}
