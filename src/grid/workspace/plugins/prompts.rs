//! Workspace adapter for prompts.

use super::*;

impl Workspace {
    pub(in crate::grid::workspace) fn finish_plugin_prompt(
        &mut self,
        cancel: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(input) = self.plugins.manager.prompt.take() else {
            return;
        };
        match input.kind {
            Prompt::Search {
                original,
                selection,
                offset,
            } => {
                if cancel {
                    self.plugins.manager.search = original;
                    self.plugins.manager.selection = selection;
                    self.plugins.manager.offset = offset;
                }
            }
            Prompt::Development if !cancel => {
                let path = PathBuf::from(input.value.trim().trim_matches('"'));
                self.plugins.send(Action::InspectDevelopment(path));
            }
            Prompt::DevSource {
                namespace,
                mut settings,
            } if !cancel => {
                let value = input.value.trim().trim_matches('"');
                settings.source = (!value.is_empty()).then(|| PathBuf::from(value));
                if settings
                    .source
                    .as_ref()
                    .is_some_and(|path| !path.is_absolute())
                {
                    self.plugins.manager.feedback =
                        Some("Enter an absolute Cargo project path".into());
                    self.plugins.manager.prompt = Some(Input {
                        kind: Prompt::DevSource {
                            namespace,
                            settings,
                        },
                        value: input.value,
                    });
                    return;
                }
                if settings.source.is_none() {
                    settings.automatic = false;
                }
                self.plugins
                    .send(Action::SetDeveloperSettings(namespace, settings));
            }
            Prompt::Rule {
                scope,
                owner,
                module,
            } if !cancel => {
                let value = match input.value.trim() {
                    "" | "inherit" => None,
                    "true" => Some(Rule::Always),
                    "false" => Some(Rule::Never),
                    _ => Some(Rule::Script(input.value)),
                };
                self.plugins.send(Action::EditConfig(
                    scope,
                    ConfigEdit::Rule {
                        owner,
                        module,
                        value,
                    },
                ));
            }
            Prompt::Setting {
                scope,
                owner,
                id,
                renamed_from,
                original,
            } if !cancel => {
                let value = if matches!(input.value.trim(), "inherit" | "") {
                    None
                } else {
                    match serde_json::from_str(&input.value) {
                        Ok(parsed) => Some(parsed),
                        Err(error) => {
                            self.plugins.manager.feedback =
                                Some(format!("Invalid JSON value: {error}"));
                            self.plugins.manager.prompt = Some(Input {
                                kind: Prompt::Setting {
                                    scope,
                                    owner,
                                    id,
                                    renamed_from,
                                    original,
                                },
                                value: input.value,
                            });
                            return;
                        }
                    }
                };
                self.plugins.send(Action::EditConfig(
                    scope,
                    ConfigEdit::Setting {
                        owner,
                        id,
                        renamed_from,
                        original,
                        value,
                    },
                ));
            }
            Prompt::Credential {
                namespace,
                slot,
                value,
            } => {
                self.plugins.composition.text.zeroize();
                if !value.is_empty() {
                    self.review_plugin_credential(namespace, slot, value);
                }
            }
            Prompt::ExportDraft if !cancel => {
                let path = PathBuf::from(input.value.trim().trim_matches('"'));
                if path.is_absolute() {
                    self.write_close_draft(Some(path), window, cx);
                } else {
                    self.plugins.manager.feedback = Some("Enter an absolute export path".into());
                    self.plugins.manager.prompt = Some(input);
                }
            }
            Prompt::Shortcut { namespace, command } if !cancel => {
                let value = input.value.trim();
                let error = if value == "inherit" || value.is_empty() {
                    None
                } else if let Some(stroke) = shortcuts::parse(value) {
                    let selected = shortcuts::declaration(&self.plugins.view, &namespace, &command);
                    self.plugins
                        .view
                        .configurations
                        .get(&ConfigScope::AllProjects)
                        .and_then(|view| {
                            view.draft
                                .as_ref()
                                .unwrap_or(&view.applied)
                                .shortcuts
                                .iter()
                                .find_map(|(owner, mappings)| {
                                    mappings.iter().find_map(|(other, mapped)| {
                                        let same = owner == &namespace && other == &command;
                                        let overlaps = selected
                                            .zip(shortcuts::declaration(
                                                &self.plugins.view,
                                                owner,
                                                other,
                                            ))
                                            .is_none_or(|(current, prior)| {
                                                current.shares_context(prior)
                                            });
                                        (!same && overlaps)
                                            .then(|| shortcuts::parse(mapped))
                                            .flatten()
                                            .filter(|existing| existing == &stroke)
                                            .map(|_| {
                                                format!("This key is already assigned to {other}.")
                                            })
                                    })
                                })
                        })
                } else {
                    Some("Choose a free function key or Ctrl+Alt chord. Enter inherit or leave empty to disable.".into())
                };
                if let Some(error) = error {
                    self.plugins.manager.feedback = Some(error);
                    self.plugins.manager.prompt = Some(Input {
                        kind: Prompt::Shortcut { namespace, command },
                        value: input.value,
                    });
                    return;
                }
                self.plugins.send(Action::EditConfig(
                    ConfigScope::AllProjects,
                    ConfigEdit::Shortcut {
                        namespace,
                        command,
                        value: (value != "inherit").then(|| value.into()),
                    },
                ));
            }
            _ => {}
        }
        self.plugins.composition.clear();
    }

    pub(in crate::grid::workspace) fn review_plugin_credential(
        &mut self,
        namespace: String,
        slot: String,
        value: Zeroizing<String>,
    ) {
        self.plugins.credential_draft = Some(CredentialDraft {
            namespace,
            slot,
            value,
        });
        self.plugins.composition.text.zeroize();
        self.plugins.composition.clear();
        self.plugins.manager.selection = 0;
        self.plugins.manager.offset = 0;
        self.plugins.manager.feedback = None;
    }

    pub(in crate::grid::workspace) fn edit_plugin_credential(&mut self) {
        if let Some(draft) = self.plugins.credential_draft.take() {
            self.plugins.credential_close = None;
            self.plugins.manager.prompt = Some(Input {
                kind: Prompt::Credential {
                    namespace: draft.namespace,
                    slot: draft.slot,
                    value: draft.value,
                },
                value: String::new(),
            });
        }
    }

    pub(in crate::grid::workspace) fn finish_credential_close(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(target) = self.plugins.credential_close.take() {
            self.plugins.open = false;
            self.request_close(target, window, cx);
        }
    }

    pub(in crate::grid::workspace) fn save_plugin_credential(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(draft) = self.plugins.credential_draft.take() else {
            return;
        };
        let (reply, response) = async_channel::bounded(1);
        if !self.plugins.send(Action::SetCredential {
            namespace: draft.namespace.clone(),
            slot: draft.slot.clone(),
            value: draft.value.clone(),
            reply,
        }) {
            self.plugins.credential_draft = Some(draft);
            return;
        }
        self.plugins.credential_waiting = true;
        self.plugins.manager.feedback = Some("Saving credential…".into());
        cx.spawn_in(window, async move |workspace, cx| {
            let result = response
                .recv()
                .await
                .unwrap_or_else(|_| Err("Credential store stopped".into()));
            let _ = cx.update(|window, cx| {
                workspace.update(cx, |workspace, cx| {
                    workspace.plugins.credential_waiting = false;
                    match result {
                        Ok(()) => {
                            workspace.plugins.manager.feedback = Some("Credential saved".into());
                            workspace.finish_credential_close(window, cx);
                        }
                        Err(error) => {
                            workspace.plugins.manager.feedback = Some(error);
                            workspace.plugins.credential_draft = Some(draft);
                        }
                    }
                    workspace.plugin_refresh(window, cx);
                })
            });
        })
        .detach();
    }
}
