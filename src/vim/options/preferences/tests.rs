use super::*;
use crate::{
    app_data::settings::{RestoreFields, SettingsSnapshot},
    document::Document,
    vim::Register,
};
use serde_json::json;

/// App defaults propagate to live views, retain explicit local overrides and recover a distinct disk baseline.
#[test]
fn cancel_restores_disk_and_live_defaults_independently() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("settings.toml");
    let initial = "# original\n[vim.options.buffer]\ntabstop = 3 # disk value\n";
    std::fs::write(&path, initial)?;
    let snapshot = SettingsSnapshot::load(Some(&path))?;
    let maps = InputMappings::default();
    maps.load_config(&path)?;
    let mut register = Register::default();
    register.set_input_mappings(maps.clone());
    let mut vim = Vim::new(Document::new("x"));
    register.initialize_buffer_options(&mut vim);
    assert!(vim.configure_options("set tabstop=8", &mut register));
    assert!(vim.configure_options("setlocal shiftwidth=4", &mut register));
    let changes = [
        ("tabstop".into(), json!(2)),
        ("shiftwidth".into(), json!(7)),
    ];
    let next = maps.save_option_preferences(&changes, Some(&path), None)?;
    vim.follow_preferences(&next, &changes);
    assert_eq!(vim.doc.tabstop(), 2);
    assert_eq!(vim.shiftwidth(), 4);
    assert!(path.exists());
    let original = [
        ("tabstop".into(), json!(8)),
        ("shiftwidth".into(), json!(4)),
    ];
    let next = maps.save_option_preferences(
        &original,
        Some(&path),
        Some(RestoreFields {
            snapshot: &snapshot,
            names: &["tabstop", "shiftwidth"],
        }),
    )?;
    vim.follow_preferences(&next, &original);
    assert_eq!(vim.doc.tabstop(), 8);
    assert_eq!(std::fs::read_to_string(&path)?, initial);
    Ok(())
}

/// Validation and invalid on-disk structures cannot partially publish a page.
#[test]
fn invalid_page_leaves_runtime_and_file_unchanged() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("settings.toml");
    let maps = InputMappings::default();
    let valid = ("shiftwidth".into(), json!(2));
    let invalid = ("tabstop".into(), json!(0));
    assert!(
        maps.save_option_preferences(&[valid.clone(), invalid], Some(&path), None)
            .is_err()
    );
    assert!(!path.exists());
    std::fs::write(&path, "vim = 5\n")?;
    assert!(
        maps.save_option_preferences(&[valid], Some(&path), None)
            .is_err()
    );
    assert_eq!(maps.options().preference("shiftwidth")?, json!(4));
    assert_eq!(std::fs::read_to_string(path)?, "vim = 5\n");
    Ok(())
}
