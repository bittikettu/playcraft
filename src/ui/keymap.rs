//! Keyboard actions, default bindings (spec §6.3) and key normalisation.
//!
//! Windows notes:
//! - crossterm reports key *release* events on Windows; only presses (and repeats) count.
//! - On non-US layouts AltGr arrives as Ctrl+Alt+char; such keys are treated as plain chars.
//! - With NumLock on, keypad digits arrive as plain digits, so the classic keypad bindings
//!   (1–6) work through the digit keys.

use std::collections::HashMap;
use std::fmt;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Action {
    Quit,
    PlayPause,
    Stop,
    NextTrack,
    PrevTrack,
    SeekForward,
    SeekBackward,
    VolumeUp,
    VolumeDown,
    CursorUp,
    CursorDown,
    PageUp,
    PageDown,
    Home,
    End,
    /// Play the selected track / enter the selected group or folder.
    Activate,
    /// Leave the current group or folder.
    Back,
    FileManager,
    NewGroup,
    LoadPlaylist,
    SavePlaylist,
    Help,
    Search,
    Select,
    InvertSelection,
    SelectGlob,
    AddSelected,
    ToggleHidden,
    CyclePlayMode,
    CycleRepeat,
    ToggleGroupShuffle,
    RenameGroup,
    Delete,
    MoveUp,
    MoveDown,
    ShowLog,
    Close,
    #[cfg(debug_assertions)]
    DebugPanic,
}

impl Action {
    /// All actions, in the order they are listed in the help screen.
    pub const ALL: &[Action] = &[
        Action::PlayPause,
        Action::Stop,
        Action::NextTrack,
        Action::PrevTrack,
        Action::SeekForward,
        Action::SeekBackward,
        Action::VolumeUp,
        Action::VolumeDown,
        Action::CyclePlayMode,
        Action::CycleRepeat,
        Action::ToggleGroupShuffle,
        Action::CursorUp,
        Action::CursorDown,
        Action::PageUp,
        Action::PageDown,
        Action::Home,
        Action::End,
        Action::Activate,
        Action::Back,
        Action::FileManager,
        Action::NewGroup,
        Action::RenameGroup,
        Action::Delete,
        Action::MoveUp,
        Action::MoveDown,
        Action::LoadPlaylist,
        Action::SavePlaylist,
        Action::Search,
        Action::Select,
        Action::InvertSelection,
        Action::SelectGlob,
        Action::AddSelected,
        Action::ToggleHidden,
        Action::Help,
        Action::ShowLog,
        Action::Close,
        Action::Quit,
    ];

    /// Name used in `config.toml` (`[keybindings]`).
    pub fn config_name(self) -> &'static str {
        match self {
            Action::Quit => "quit",
            Action::PlayPause => "play_pause",
            Action::Stop => "stop",
            Action::NextTrack => "next_track",
            Action::PrevTrack => "prev_track",
            Action::SeekForward => "seek_forward",
            Action::SeekBackward => "seek_backward",
            Action::VolumeUp => "volume_up",
            Action::VolumeDown => "volume_down",
            Action::CursorUp => "cursor_up",
            Action::CursorDown => "cursor_down",
            Action::PageUp => "page_up",
            Action::PageDown => "page_down",
            Action::Home => "home",
            Action::End => "end",
            Action::Activate => "activate",
            Action::Back => "back",
            Action::FileManager => "file_manager",
            Action::NewGroup => "new_group",
            Action::LoadPlaylist => "load_playlist",
            Action::SavePlaylist => "save_playlist",
            Action::Help => "help",
            Action::Search => "search",
            Action::Select => "select",
            Action::InvertSelection => "invert_selection",
            Action::SelectGlob => "select_glob",
            Action::AddSelected => "add_selected",
            Action::ToggleHidden => "toggle_hidden",
            Action::CyclePlayMode => "cycle_play_mode",
            Action::CycleRepeat => "cycle_repeat",
            Action::ToggleGroupShuffle => "toggle_group_shuffle",
            Action::RenameGroup => "rename_group",
            Action::Delete => "delete",
            Action::MoveUp => "move_up",
            Action::MoveDown => "move_down",
            Action::ShowLog => "show_log",
            Action::Close => "close",
            #[cfg(debug_assertions)]
            Action::DebugPanic => "debug_panic",
        }
    }

    pub fn from_config_name(name: &str) -> Option<Action> {
        Action::ALL.iter().copied().find(|a| a.config_name() == name)
    }

    pub fn description(self) -> &'static str {
        match self {
            Action::Quit => "Quit",
            Action::PlayPause => "Play / pause",
            Action::Stop => "Stop playback",
            Action::NextTrack => "Next track",
            Action::PrevTrack => "Previous track",
            Action::SeekForward => "Seek +5 s",
            Action::SeekBackward => "Seek -5 s",
            Action::VolumeUp => "Volume up",
            Action::VolumeDown => "Volume down",
            Action::CursorUp => "Cursor up",
            Action::CursorDown => "Cursor down",
            Action::PageUp => "Page up",
            Action::PageDown => "Page down",
            Action::Home => "First item",
            Action::End => "Last item",
            Action::Activate => "Play track / open group or folder",
            Action::Back => "Parent group or folder",
            Action::FileManager => "File manager (add files)",
            Action::NewGroup => "Create new group",
            Action::LoadPlaylist => "Load playlist",
            Action::SavePlaylist => "Save playlist",
            Action::Help => "This help",
            Action::Search => "Search / filter",
            Action::Select => "Select item (file manager)",
            Action::InvertSelection => "Invert selection (file manager)",
            Action::SelectGlob => "Select by pattern (file manager)",
            Action::AddSelected => "Add selection to playlist (file manager)",
            Action::ToggleHidden => "Show hidden files (file manager)",
            Action::CyclePlayMode => "Cycle play mode",
            Action::CycleRepeat => "Cycle repeat mode",
            Action::ToggleGroupShuffle => "Shuffle current group on/off",
            Action::RenameGroup => "Rename group",
            Action::Delete => "Remove item",
            Action::MoveUp => "Move item up",
            Action::MoveDown => "Move item down",
            Action::ShowLog => "Message log",
            Action::Close => "Close view / cancel",
            #[cfg(debug_assertions)]
            Action::DebugPanic => "Crash on purpose (debug builds)",
        }
    }
}

/// Where a key press is interpreted. Context bindings win over global ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Context {
    Global,
    Playlist,
    FileManager,
}

/// A normalised key: chars never carry SHIFT (the char itself is already shifted).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyBinding {
    pub code: KeyCode,
    pub mods: KeyModifiers,
}

impl KeyBinding {
    pub fn new(code: KeyCode, mods: KeyModifiers) -> Self {
        let mut mods = mods & (KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT);
        if matches!(code, KeyCode::Char(_)) {
            mods.remove(KeyModifiers::SHIFT);
        }
        KeyBinding { code, mods }
    }

    pub fn plain(c: char) -> Self {
        KeyBinding::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    /// Converts a terminal key event, or `None` for releases.
    pub fn from_event(ev: &KeyEvent) -> Option<Self> {
        if !matches!(ev.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return None;
        }
        let mut mods = ev.modifiers;
        if let KeyCode::Char(c) = ev.code {
            // AltGr on Windows: Ctrl+Alt+<printable> is just the printable char.
            if mods.contains(KeyModifiers::CONTROL | KeyModifiers::ALT) && !c.is_control() {
                mods.remove(KeyModifiers::CONTROL | KeyModifiers::ALT);
            }
        }
        Some(KeyBinding::new(ev.code, mods))
    }

    /// Parses config strings such as `"Space"`, `"c"`, `"Ctrl+u"`, `"F1"`, `"PgDn"`.
    pub fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim();
        if s.is_empty() {
            return Err("empty key".into());
        }
        let mut mods = KeyModifiers::NONE;
        let mut rest = s;
        // A lone "+" or "-" is a key, not a separator.
        while rest.chars().count() > 1 {
            let Some((head, tail)) = rest.split_once('+') else { break };
            if tail.is_empty() {
                break;
            }
            match head.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => mods |= KeyModifiers::CONTROL,
                "alt" | "meta" => mods |= KeyModifiers::ALT,
                "shift" => mods |= KeyModifiers::SHIFT,
                _ => return Err(format!("unknown modifier \"{head}\" in \"{s}\"")),
            }
            rest = tail;
        }
        let code = match rest.to_ascii_lowercase().as_str() {
            "space" => KeyCode::Char(' '),
            "enter" | "return" => KeyCode::Enter,
            "esc" | "escape" => KeyCode::Esc,
            "tab" => KeyCode::Tab,
            "backtab" => KeyCode::BackTab,
            "backspace" | "bs" => KeyCode::Backspace,
            "delete" | "del" => KeyCode::Delete,
            "insert" | "ins" => KeyCode::Insert,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "pgup" | "pageup" => KeyCode::PageUp,
            "pgdn" | "pagedown" => KeyCode::PageDown,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            lower => {
                if let Some(n) = lower.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
                    if (1..=24).contains(&n) {
                        KeyCode::F(n)
                    } else {
                        return Err(format!("no such function key \"{rest}\""));
                    }
                } else {
                    let mut chars = rest.chars();
                    match (chars.next(), chars.next()) {
                        (Some(c), None) => KeyCode::Char(c),
                        _ => return Err(format!("unknown key \"{rest}\"")),
                    }
                }
            }
        };
        Ok(KeyBinding::new(code, mods))
    }
}

impl fmt::Display for KeyBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.mods.contains(KeyModifiers::CONTROL) {
            write!(f, "Ctrl+")?;
        }
        if self.mods.contains(KeyModifiers::ALT) {
            write!(f, "Alt+")?;
        }
        if self.mods.contains(KeyModifiers::SHIFT) {
            write!(f, "Shift+")?;
        }
        match self.code {
            KeyCode::Char(' ') => write!(f, "Space"),
            KeyCode::Char(c) => write!(f, "{c}"),
            KeyCode::F(n) => write!(f, "F{n}"),
            KeyCode::Enter => write!(f, "Enter"),
            KeyCode::Esc => write!(f, "Esc"),
            KeyCode::Tab => write!(f, "Tab"),
            KeyCode::BackTab => write!(f, "BackTab"),
            KeyCode::Backspace => write!(f, "Backspace"),
            KeyCode::Delete => write!(f, "Del"),
            KeyCode::Insert => write!(f, "Ins"),
            KeyCode::Left => write!(f, "Left"),
            KeyCode::Right => write!(f, "Right"),
            KeyCode::Up => write!(f, "Up"),
            KeyCode::Down => write!(f, "Down"),
            KeyCode::PageUp => write!(f, "PgUp"),
            KeyCode::PageDown => write!(f, "PgDn"),
            KeyCode::Home => write!(f, "Home"),
            KeyCode::End => write!(f, "End"),
            other => write!(f, "{other:?}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Keymap {
    /// Per context: key -> (action, insertion order). The order keeps the preferred key first.
    bindings: HashMap<Context, HashMap<KeyBinding, (Action, usize)>>,
    next_order: usize,
}

fn k(s: &str) -> KeyBinding {
    KeyBinding::parse(s).unwrap_or_else(|e| panic!("bad default key {s}: {e}"))
}

impl Default for Keymap {
    fn default() -> Self {
        use Action::*;
        use Context as C;
        let defaults: &[(Context, Action, &[&str])] = &[
            (C::Global, PlayPause, &["Space", "c", "5"]),
            (C::Global, Stop, &["s", "2"]),
            (C::Global, PrevTrack, &["b", "h", "4"]),
            (C::Global, NextTrack, &["n", "l", "6"]),
            (C::Global, SeekBackward, &["Left", "1"]),
            (C::Global, SeekForward, &["Right", "3"]),
            (C::Global, VolumeDown, &["-", "<"]),
            (C::Global, VolumeUp, &["+", "=", ">"]),
            (C::Global, CursorUp, &["Up", "k"]),
            (C::Global, CursorDown, &["Down", "j"]),
            (C::Global, PageUp, &["PgUp", "Ctrl+u"]),
            (C::Global, PageDown, &["PgDn", "Ctrl+d"]),
            (C::Global, Home, &["Home"]),
            (C::Global, End, &["End"]),
            (C::Global, Activate, &["Enter"]),
            (C::Global, Back, &["Backspace"]),
            (C::Global, FileManager, &["F1", "a"]),
            (C::Global, NewGroup, &["F2", "g"]),
            (C::Global, LoadPlaylist, &["F3"]),
            (C::Global, SavePlaylist, &["F4"]),
            (C::Global, Help, &["?"]),
            (C::Global, Search, &["/"]),
            (C::Global, CyclePlayMode, &["m"]),
            (C::Global, CycleRepeat, &["r"]),
            (C::Global, ToggleGroupShuffle, &["z"]),
            (C::Global, ShowLog, &["L"]),
            (C::Global, Close, &["Esc"]),
            (C::Global, Quit, &["q", "Ctrl+c"]),
            (C::Playlist, RenameGroup, &["R"]),
            (C::Playlist, Delete, &["d", "Delete"]),
            (C::Playlist, MoveUp, &["K"]),
            (C::Playlist, MoveDown, &["J"]),
            (C::FileManager, Select, &["Space", "v"]),
            (C::FileManager, InvertSelection, &["V"]),
            (C::FileManager, SelectGlob, &["*"]),
            (C::FileManager, AddSelected, &["a", "Insert"]),
            (C::FileManager, Activate, &["Enter", "l", "Right"]),
            (C::FileManager, Back, &["Backspace", "h", "Left"]),
            (C::FileManager, ToggleHidden, &["."]),
            (C::FileManager, Close, &["Esc", "F1"]),
            #[cfg(debug_assertions)]
            (C::Global, DebugPanic, &["Ctrl+F12"]),
        ];
        let mut map = Keymap { bindings: HashMap::new(), next_order: 0 };
        for (ctx, action, keys) in defaults {
            for key in *keys {
                map.bind(*ctx, k(key), *action);
            }
        }
        map
    }
}

impl Keymap {
    pub fn bind(&mut self, ctx: Context, key: KeyBinding, action: Action) {
        self.next_order += 1;
        self.bindings.entry(ctx).or_default().insert(key, (action, self.next_order));
    }

    /// Replaces every binding of `action` (in all contexts) with `keys` in the global context.
    pub fn rebind(&mut self, action: Action, keys: &[KeyBinding]) {
        for map in self.bindings.values_mut() {
            map.retain(|_, (a, _)| *a != action);
        }
        for key in keys {
            self.bind(Context::Global, *key, action);
        }
    }

    pub fn lookup(&self, ctx: Context, key: &KeyBinding) -> Option<Action> {
        self.bindings
            .get(&ctx)
            .and_then(|m| m.get(key))
            .or_else(|| self.bindings.get(&Context::Global).and_then(|m| m.get(key)))
            .map(|(a, _)| *a)
    }

    /// Keys bound to `action` in `ctx` (falling back to global), for help and hint texts.
    pub fn keys_for(&self, ctx: Context, action: Action) -> Vec<KeyBinding> {
        let collect = |c: Context| -> Vec<KeyBinding> {
            let mut keys: Vec<(usize, KeyBinding)> = self
                .bindings
                .get(&c)
                .map(|m| m.iter().filter(|(_, (a, _))| *a == action).map(|(k, (_, o))| (*o, *k)).collect())
                .unwrap_or_default();
            keys.sort_by_key(|(order, _)| *order);
            keys.into_iter().map(|(_, k)| k).collect()
        };
        let keys = collect(ctx);
        if keys.is_empty() && ctx != Context::Global { collect(Context::Global) } else { keys }
    }

    /// The first key for `action`, formatted, or "?" if unbound.
    pub fn hint(&self, ctx: Context, action: Action) -> String {
        self.keys_for(ctx, action).first().map_or_else(|| "?".into(), |k| k.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new_with_kind(code, mods, KeyEventKind::Press)
    }

    #[test]
    fn parses_key_strings() {
        assert_eq!(KeyBinding::parse("Space").unwrap(), KeyBinding::plain(' '));
        assert_eq!(KeyBinding::parse("c").unwrap(), KeyBinding::plain('c'));
        assert_eq!(KeyBinding::parse("+").unwrap(), KeyBinding::plain('+'));
        assert_eq!(KeyBinding::parse("-").unwrap(), KeyBinding::plain('-'));
        assert_eq!(
            KeyBinding::parse("Ctrl+u").unwrap(),
            KeyBinding::new(KeyCode::Char('u'), KeyModifiers::CONTROL)
        );
        assert_eq!(
            KeyBinding::parse("ctrl+alt+Del").unwrap(),
            KeyBinding::new(KeyCode::Delete, KeyModifiers::CONTROL | KeyModifiers::ALT)
        );
        assert_eq!(KeyBinding::parse("F12").unwrap().code, KeyCode::F(12));
        assert_eq!(KeyBinding::parse("PgDn").unwrap().code, KeyCode::PageDown);
        assert_eq!(KeyBinding::parse("Ctrl++").unwrap(), KeyBinding::new(KeyCode::Char('+'), KeyModifiers::CONTROL));
        assert!(KeyBinding::parse("Hyper+x").is_err());
        assert!(KeyBinding::parse("F99").is_err());
        assert!(KeyBinding::parse("abc").is_err());
    }

    #[test]
    fn display_round_trips() {
        for s in ["Space", "c", "Ctrl+u", "F1", "PgUp", "Left", "Alt+x", "Shift+Tab", "Del"] {
            let k = KeyBinding::parse(s).unwrap();
            assert_eq!(KeyBinding::parse(&k.to_string()).unwrap(), k, "{s}");
        }
    }

    #[test]
    fn ignores_key_releases() {
        let ev = KeyEvent::new_with_kind(KeyCode::Char('q'), KeyModifiers::NONE, KeyEventKind::Release);
        assert_eq!(KeyBinding::from_event(&ev), None);
    }

    #[test]
    fn altgr_and_shift_are_normalised() {
        // AltGr+7 on a Nordic layout produces '{' reported as Ctrl+Alt+'{'.
        let altgr = press(KeyCode::Char('{'), KeyModifiers::CONTROL | KeyModifiers::ALT);
        assert_eq!(KeyBinding::from_event(&altgr), Some(KeyBinding::plain('{')));
        let shifted = press(KeyCode::Char('V'), KeyModifiers::SHIFT);
        assert_eq!(KeyBinding::from_event(&shifted), Some(KeyBinding::plain('V')));
    }

    #[test]
    fn context_bindings_override_global() {
        let km = Keymap::default();
        let space = KeyBinding::plain(' ');
        assert_eq!(km.lookup(Context::Playlist, &space), Some(Action::PlayPause));
        assert_eq!(km.lookup(Context::FileManager, &space), Some(Action::Select));
        assert_eq!(km.lookup(Context::Playlist, &KeyBinding::plain('h')), Some(Action::PrevTrack));
        assert_eq!(km.lookup(Context::FileManager, &KeyBinding::plain('h')), Some(Action::Back));
        assert_eq!(km.lookup(Context::FileManager, &KeyBinding::plain('c')), Some(Action::PlayPause));
    }

    #[test]
    fn classic_and_modern_defaults_from_spec() {
        let km = Keymap::default();
        let ctx = Context::Playlist;
        for (key, action) in [
            ("5", Action::PlayPause),
            ("2", Action::Stop),
            ("4", Action::PrevTrack),
            ("6", Action::NextTrack),
            ("1", Action::SeekBackward),
            ("3", Action::SeekForward),
            ("F1", Action::FileManager),
            ("F2", Action::NewGroup),
            ("g", Action::NewGroup),
            ("Ctrl+d", Action::PageDown),
            ("q", Action::Quit),
        ] {
            assert_eq!(km.lookup(ctx, &KeyBinding::parse(key).unwrap()), Some(action), "{key}");
        }
    }

    #[test]
    fn rebind_replaces_all_previous_keys() {
        let mut km = Keymap::default();
        km.rebind(Action::PlayPause, &[KeyBinding::plain('x')]);
        assert_eq!(km.lookup(Context::Playlist, &KeyBinding::plain('x')), Some(Action::PlayPause));
        assert_eq!(km.lookup(Context::Playlist, &KeyBinding::plain('c')), None);
        assert_eq!(km.hint(Context::Playlist, Action::PlayPause), "x");
    }

    #[test]
    fn config_names_round_trip() {
        for &a in Action::ALL {
            assert_eq!(Action::from_config_name(a.config_name()), Some(a));
        }
    }
}
