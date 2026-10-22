//! `config.toml` (spec §7). Every key is optional; invalid sections fall back to defaults
//! with a warning instead of stopping the player.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::ui::keymap::{Action, Context, KeyBinding, Keymap};
use crate::ui::theme::{DEFAULT_THEME, THEMES, theme_info};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReplayGainMode {
    #[default]
    Off,
    Track,
    Album,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct General {
    /// Where the file manager starts. Empty means the system Music folder.
    pub default_music_dir: String,
    pub resume_on_startup: bool,
    pub gapless_playback: bool,
    pub audio_buffer_ms: u32,
    pub replaygain: ReplayGainMode,
}

impl Default for General {
    fn default() -> Self {
        General {
            default_music_dir: String::new(),
            resume_on_startup: true,
            gapless_playback: true,
            audio_buffer_ms: 150,
            replaygain: ReplayGainMode::Off,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Audio {
    /// Part of the output device name; empty follows the Windows default device.
    pub output_device: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Ui {
    pub fps: u32,
    pub show_spectrum: bool,
    pub color_theme: String,
    pub charset: String,
    pub show_hidden: bool,
}

impl Default for Ui {
    fn default() -> Self {
        Ui {
            fps: 60,
            show_spectrum: true,
            color_theme: DEFAULT_THEME.into(),
            charset: "unicode".into(),
            show_hidden: false,
        }
    }
}

/// A key list in `[keybindings]`: `play_pause = ["Space", "c"]` or `stop = "s"`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum KeyList {
    One(String),
    Many(Vec<String>),
}

impl KeyList {
    fn keys(&self) -> Vec<&str> {
        match self {
            KeyList::One(k) => vec![k.as_str()],
            KeyList::Many(v) => v.iter().map(String::as_str).collect(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Config {
    pub general: General,
    pub audio: Audio,
    pub ui: Ui,
    pub keybindings: Keybindings,
}

/// `[keybindings]` rebinds actions everywhere; `[keybindings.file_manager]` only inside the
/// file manager (where e.g. Space selects instead of pausing).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Keybindings {
    pub file_manager: BTreeMap<String, KeyList>,
    #[serde(flatten)]
    pub main: BTreeMap<String, KeyList>,
}

/// Names accepted for `[ui] color_theme`.
pub fn theme_names() -> Vec<&'static str> {
    THEMES.iter().map(|t| t.name).collect()
}

pub struct Loaded {
    pub config: Config,
    pub path: Option<PathBuf>,
    pub warnings: Vec<String>,
}

fn section<T: DeserializeOwned + Default>(
    doc: &toml::Table,
    name: &str,
    warnings: &mut Vec<String>,
) -> T {
    match doc.get(name) {
        None => T::default(),
        Some(value) => value.clone().try_into().unwrap_or_else(|e| {
            warnings.push(format!(
                "config [{name}]: {e}; using defaults for this section"
            ));
            T::default()
        }),
    }
}

/// Parses config text, collecting problems as warnings.
pub fn parse(text: &str) -> (Config, Vec<String>) {
    let mut warnings = Vec::new();
    let doc: toml::Table = match toml::from_str(text) {
        Ok(doc) => doc,
        Err(e) => {
            warnings.push(format!(
                "config.toml is not valid TOML ({}); using defaults",
                e.message()
            ));
            return (Config::default(), warnings);
        }
    };
    for key in doc.keys() {
        if !["general", "audio", "ui", "keybindings"].contains(&key.as_str()) {
            warnings.push(format!("config: unknown section [{key}]"));
        }
    }
    let mut config = Config {
        general: section(&doc, "general", &mut warnings),
        audio: section(&doc, "audio", &mut warnings),
        ui: section(&doc, "ui", &mut warnings),
        keybindings: section(&doc, "keybindings", &mut warnings),
    };

    let ui = &mut config.ui;
    if !(1..=240).contains(&ui.fps) {
        warnings.push(format!("config: fps {} out of range 1-240", ui.fps));
        ui.fps = ui.fps.clamp(1, 240);
    }
    if theme_info(&ui.color_theme).is_none() {
        warnings.push(format!(
            "config: unknown color_theme \"{}\" (use {})",
            ui.color_theme,
            theme_names().join(", ")
        ));
        ui.color_theme = Ui::default().color_theme;
    }
    if !["unicode", "ascii"].contains(&ui.charset.as_str()) {
        warnings.push(format!(
            "config: charset must be \"unicode\" or \"ascii\", not \"{}\"",
            ui.charset
        ));
        ui.charset = Ui::default().charset;
    }
    let general = &mut config.general;
    if !(50..=2000).contains(&general.audio_buffer_ms) {
        warnings.push(format!(
            "config: audio_buffer_ms {} out of range 50-2000",
            general.audio_buffer_ms
        ));
        general.audio_buffer_ms = general.audio_buffer_ms.clamp(50, 2000);
    }
    (config, warnings)
}

/// Loads `path` (or the default location). A missing file silently gives the defaults.
pub fn load(path: Option<&Path>) -> Loaded {
    let path = path
        .map(Path::to_path_buf)
        .or_else(crate::paths::config_file);
    let Some(file) = path.as_deref() else {
        return Loaded {
            config: Config::default(),
            path: None,
            warnings: Vec::new(),
        };
    };
    match std::fs::read_to_string(file) {
        Ok(text) => {
            let (config, warnings) = parse(&text);
            Loaded {
                config,
                path,
                warnings,
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Loaded {
            config: Config::default(),
            path,
            warnings: Vec::new(),
        },
        Err(e) => Loaded {
            config: Config::default(),
            warnings: vec![format!("cannot read {}: {e}", file.display())],
            path,
        },
    }
}

impl Config {
    /// The default keymap with the `[keybindings]` overrides applied. Returns warnings for
    /// unknown actions and unparsable keys.
    pub fn keymap(&self) -> (Keymap, Vec<String>) {
        let mut keymap = Keymap::default();
        let mut warnings = Vec::new();
        let sections = [
            (&self.keybindings.main, None, "[keybindings]"),
            (
                &self.keybindings.file_manager,
                Some(Context::FileManager),
                "[keybindings.file_manager]",
            ),
        ];
        for (map, ctx, section) in sections {
            for (name, list) in map {
                let Some(action) = Action::from_config_name(name) else {
                    warnings.push(format!("config: unknown action \"{name}\" in {section}"));
                    continue;
                };
                let mut keys = Vec::new();
                for key in list.keys() {
                    match KeyBinding::parse(key) {
                        Ok(k) => keys.push(k),
                        Err(e) => warnings.push(format!("config: {name}: {e}")),
                    }
                }
                if keys.is_empty() {
                    continue;
                }
                match ctx {
                    Some(ctx) => keymap.rebind_in(ctx, action, &keys),
                    None => keymap.rebind(action, &keys),
                }
            }
        }
        (keymap, warnings)
    }

    /// The folder the file manager starts in.
    pub fn music_dir(&self) -> PathBuf {
        let dir = self.general.default_music_dir.trim();
        if dir.is_empty() {
            return crate::paths::default_music_dir();
        }
        let path = crate::paths::expand_tilde(dir);
        if path.is_dir() {
            path
        } else {
            crate::paths::default_music_dir()
        }
    }

    pub fn output_device(&self) -> Option<String> {
        Some(self.audio.output_device.trim().to_string()).filter(|d| !d.is_empty())
    }
}

/// Sets `[section] key = "value"` in the config file, keeping its comments and layout.
/// A missing file is created from the commented defaults first.
pub fn set_string(path: &Path, section: &str, key: &str, value: &str) -> Result<(), String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => default_file_text(),
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|_| format!("{} is not valid TOML; fix it first", path.display()))?;
    if !doc.contains_key(section) {
        doc[section] = toml_edit::table();
    }
    let Some(table) = doc[section].as_table_like_mut() else {
        return Err(format!("[{section}] in {} is not a table", path.display()));
    };
    table.insert(key, toml_edit::value(value));
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, doc.to_string()).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

/// A commented config file with every setting at its default.
pub fn default_file_text() -> String {
    let mut out = String::from(
        "# mp3blaster-rs configuration. Every setting is optional.\n\
         \n\
         [general]\n\
         # Where the file manager starts; empty uses your Music folder.\n\
         default_music_dir = \"\"\n\
         resume_on_startup = true\n\
         gapless_playback = true\n\
         audio_buffer_ms = 150\n\
         # off, track or album\n\
         replaygain = \"off\"\n\
         \n\
         [audio]\n\
         # Part of an output device name (see --list-devices); empty follows the Windows default.\n\
         output_device = \"\"\n\
         \n\
         [ui]\n\
         fps = 60\n\
         show_spectrum = true\n\
         # THEME_LIST\n\
         color_theme = \"DEFAULT_THEME\"\n\
         # unicode or ascii (use ascii in the legacy console with raster fonts)\n\
         charset = \"unicode\"\n\
         show_hidden = false\n\
         \n\
         [keybindings]\n\
         # Uncomment and edit to rebind. Keys: letters, Space, Enter, Esc, Tab, Backspace, Del,\n\
         # Ins, Left/Right/Up/Down, PgUp/PgDn, Home/End, F1-F24, with Ctrl+/Alt+/Shift+ prefixes.\n",
    )
    .replace(
        "THEME_LIST",
        &format!("{} (or press t in the player)", theme_names().join(", ")),
    )
    .replace("DEFAULT_THEME", DEFAULT_THEME);
    let defaults = Keymap::default();
    let line = |out: &mut String, action: Action, keys: Vec<KeyBinding>| {
        let quoted: Vec<String> = keys
            .iter()
            .map(|k| {
                format!(
                    "\"{}\"",
                    k.to_string().replace('\\', "\\\\").replace('"', "\\\"")
                )
            })
            .collect();
        let _ = writeln!(out, "# {} = [{}]", action.config_name(), quoted.join(", "));
    };
    for &action in Action::ALL {
        let home = defaults.home_context(action);
        if home != Context::FileManager {
            line(&mut out, action, defaults.keys_exact(home, action));
        }
    }
    out.push_str(
        "\n[keybindings.file_manager]\n\
         # Keys that apply only inside the file manager (they take precedence there).\n",
    );
    for &action in Action::ALL {
        let keys = defaults.keys_exact(Context::FileManager, action);
        if !keys.is_empty() {
            line(&mut out, action, keys);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC_SAMPLE: &str = r#"
[general]
default_music_dir = "~/Music"
resume_on_startup = true
gapless_playback = true
audio_buffer_ms = 150

[ui]
fps = 60
show_spectrum = true
color_theme = "classic_blue" # Options: classic_blue, gruvbox, catppuccin, monochrome
charset = "unicode"          # Options: unicode, ascii

[keybindings]
play_pause = ["Space", "c"]
next_track = ["n", "6"]
prev_track = ["b", "4"]
seek_forward = ["Right", "3"]
seek_backward = ["Left", "1"]
"#;

    #[test]
    fn spec_sample_parses_without_warnings() {
        let (config, warnings) = parse(SPEC_SAMPLE);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(config.general.default_music_dir, "~/Music");
        assert_eq!(config.ui.color_theme, "classic_blue");
        let (keymap, warnings) = config.keymap();
        assert!(warnings.is_empty(), "{warnings:?}");
        let ctx = Context::Playlist;
        assert_eq!(
            keymap.lookup(ctx, &KeyBinding::plain('c')),
            Some(Action::PlayPause)
        );
        // "5" was a default for play/pause but the sample's list replaces the defaults.
        assert_eq!(keymap.lookup(ctx, &KeyBinding::plain('5')), None);
    }

    #[test]
    fn bad_values_fall_back_with_warnings() {
        let (config, warnings) = parse(
            "[ui]\nfps = 1000\ncolor_theme = \"neon\"\ncharset = \"emoji\"\n[general]\naudio_buffer_ms = \"lots\"\n[extra]\n",
        );
        assert_eq!(config.ui.fps, 240);
        assert_eq!(config.ui.color_theme, DEFAULT_THEME);
        assert_eq!(config.ui.charset, "unicode");
        assert_eq!(config.general, General::default());
        assert_eq!(warnings.len(), 5, "{warnings:?}");
    }

    #[test]
    fn invalid_toml_gives_defaults() {
        let (config, warnings) = parse("this is = = not toml");
        assert_eq!(config, Config::default());
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn keybinding_errors_are_reported() {
        let (config, _) =
            parse("[keybindings]\nfly = \"x\"\nstop = [\"Hyper+s\", \"x\"]\nquit = \"Q\"\n");
        let (keymap, warnings) = config.keymap();
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert_eq!(
            keymap.lookup(Context::Playlist, &KeyBinding::plain('x')),
            Some(Action::Stop)
        );
        assert_eq!(
            keymap.lookup(Context::Playlist, &KeyBinding::plain('Q')),
            Some(Action::Quit)
        );
        assert_eq!(
            keymap.lookup(Context::Playlist, &KeyBinding::plain('q')),
            None
        );
    }

    #[test]
    fn default_file_parses_to_defaults() {
        let text = default_file_text();
        let (config, warnings) = parse(&text);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(config, Config::default());
        assert!(
            text.contains("# play_pause = [\"Space\", \"c\", \"5\"]"),
            "{text}"
        );
        // Uncommenting every binding line must leave the keymap exactly as it was.
        let uncommented: String = text
            .lines()
            .map(|l| match l.strip_prefix("# ") {
                Some(rest) if rest.contains(" = [") => rest,
                _ => l,
            })
            .map(|l| format!("{l}\n"))
            .collect();
        let (config, warnings) = parse(&uncommented);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(!config.keybindings.main.is_empty());
        assert!(!config.keybindings.file_manager.is_empty());
        let (keymap, warnings) = config.keymap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(keymap.all_bindings(), Keymap::default().all_bindings());
    }

    #[test]
    fn file_manager_section_only_affects_the_file_manager() {
        let (config, warnings) = parse(
            "[keybindings]\nactivate = [\"Enter\", \"o\"]\n[keybindings.file_manager]\nselect = \"x\"\n",
        );
        assert!(warnings.is_empty(), "{warnings:?}");
        let (keymap, _) = config.keymap();
        let o = KeyBinding::plain('o');
        let x = KeyBinding::plain('x');
        assert_eq!(keymap.lookup(Context::Playlist, &o), Some(Action::Activate));
        // The file manager keeps its own activate keys and gets the new select key.
        assert_eq!(
            keymap.lookup(Context::FileManager, &KeyBinding::plain('l')),
            Some(Action::Activate)
        );
        assert_eq!(
            keymap.lookup(Context::FileManager, &x),
            Some(Action::Select)
        );
        assert_eq!(keymap.lookup(Context::Playlist, &x), None);
        assert_eq!(
            keymap.lookup(Context::FileManager, &KeyBinding::plain(' ')),
            Some(Action::PlayPause)
        );
        // Next-track on "l" is untouched outside the file manager.
        assert_eq!(
            keymap.lookup(Context::Playlist, &KeyBinding::plain('l')),
            Some(Action::NextTrack)
        );
    }

    #[test]
    fn set_string_keeps_comments_and_creates_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfg").join("config.toml");
        set_string(&path, "ui", "color_theme", "nord").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("color_theme = \"nord\""), "{text}");
        assert!(
            text.contains("# mp3blaster-rs configuration"),
            "defaults were not written"
        );
        assert_eq!(parse(&text).0.ui.color_theme, "nord");

        std::fs::write(
            &path,
            "# my notes\n[general]\nreplaygain = \"track\" # loud\n",
        )
        .unwrap();
        set_string(&path, "ui", "color_theme", "dracula").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("# my notes") && text.contains("# loud"),
            "{text}"
        );
        let (config, warnings) = parse(&text);
        assert!(warnings.is_empty());
        assert_eq!(config.ui.color_theme, "dracula");
        assert_eq!(config.general.replaygain, ReplayGainMode::Track);

        std::fs::write(&path, "this = = broken").unwrap();
        assert!(set_string(&path, "ui", "color_theme", "nord").is_err());
    }

    #[test]
    fn missing_file_is_silent() {
        let loaded = load(Some(Path::new("Z:/definitely/not/here/config.toml")));
        assert!(loaded.warnings.is_empty());
        assert_eq!(loaded.config, Config::default());
    }
}
