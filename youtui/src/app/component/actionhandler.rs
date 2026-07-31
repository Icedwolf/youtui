use crate::app::effect::Effects;
use crate::app::AppCallback;
use crate::config::Config;
use crate::config::keymap::{KeyActionTree, Keymap};
use crate::keyaction::{DisplayableKeyAction, KeyAction, KeyActionVisibility};
use crate::keybind::Keybind;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use std::borrow::Cow;
use tracing::trace;
use ytmapi_rs::common::SearchSuggestion;

pub trait Component: Sized + 'static {}

#[must_use]
pub struct YoutuiEffect<C: Component> {
    pub effect: Effects<C>,
    pub callback: Option<AppCallback>,
}
impl<C: Component> YoutuiEffect<C> {
    pub fn new_no_op() -> Self {
        trace!("no-op effect created");
        YoutuiEffect {
            effect: Effects::none(),
            callback: None,
        }
    }
    pub fn map<C2>(self, f: impl Fn(&mut C2) -> &mut C + Clone + Send + 'static) -> YoutuiEffect<C2>
    where
        C2: Component,
    {
        let YoutuiEffect { effect, callback } = self;
        YoutuiEffect {
            effect: effect.map(f),
            callback,
        }
    }
}
impl<C: Component> From<Effects<C>> for YoutuiEffect<C> {
    fn from(value: Effects<C>) -> Self {
        YoutuiEffect {
            effect: value,
            callback: None,
        }
    }
}
impl<C: Component> From<(Effects<C>, Option<AppCallback>)> for YoutuiEffect<C> {
    fn from(value: (Effects<C>, Option<AppCallback>)) -> Self {
        YoutuiEffect {
            effect: value.0,
            callback: value.1,
        }
    }
}

pub trait Action {
    fn context(&self) -> Cow<'_, str>;
    fn describe(&self) -> Cow<'_, str>;
}

pub trait ActionHandler<A: Action>: Component + Sized {
    fn apply_action(&mut self, action: A) -> impl Into<YoutuiEffect<Self>>;
}
pub fn apply_action_mapped<R, B, C, F>(root: &mut R, action: B, f: F) -> YoutuiEffect<R>
where
    B: Action,
    R: Component,
    C: Component + ActionHandler<B> + 'static,
    F: Fn(&mut R) -> &mut C + Send + Clone + 'static,
{
    f(root)
        .apply_action(action)
        .into()
        .map(move |this: &mut R| f(this))
}

pub trait Scrollable {
    fn increment_list(&mut self, amount: isize);
    fn is_scrollable(&self) -> bool;
}
pub trait DelegateScrollable {
    fn delegate_mut(&mut self) -> &mut dyn Scrollable;
    fn delegate_ref(&self) -> &dyn Scrollable;
}
impl<T: DelegateScrollable> Scrollable for T {
    fn increment_list(&mut self, amount: isize) {
        self.delegate_mut().increment_list(amount);
    }
    fn is_scrollable(&self) -> bool {
        self.delegate_ref().is_scrollable()
    }
}

pub trait KeyRouter<A: Action + 'static> {
    fn get_active_keybinds<'a>(
        &self,
        config: &'a Config,
    ) -> impl Iterator<Item = &'a Keymap<A>> + 'a;
    fn get_all_keybinds<'a>(&self, config: &'a Config) -> impl Iterator<Item = &'a Keymap<A>> + 'a;
}

pub trait DominantKeyRouter<A: Action + 'static> {
    fn dominant_keybinds_active(&self) -> bool;
    fn get_dominant_keybinds<'a>(
        &self,
        config: &'a Config,
    ) -> impl Iterator<Item = &'a Keymap<A>> + 'a;
}

pub fn get_global_keybinds_as_readable_iter<'a, A: Action + 'static>(
    keybinds: impl Iterator<Item = &'a Keymap<A>> + 'a,
) -> impl Iterator<Item = DisplayableKeyAction<'a>> + 'a {
    keybinds
        .flat_map(|keymap| keymap.iter())
        .filter(|(_, kt)| (*kt).get_visibility() == KeyActionVisibility::Global)
        .map(|(kb, kt)| DisplayableKeyAction::from_keybind_and_action_tree(kb, kt))
}

pub trait TextHandler: Component {
    fn get_text(&self) -> Option<&str>;
    fn clear_text(&mut self) -> bool;
    fn replace_text(&mut self, text: impl Into<String>);
    fn is_text_handling(&self) -> bool;
    fn handle_text_event_impl(
        &mut self,
        event: &Event,
    ) -> Option<Effects<Self>>
    where
        Self: Sized;
    fn try_handle_text(&mut self, event: &Event) -> Option<Effects<Self>>
    where
        Self: Sized,
    {
        if !self.is_text_handling() {
            return None;
        }
        self.handle_text_event_impl(event)
    }
}

pub trait Suggestable: TextHandler {
    fn get_search_suggestions(&self) -> &[SearchSuggestion];
    fn has_search_suggestions(&self) -> bool;
}

#[derive(Debug)]
pub enum KeyHandleAction<'a, A: Action> {
    Action(A),
    Mode { name: String, keys: &'a Keymap<A> },
    NoMap,
}

pub fn handle_key_stack<'a, A, I>(keys: I, key_stack: &[KeyEvent]) -> KeyHandleAction<'a, A>
where
    A: Action + Copy + 'static,
    I: IntoIterator<Item = &'a Keymap<A>>,
{
    let convert = |k: KeyEvent| {
        let KeyEvent {
            code,
            mut modifiers,
            ..
        } = k;
        if let KeyCode::Char(_) = code {
            modifiers = modifiers.difference(KeyModifiers::SHIFT);
        }
        Keybind { code, modifiers }
    };
    let mut key_stack_iter = key_stack.iter();
    let Some(first_key) = key_stack_iter.next() else {
        return KeyHandleAction::NoMap;
    };
    let first_found = keys.into_iter().find_map(|km| km.get(&convert(*first_key)));
    let mut next_mode = match first_found {
        Some(KeyActionTree::Key(KeyAction { action, .. })) => {
            return KeyHandleAction::Action(*action);
        }
        Some(KeyActionTree::Mode { name, keys }) => (name, keys),
        None => return KeyHandleAction::NoMap,
    };
    for key in key_stack_iter {
        let next_found = next_mode.1.get(&convert(*key));
        match next_found {
            Some(KeyActionTree::Key(KeyAction { action, .. })) => {
                return KeyHandleAction::Action(*action);
            }
            Some(KeyActionTree::Mode { name, keys }) => next_mode = (name, keys),
            None => return KeyHandleAction::NoMap,
        };
    }
    KeyHandleAction::Mode {
        name: next_mode.0.as_deref().unwrap_or("UNNAMED MODE").to_string(),
        keys: next_mode.1,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::todo)]
    use super::{Action, Component};
    use crate::app::component::actionhandler::{KeyHandleAction, Keymap, handle_key_stack};
    use crate::config::keymap::KeyActionTree;
    use crate::keybind::Keybind;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use pretty_assertions::assert_eq;

    #[derive(PartialEq, Debug, Copy, Clone)]
    enum TestAction {
        Test1,
        Test2,
        Test3,
        TestStack,
    }
    impl Component for () {}
    impl Action for TestAction {
        fn context(&self) -> std::borrow::Cow<'_, str> {
            todo!()
        }
        fn describe(&self) -> std::borrow::Cow<'_, str> {
            todo!()
        }
    }
    fn test_keymap() -> Keymap<TestAction> {
        [
            (
                Keybind::new_unmodified(KeyCode::F(10)),
                KeyActionTree::new_key(TestAction::Test1),
            ),
            (
                Keybind::new_unmodified(KeyCode::F(12)),
                KeyActionTree::new_key(TestAction::Test2),
            ),
            (
                Keybind::new_unmodified(KeyCode::Left),
                KeyActionTree::new_key(TestAction::Test3),
            ),
            (
                Keybind::new_unmodified(KeyCode::Right),
                KeyActionTree::new_key(TestAction::Test3),
            ),
            (
                Keybind::new_unmodified(KeyCode::Enter),
                KeyActionTree::new_mode(
                    [
                        (
                            Keybind::new_unmodified(KeyCode::Enter),
                            KeyActionTree::new_key(TestAction::Test2),
                        ),
                        (
                            Keybind::new_unmodified(KeyCode::Char('a')),
                            KeyActionTree::new_key(TestAction::Test3),
                        ),
                        (
                            Keybind::new_unmodified(KeyCode::Char('p')),
                            KeyActionTree::new_key(TestAction::Test2),
                        ),
                        (
                            Keybind::new_unmodified(KeyCode::Char(' ')),
                            KeyActionTree::new_key(TestAction::Test3),
                        ),
                        (
                            Keybind::new_unmodified(KeyCode::Char('P')),
                            KeyActionTree::new_key(TestAction::TestStack),
                        ),
                    ],
                    "Play".into(),
                ),
            ),
        ]
        .into_iter()
        .collect::<Keymap<_>>()
    }
    #[test]
    fn test_key_stack_shift_modifier() {
        let kb = test_keymap();
        let ks1 = KeyEvent::new(KeyCode::Enter, KeyModifiers::empty());
        let ks2 = KeyEvent::new(KeyCode::Char('P'), KeyModifiers::SHIFT);
        let key_stack = [ks1, ks2];
        let expected = TestAction::TestStack;
        let output = handle_key_stack(std::iter::once(&kb), &key_stack);
        let KeyHandleAction::Action(output) = output else {
            panic!("Expected keyhandleoutcome::action");
        };
        assert_eq!(expected, output);
    }
    #[test]
    fn test_key_stack() {
        let kb = test_keymap();
        let ks1 = KeyEvent::new(KeyCode::Enter, KeyModifiers::empty());
        let ks2 = KeyEvent::new(KeyCode::Char('P'), KeyModifiers::empty());
        let key_stack = [ks1, ks2];
        let expected = TestAction::TestStack;
        let KeyHandleAction::Action(output) = handle_key_stack(std::iter::once(&kb), &key_stack)
        else {
            panic!("Expected keyhandleoutcome::action");
        };
        assert_eq!(expected, output);
    }
    #[test]
    fn test_index_keybinds() {
        let kb = test_keymap();
        let ks = KeyEvent::new(KeyCode::Enter, KeyModifiers::empty());
        let expected_keys = [
            (
                Keybind::new_unmodified(KeyCode::Enter),
                KeyActionTree::new_key(TestAction::Test2),
            ),
            (
                Keybind::new_unmodified(KeyCode::Char('a')),
                KeyActionTree::new_key(TestAction::Test3),
            ),
            (
                Keybind::new_unmodified(KeyCode::Char('p')),
                KeyActionTree::new_key(TestAction::Test2),
            ),
            (
                Keybind::new_unmodified(KeyCode::Char(' ')),
                KeyActionTree::new_key(TestAction::Test3),
            ),
            (
                Keybind::new_unmodified(KeyCode::Char('P')),
                KeyActionTree::new_key(TestAction::TestStack),
            ),
        ]
        .into_iter()
        .collect::<Keymap<_>>();
        let expected_name = "Play".to_string();
        let KeyHandleAction::Mode { keys, name } = handle_key_stack(std::iter::once(&kb), &[ks])
        else {
            panic!("Expected keyhandleoutcome::mode");
        };
        assert_eq!(name, expected_name);
        assert_eq!(keys, &expected_keys);
    }
}
