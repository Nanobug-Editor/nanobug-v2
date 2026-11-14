//! Rows for editor-side operations presented by the plugin window.

use super::{PluginCoordinator, Row, Target};
use crate::grid::windows::plugin_manager::rows::row;

pub(super) fn rows(ui: &PluginCoordinator) -> Vec<Row> {
    if ui.credential_waiting {
        return vec![row("Saving credential…", "", Target::None)];
    }
    if ui.credential_draft.is_some() {
        return vec![
            row(
                "Apply credential",
                "Save the new value in the system credential store.",
                Target::ApplyCredential,
            ),
            row(
                "Continue editing",
                "Keep the new value in this session.",
                Target::EditCredential,
            ),
            row(
                "Discard pending value",
                "Keep the currently saved credential.",
                Target::DiscardCredential,
            ),
        ];
    }
    if let Some(choices) = &ui.formatting.choices {
        return choices.rows();
    }
    if ui.file_busy {
        return vec![row("Applying file operations…", "", Target::None)];
    }
    if let Some(plan) = &ui.file_plan {
        return plan.rows();
    }
    if !ui.file_report.is_empty() {
        return ui
            .file_report
            .iter()
            .map(|line| row(line, line, Target::None))
            .collect();
    }
    if let Some(results) = &ui.language {
        return results.rows();
    }
    if let Some(outline) = &ui.outline {
        return outline.rows();
    }
    if let Some(plan) = &ui.edit_plan {
        return plan.rows();
    }
    if ui.edit_check.is_some() {
        return vec![row(
            "Checking file versions…",
            "Press Esc to cancel.",
            Target::None,
        )];
    }
    if let Some(close) = &ui.close_draft {
        if close.waiting {
            return vec![row(
                if close.draining {
                    "Waiting for plugin operations to finish…"
                } else {
                    "Saving confirmed plugin inputs…"
                },
                "",
                Target::None,
            )];
        }
        if close.draining {
            return vec![
                row(
                    "Wait for operations",
                    close
                        .error
                        .as_deref()
                        .unwrap_or("Keep the editor open while writes finish."),
                    Target::RetryDraftClose,
                ),
                row(
                    "Cancel close",
                    "Return to editing.",
                    Target::CancelDraftClose,
                ),
            ];
        }
        return vec![
            row(
                "Retry saving",
                close
                    .error
                    .as_deref()
                    .unwrap_or("Plugin inputs could not be saved."),
                Target::RetryDraftClose,
            ),
            row(
                "Export inputs and close",
                "Save a copy of confirmed ordinary input to another path.",
                Target::ExportDraft,
            ),
            row(
                "Cancel close",
                "Keep the editor open and keep inputs in memory.",
                Target::CancelDraftClose,
            ),
            row(
                "Discard unsaved plugin inputs and close",
                "Keeps the last successfully saved input.",
                Target::DiscardDraftClose,
            ),
        ];
    }
    Vec::new()
}
