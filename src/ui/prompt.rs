//! Single-line text input shown in the bottom bar.

use ratatui::crossterm::event::{KeyCode, KeyModifiers};

use super::keymap::KeyBinding;

/// What a prompt's answer is used for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptKind {
    SelectGlob,
    NewGroup,
    RenameGroup,
    SavePlaylist,
    Search,
    /// Yes/no question; the payload says what is being confirmed.
    Confirm(Confirm),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirm {
    DeleteGroup(Vec<usize>),
    Overwrite(std::path::PathBuf),
}

#[derive(Debug, Clone)]
pub struct Prompt {
    pub kind: PromptKind,
    pub label: String,
    pub input: String,
}

pub enum PromptResult {
    /// Still editing.
    Editing,
    Submitted(String),
    Cancelled,
}

impl Prompt {
    pub fn new(kind: PromptKind, label: impl Into<String>, initial: impl Into<String>) -> Self {
        Prompt {
            kind,
            label: label.into(),
            input: initial.into(),
        }
    }

    pub fn is_confirm(&self) -> bool {
        matches!(self.kind, PromptKind::Confirm(_))
    }

    /// Applies one key press.
    pub fn handle(&mut self, key: KeyBinding) -> PromptResult {
        if self.is_confirm() {
            return match key.code {
                KeyCode::Char('y' | 'Y') | KeyCode::Enter => PromptResult::Submitted("y".into()),
                _ => PromptResult::Cancelled,
            };
        }
        let ctrl = key.mods.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Enter => PromptResult::Submitted(self.input.trim().to_string()),
            KeyCode::Esc => PromptResult::Cancelled,
            KeyCode::Backspace => {
                self.input.pop();
                PromptResult::Editing
            }
            KeyCode::Char('u') if ctrl => {
                self.input.clear();
                PromptResult::Editing
            }
            KeyCode::Char('c') if ctrl => PromptResult::Cancelled,
            KeyCode::Char(c) if !ctrl && !key.mods.contains(KeyModifiers::ALT) => {
                self.input.push(c);
                PromptResult::Editing
            }
            _ => PromptResult::Editing,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(s: &str) -> KeyBinding {
        KeyBinding::parse(s).unwrap()
    }

    #[test]
    fn edits_and_submits() {
        let mut p = Prompt::new(PromptKind::NewGroup, "Group name: ", "");
        for c in ["S", "y", "n", "t", "h", "x"] {
            p.handle(key(c));
        }
        p.handle(key("Backspace"));
        assert!(matches!(p.handle(key("Enter")), PromptResult::Submitted(s) if s == "Synth"));
    }

    #[test]
    fn escape_cancels_and_ctrl_u_clears() {
        let mut p = Prompt::new(PromptKind::Search, "/", "abc");
        p.handle(key("Ctrl+u"));
        assert_eq!(p.input, "");
        assert!(matches!(p.handle(key("Esc")), PromptResult::Cancelled));
    }

    #[test]
    fn confirm_accepts_only_yes() {
        let mut p = Prompt::new(
            PromptKind::Confirm(Confirm::DeleteGroup(vec![0])),
            "Delete? ",
            "",
        );
        assert!(matches!(p.handle(key("y")), PromptResult::Submitted(_)));
        assert!(matches!(p.handle(key("n")), PromptResult::Cancelled));
        assert!(matches!(p.handle(key("q")), PromptResult::Cancelled));
    }
}
