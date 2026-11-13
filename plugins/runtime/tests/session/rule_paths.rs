//! Exercise package-scoped external read approval through real rules and worker processes.

use super::*;

fn external_bundle(
    archive: &Path,
    version: &str,
    approved: &[&str],
    wanted: &str,
    unapproved: &str,
    broken: bool,
) {
    let extra = if approved.is_empty() {
        "permissions=['read_project']".to_owned()
    } else {
        format!(
            "permissions=['read_project','read_rule_paths']\nrule_paths={}",
            serde_json::to_string(approved).unwrap(),
        )
    };
    let manifest = format!(
        "name='External Rules'\nid='external-rules'\nversion={version:?}\n{extra}\n\
         [engines]\nnanobug='^0.1.0'\n[[modules]]\nid='workspace'\nwasm='main.wasm'\nrule='detect.js'\n"
    );
    let script = format!(
        r#"
        const path = {};
        const sibling = {};
        if (await host.readFile(sibling).then(() => true, () => false))
            throw new Error('Unapproved external sibling was readable');
        const present = await host.exists(path);
        if (await host.list(path).then(() => true, () => false))
            throw new Error('An exact file approval allowed a directory listing');
        if (!present) return false;
        const data = await host.readFile(path, {{limit:32}});
        const bytes = await host.readBytes(path, {{limit:1}});
        const matches = await host.scanText(path, 'allowed');
        return data?.text === 'allowed' && bytes?.bytes[0] === 97 && matches?.offsets[0] === 0;
    "#,
        serde_json::to_string(wanted).unwrap(),
        serde_json::to_string(unapproved).unwrap()
    );
    bundle_guest(
        archive,
        &manifest,
        &script,
        r#"{"message":"fixture ran"}"#,
        broken,
    );
}

/// Obtain the fixture's native spelling without temporary-directory symlink aliases.
fn fixture_root(path: &Path) -> anyhow::Result<PathBuf> {
    let root = path.canonicalize()?;
    #[cfg(windows)]
    let root = PathBuf::from(root.to_str().unwrap().strip_prefix(r"\\?\").unwrap());
    Ok(root)
}

#[test]
fn exact_external_rule_paths_are_reviewed_and_invalidate_without_widening_read_project()
-> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let external = fixture_root(temp.path())?.join("external");
    fs::create_dir(&external)?;
    let first = external.join("first.txt");
    let second = external.join("second.txt");
    let sibling = external.join("sibling.txt");
    let denied = external.join("denied.txt");
    fs::write(&sibling, "unapproved")?;
    fs::write(&denied, "unapproved")?;
    let (first_path, second_path, sibling_path) = (
        first.to_str().unwrap(),
        second.to_str().unwrap(),
        sibling.to_str().unwrap(),
    );
    let workspace = project(temp.path(), 1);
    let profile = temp.path().join("profile");
    let editor = session(&profile, false);
    let archive = temp.path().join("fixture.zip");
    external_bundle(&archive, "1.0.0", &[], first_path, sibling_path, false);
    let namespace = install(&editor, archive.clone(), "1.0.0");
    let owner = key(&namespace, workspace.id);
    editor.projects(Some(1), vec![workspace]);
    wait(
        &editor,
        |view| matches!(view.modules.get(&owner), Some(State::Failed(reason)) if reason.contains("outside the project")),
    );

    external_bundle(
        &archive,
        "1.1.0",
        &[first_path],
        first_path,
        sibling_path,
        false,
    );
    editor.send(Action::InspectZip(archive.clone()))?;
    let review = wait(&editor, |view| view.review.is_some());
    let review = review.review.as_ref().unwrap();
    assert_eq!(
        review.additional.iter().copied().collect::<Vec<_>>(),
        [Permission::ReadRulePaths]
    );
    assert_eq!(&review.additional_rule_paths, std::slice::from_ref(&first));
    editor.send(Action::Approve(review.token))?;
    wait(&editor, |view| {
        view.modules.get(&owner) == Some(&State::Unmatched)
    });
    fs::write(&first, "allowed")?;
    wait(&editor, |view| {
        view.modules.get(&owner) == Some(&State::Ready)
    });
    fs::write(&first, "changed")?;
    wait(&editor, |view| {
        view.modules.get(&owner) == Some(&State::Unmatched)
    });
    fs::write(&first, "allowed")?;
    wait(&editor, |view| {
        view.modules.get(&owner) == Some(&State::Ready)
    });

    let displaced = temp.path().join("displaced");
    fs::rename(&external, &displaced)?;
    fs::create_dir(&external)?;
    fs::write(&first, "allowed")?;
    wait(&editor, |view| {
        matches!(view.modules.get(&owner), Some(State::Failed(reason))
        if reason.contains("original file location"))
    });
    // A new release with the same declared path must retain the old directory identity.
    external_bundle(
        &archive,
        "1.1.1",
        &[first_path],
        first_path,
        sibling_path,
        false,
    );
    editor.send(Action::InspectZip(archive.clone()))?;
    let inherited = wait(&editor, |view| view.review.is_some());
    let inherited = inherited.review.as_ref().unwrap();
    assert!(inherited.additional_rule_paths.is_empty());
    editor.send(Action::Approve(inherited.token))?;
    wait(&editor, |view| {
        view.installed[0].release.manifest.version.to_string() == "1.1.1"
            && matches!(view.modules.get(&owner), Some(State::Failed(reason))
                if reason.contains("original file location"))
    });
    fs::remove_file(&first)?;
    fs::remove_dir(&external)?;
    fs::rename(&displaced, &external)?;
    wait(&editor, |view| {
        view.modules.get(&owner) == Some(&State::Ready)
    });

    fs::write(&second, "allowed")?;
    external_bundle(
        &archive,
        "1.2.0",
        &[first_path, second_path],
        second_path,
        sibling_path,
        false,
    );
    editor.send(Action::InspectZip(archive.clone()))?;
    let review = wait(&editor, |view| view.review.is_some());
    let review = review.review.as_ref().unwrap();
    assert_eq!(
        review.additional.iter().copied().collect::<Vec<_>>(),
        [Permission::ReadRulePaths]
    );
    assert_eq!(&review.additional_rule_paths, std::slice::from_ref(&second));
    editor.send(Action::CancelInstall)?;
    let kept = wait(&editor, |view| view.review.is_none());
    assert_eq!(
        kept.installed[0].release.manifest.version.to_string(),
        "1.1.1"
    );
    fs::write(&first, "changed")?;
    wait(&editor, |view| {
        view.modules.get(&owner) == Some(&State::Unmatched)
    });

    editor.send(Action::InspectZip(archive.clone()))?;
    let review = wait(&editor, |view| view.review.is_some());
    editor.send(Action::Approve(review.review.as_ref().unwrap().token))?;
    wait(&editor, |view| {
        view.modules.get(&owner) == Some(&State::Ready)
    });
    fs::write(&first, "allowed")?;
    external_bundle(
        &archive,
        "1.3.0",
        &[first_path, second_path, sibling_path],
        first_path,
        denied.to_str().unwrap(),
        true,
    );
    editor.send(Action::InspectZip(archive))?;
    let review = wait(&editor, |view| view.review.is_some());
    assert_eq!(
        &review.review.as_ref().unwrap().additional_rule_paths,
        std::slice::from_ref(&sibling)
    );
    editor.send(Action::Approve(review.review.as_ref().unwrap().token))?;
    let reverted = wait(&editor, |view| {
        view.installed[0].release.manifest.version.to_string() == "1.2.0"
            && view.modules.get(&owner) == Some(&State::Ready)
    });
    assert_eq!(
        reverted.installed[0].release.manifest.version.to_string(),
        "1.2.0"
    );
    assert!(
        !reverted.installed[0]
            .release
            .manifest
            .rule_paths
            .contains(sibling_path)
    );
    Ok(())
}

/// The missing parent must be reported at confirmation time, after the review shows its name.
#[test]
fn preparing_external_rule_paths_needs_no_target_io() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let directory = fixture_root(temp.path())?;
    let path = directory.join("missing-parent").join("allowed.txt");
    let denied = directory.join("denied.txt");
    let archive = temp.path().join("unavailable.zip");
    external_bundle(
        &archive,
        "1.0.0",
        &[path.to_str().unwrap()],
        path.to_str().unwrap(),
        denied.to_str().unwrap(),
        false,
    );
    let editor = session(&temp.path().join("profile"), false);

    editor.send(Action::InspectZip(archive.clone()))?;
    let review = wait(&editor, |view| view.review.is_some());
    let review = review.review.as_ref().unwrap();
    assert_eq!(&review.additional_rule_paths, std::slice::from_ref(&path));
    editor.send(Action::Approve(review.token))?;
    let failed = wait(&editor, |view| {
        view.message
            .contains("Could not approve external rule file")
    });
    assert!(failed.installed.is_empty());

    fs::create_dir(path.parent().unwrap())?;
    fs::write(&path, "allowed")?;
    let namespace = install(&editor, archive, "1.0.0");
    let workspace = project(temp.path(), 1);
    let owner = key(&namespace, workspace.id);
    editor.projects(Some(1), vec![workspace]);
    wait(&editor, |view| {
        view.modules.get(&owner) == Some(&State::Ready)
    });
    Ok(())
}

/// A project-local name pointing to an outside fixture must not trigger a traversal.
#[cfg(any(windows, target_os = "linux", target_os = "macos"))]
#[test]
fn project_read_rule_does_not_follow_an_external_directory_link() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = project(temp.path(), 1);
    let outside = temp.path().join("outside");
    fs::create_dir(&outside)?;
    fs::write(outside.join("data.txt"), "external")?;
    let link = workspace.root.join("link");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let output = std::process::Command::new("cmd.exe")
            .creation_flags(0x0800_0000)
            .args(["/D", "/C", "mklink", "/J"])
            .arg(&link)
            .arg(&outside)
            .output()?;
        anyhow::ensure!(
            output.status.success(),
            "Could not create fixture junction: {:?}",
            output
        );
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, &link)?;

    let archive = temp.path().join("no-links.zip");
    bundle(
        &archive,
        "^0.1.0",
        r#"
        for (const query of [host.exists, host.readFile, host.readBytes, host.list]) {
            if (await query('link/data.txt').then(() => true, () => false))
                throw new Error('External directory link was traversed');
        }
        const data = await host.readFile('direct.txt');
        return data?.text === 'in project';
    "#,
        false,
        "1.0.0",
        false,
    );
    fs::write(workspace.root.join("direct.txt"), "in project")?;
    let editor = session(&temp.path().join("profile"), false);
    let namespace = install(&editor, archive, "1.0.0");
    let owner = key(&namespace, workspace.id);
    editor.projects(Some(1), vec![workspace]);
    wait(&editor, |view| {
        view.modules.get(&owner) == Some(&State::Ready)
    });
    Ok(())
}
