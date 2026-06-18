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
                out.push(DisplayableKeyAction {
                    keybinds: key.to_string().into(),
                    context: k.action.context(),
                    description: k.action.describe(),
                });
            }
        }
        KeyActionTree::Mode { name, keys } => {
            // Show the mode trigger as one row.
            out.push(DisplayableKeyAction {
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
            });
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
