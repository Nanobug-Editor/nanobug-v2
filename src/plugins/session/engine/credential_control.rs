//! Deliver credential changes and report authentication outcomes without retaining secret data.

use super::{CallKind, Engine, ResultEvent};
use crate::plugins::{
    credentials::{Credentials, DeleteOutcome},
    session::Key,
};
use nanobug_plugin_sdk::{CredentialChange, PluginEvent, ServiceResult};
use std::{collections::BTreeMap, time::Duration};
use zeroize::Zeroizing;

/// Only revision numbers are retained here. Saved values live in the system credential store.
#[derive(Default)]
pub(super) struct CredentialEvents {
    slots: BTreeMap<String, BTreeMap<String, u64>>,
}

pub(super) struct AccessResult {
    pub owner: Key,
    pub generation: u64,
    pub slot: String,
    pub revision: u64,
    pub read: bool,
    pub changed: bool,
    pub result: ServiceResult,
}

pub(super) enum Access {
    Read,
    Save(Zeroizing<String>),
    Delete,
}

/// Detachment can be visible before OS credential cleanup completes.
pub(super) struct AccessCompletion {
    pub value: Result<serde_json::Value, String>,
    pub changed: bool,
}

impl Access {
    pub(super) fn run(self, store: &Credentials, namespace: &str, slot: &str) -> AccessCompletion {
        use serde_json::Value;
        let unchanged = |value: anyhow::Result<Value>| AccessCompletion {
            changed: false,
            value: value.map_err(|error| error.to_string()),
        };
        match self {
            Self::Read => unchanged(store.get(namespace, slot).map(|secret| {
                secret.map_or(Value::Null, |secret| Value::String(secret.to_string()))
            })),
            Self::Save(secret) => match store.set(namespace, slot, &secret) {
                Ok(outcome) => AccessCompletion {
                    changed: true,
                    value: outcome
                        .confirmed()
                        .map(|()| Value::Null)
                        .map_err(|error| error.to_string()),
                },
                Err(error) => unchanged(Err(error)),
            },
            Self::Delete => match store.delete(namespace, Some(slot)) {
                Ok(outcome) => AccessCompletion {
                    changed: true,
                    value: match outcome {
                        DeleteOutcome::Removed => Ok(Value::Null),
                        DeleteOutcome::CleanupPending(message) => Err(message),
                    },
                },
                Err(error) => unchanged(Err(error)),
            },
        }
    }
}

impl Engine {
    /// A deleted reference invalidates live users even if its vault entry remains for cleanup.
    pub(super) fn credential_removal_finished(
        &mut self,
        namespace: &str,
        slot: Option<String>,
        result: anyhow::Result<DeleteOutcome>,
    ) {
        self.refresh_credentials();
        match result {
            Ok(outcome) => {
                self.changed_credential(namespace, slot);
                match outcome {
                    DeleteOutcome::Removed => self.message("Saved credential removed"),
                    DeleteOutcome::CleanupPending(message) => self.message(message),
                }
            }
            Err(error) => self.message(error.to_string()),
        }
    }

    pub(super) fn changed_credential(&mut self, namespace: &str, slot: Option<String>) {
        let revision = self.next();
        if let Some(slot) = slot {
            let slots = self
                .credential_events
                .slots
                .entry(namespace.into())
                .or_default();
            slots.insert(slot.clone(), revision);
            for (key, errors) in &mut self.view.credential_errors {
                if key.namespace == namespace {
                    errors.remove(&slot);
                }
            }
        } else {
            self.credential_events.slots.remove(namespace);
            self.view
                .credential_errors
                .retain(|key, _| key.namespace != namespace);
        }
        self.view
            .credential_errors
            .retain(|_, errors| !errors.is_empty());
        self.changed = true;
    }

    pub(super) fn advance_credential_events(&mut self) {
        if self.credential_events.slots.is_empty() {
            return;
        }
        let mut ready = Vec::new();
        for (namespace, slots) in &self.credential_events.slots {
            let Some(runtime) = self.runtimes.get(namespace) else {
                continue;
            };
            for (instance, live) in &runtime.instances {
                if !live.opened
                    || live.failure.is_some()
                    || !live.module.credential_events
                    || runtime
                        .calls
                        .values()
                        .any(|call| call.instance == *instance)
                {
                    continue;
                }
                let Some((slot, revision)) = slots.iter().find(|(slot, revision)| {
                    live.credential_seen.get(*slot) != Some(*revision)
                        && self.can_use_credential(namespace, slot)
                }) else {
                    continue;
                };
                let key = Key {
                    namespace: namespace.clone(),
                    instance: instance.clone(),
                };
                if self.wanted.contains_key(&key) && !self.is_draining(instance.project) {
                    ready.push((
                        key,
                        CredentialChange {
                            slot: slot.clone(),
                            revision: *revision,
                        },
                    ));
                    if ready.len() >= 8 {
                        break;
                    }
                }
            }
            if ready.len() >= 8 {
                break;
            }
        }
        for (key, change) in ready {
            self.invoke_with_kind(
                key,
                PluginEvent::CredentialChanged {
                    change: change.clone(),
                },
                None,
                Duration::from_secs(2),
                CallKind::CredentialChange,
                Some(change),
            );
        }
    }

    pub(super) fn credential_access_finished(&mut self, access: AccessResult) {
        let AccessResult {
            owner,
            generation,
            slot,
            revision,
            read,
            changed,
            mut result,
        } = access;
        if changed {
            self.refresh_credentials();
            self.changed_credential(&owner.namespace, Some(slot.clone()));
        }
        let private = read
            && result
                .value
                .as_ref()
                .is_ok_and(serde_json::Value::is_string);
        if private && self.credential_revision(&owner.namespace, &slot) != revision {
            result.value = Err("Credential changed while it was being read. Try again.".into());
            self.event(ResultEvent::Service(owner, generation, result));
        } else if private {
            self.finish_service(&owner.namespace);
            let current = self.runtimes.get(&owner.namespace).is_some_and(|runtime| {
                runtime.generation == generation && runtime.worker.is_some()
            }) && self.wanted.contains_key(&owner)
                && self.can_use_credential(&owner.namespace, &slot);
            if current {
                self.invoke_with_kind(
                    owner,
                    PluginEvent::ServiceResult { result },
                    None,
                    Duration::from_secs(2),
                    CallKind::CredentialReply,
                    Some(CredentialChange { slot, revision }),
                );
            }
        } else {
            self.event(ResultEvent::Service(owner, generation, result));
        }
    }

    pub(super) fn credential_revision(&self, owner: &str, slot: &str) -> u64 {
        self.credential_events
            .slots
            .get(owner)
            .and_then(|slots| slots.get(slot))
            .copied()
            .unwrap_or(0)
    }

    pub(super) fn credential_check_finished(
        &mut self,
        key: &Key,
        change: &CredentialChange,
        rejected: bool,
    ) {
        if self.credential_revision(&key.namespace, &change.slot) != change.revision
            || !self.wanted.contains_key(key)
        {
            return;
        }
        if rejected {
            self.view
                .credential_errors
                .entry(key.clone())
                .or_default()
                .insert(change.slot.clone());
        } else if let Some(errors) = self.view.credential_errors.get_mut(key) {
            errors.remove(&change.slot);
            if errors.is_empty() {
                self.view.credential_errors.remove(key);
            }
        }
        self.changed = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::package::InstallStore;
    use nanobug_plugin_sdk::Instance;
    use std::collections::BTreeSet;

    #[test]
    fn pending_vault_cleanup_invalidates_saved_slot_and_stale_authentication() -> anyhow::Result<()>
    {
        let directory = tempfile::tempdir()?;
        let mut engine = Engine::new(InstallStore::new(directory.path().to_path_buf()), false);
        let owner = Key {
            namespace: "fixture".into(),
            instance: Instance {
                project: 1,
                module: "fixture".into(),
            },
        };
        let slot = "login";
        engine.changed_credential(&owner.namespace, Some(slot.into()));
        let prior = engine.credential_revision(&owner.namespace, slot);
        engine
            .view
            .credential_errors
            .insert(owner.clone(), BTreeSet::from([slot.into()]));

        engine.credential_removal_finished(
            &owner.namespace,
            Some(slot.into()),
            Ok(DeleteOutcome::CleanupPending(
                "Credential detached. Cleanup pending.".into(),
            )),
        );
        assert!(engine.credential_revision(&owner.namespace, slot) > prior);
        assert!(engine.view.credential_errors.is_empty());
        assert_eq!(engine.view.message, "Credential detached. Cleanup pending.");
        Ok(())
    }
}
