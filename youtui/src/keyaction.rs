use crate::app::component::actionhandler::Action;
use crate::config::keymap::{KeyActionTree, Keymap};
use crate::keybind::Keybind;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// This is an Action that will be triggered when pressing a particular Keybind.
pub struct KeyAction<A> {
    // Consider - can there be multiple actions?
    pub action: A,
    #[serde(default)]
    pub visibility: KeyActionVisibility,
}

#[derive(PartialEq, Copy, Default, Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
/// Visibility of a KeyAction.
pub enum KeyActionVisibility {
    /// Displayed on help menu
    #[default]
    Standard,
    /// Displayed on Header and help menu
    Global,
    /// Not displayed
    Hidden,
}

#[derive(PartialEq, Debug, Clone)]
/// Type-erased keybinding for displaying.
pub struct DisplayableKeyAction<'a> {
    // XXX: Do we also want to display sub-keys in Modes?
    pub keybinds: Cow<'a, str>,
    pub context: Cow<'a, str>,
    pub description: Cow<'a, str>,
}
/// Type-erased mode for displaying its actions.
pub struct DisplayableMode<'a, I: Iterator<Item = DisplayableKeyAction<'a>>> {
    pub displayable_commands: I,
    pub description: Cow<'a, str>,
}

impl<'a> DisplayableKeyAction<'a> {
    pub fn from_keybind_and_action_tree<A: Action + 'a>(
        key: &'a Keybind,
        value: &'a KeyActionTree<A>,
    ) -> Self {
        match value {
            KeyActionTree::Key(k) => DisplayableKeyAction {
                keybinds: key.to_string().into(),
                context: k.action.context(),
                description: k.action.describe(),
            },
            KeyActionTree::Mode { name, keys } => DisplayableKeyAction {
                keybinds: key.to_string().into(),
                context: keys
                    .iter()
                    .next()
                    .map(|(_, kt)| kt.get_context())
                    .unwrap_or_default(),
                description: name
                    .as_ref()
                    .map(ToOwned::to_owned)
                    .unwrap_or_else(|| key.to_string())
                    .into(),
            },
        }
    }
}

/// Expand all keybinds from the given keymaps into displayable entries,
/// recursively flattening [`KeyActionTree::Mode`] so sub-keybindings also
/// appear in the output. Each sub-key entry includes the mode trigger key
/// as a prefix (e.g. `Enter → Space`).
pub fn flatten_keybinds_as_readable<'a, A: Action + 'static>(
    keybinds: impl Iterator<Item = &'a Keymap<A>> + 'a,
) -> Vec<DisplayableKeyAction<'a>> {
    let mut out = Vec::new();
    for keymap in keybinds {
        for (key, tree) in keymap.iter() {
            flatten_tree(key, tree, &mut out);
        }
    }
    out
}

fn flatten_tree<'a, A: Action + 'static>(
    key: &'a Keybind,
    tree: &'a KeyActionTree<A>,
    out: &mut Vec<DisplayableKeyAction<'a>>,
) {
    match tree {
        KeyActionTree::Key(k) => {
            if k.visibility != KeyActionVisibility::Hidden {
                out.push(DisplayableKeyAction::from_keybind_and_action_tree(key, tree));
            }
        }
        KeyActionTree::Mode { keys, .. } => {
            // Show the mode trigger as one row.
            out.push(DisplayableKeyAction::from_keybind_and_action_tree(key, tree));
            // Show each sub-key, prefixed by the mode trigger.
            let prefix = key.to_string();
            for (sub_key, sub_tree) in keys.iter() {
                let combined = format!("{prefix} → {}", sub_key);
                match sub_tree {
                    KeyActionTree::Key(k) => {
                        if k.visibility != KeyActionVisibility::Hidden {
                            out.push(DisplayableKeyAction {
                                keybinds: combined.into(),
                                context: k.action.context(),
                                description: k.action.describe(),
                            });
                        }
                    }
                    KeyActionTree::Mode { .. } => {
                        // Nested modes are not expected, but handle gracefully.
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, PartialEq)]
    struct TestAction(&'static str);
    impl Action for TestAction {
        fn context(&self) -> Cow<'_, str> {
            Cow::Borrowed("test-context")
        }
        fn describe(&self) -> Cow<'_, str> {
            Cow::Borrowed(self.0)
        }
    }

    /// Parity lock for the help-menu flatten: visible keys and mode trigger +
    /// visible sub-keys appear, Hidden rows and sub-keys are dropped.
    #[test]
    fn flatten_shows_visible_keys_and_mode_subkeys_and_drops_hidden() {
        let mut map: Keymap<TestAction> = Keymap::new();
        map.insert(
            Keybind::new_unmodified(crossterm::event::KeyCode::Char('q')),
            KeyActionTree::new_key_with_visibility(
                TestAction("quit"),
                KeyActionVisibility::Global,
            ),
        );
        map.insert(
            Keybind::new_unmodified(crossterm::event::KeyCode::Char('z')),
            KeyActionTree::new_key_with_visibility(
                TestAction("hidden-key"),
                KeyActionVisibility::Hidden,
            ),
        );
        map.insert(
            Keybind::new_unmodified(crossterm::event::KeyCode::Enter),
            KeyActionTree::new_mode(
                [
                    (
                        Keybind::new_unmodified(crossterm::event::KeyCode::Char(' ')),
                        KeyActionTree::new_key_with_visibility(
                            TestAction("toggle"),
                            KeyActionVisibility::Standard,
                        ),
                    ),
                    (
                        Keybind::new_unmodified(crossterm::event::KeyCode::Char('p')),
                        KeyActionTree::new_key_with_visibility(
                            TestAction("secret-subkey"),
                            KeyActionVisibility::Hidden,
                        ),
                    ),
                ],
                "actions".into(),
            ),
        );

        let out = flatten_keybinds_as_readable(std::iter::once(&map));
        let descriptions: Vec<&str> = out.iter().map(|d| d.description.as_ref()).collect();
        assert!(descriptions.contains(&"quit"), "visible key row missing");
        assert!(
            !descriptions.contains(&"hidden-key"),
            "Hidden key row must be dropped"
        );
        assert!(
            descriptions.contains(&"actions"),
            "mode trigger row (named by the mode) missing"
        );
        assert!(
            descriptions.contains(&"toggle"),
            "visible mode sub-key missing"
        );
        assert!(
            !descriptions.contains(&"secret-subkey"),
            "Hidden mode sub-key must be dropped"
        );

        let trigger = out
            .iter()
            .find(|d| d.description == "actions")
            .expect("mode trigger row");
        assert_eq!(trigger.keybinds.as_ref(), "Enter");
        let sub = out
            .iter()
            .find(|d| d.description == "toggle")
            .expect("mode sub-key row");
        assert_eq!(sub.keybinds.as_ref(), "Enter → Space");
    }
}
