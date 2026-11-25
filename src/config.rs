use anyhow::{Context, Result, bail};
use mlua::{Lua, LuaSerdeExt, Value};
use ratatui::{
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers},
    style::Color,
};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
};

pub const DEFAULT_CONFIG: &str = include_str!("../config.example.lua");

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub show_logo: bool,
    pub differences_only: bool,
    pub ignore_whitespace: bool,
    pub jump_after_merge: bool,
    pub include: String,
    pub exclude: String,
    pub left_label: String,
    pub right_label: String,
    pub tree_percent: u16,
    pub scrolloff: usize,
    pub tab_width: usize,
    pub poll_seconds: u64,
    pub colors: Theme,
    /// Action -> key binding. An empty binding disables the action's key.
    pub keys: BTreeMap<String, String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            show_logo: true,
            differences_only: false,
            ignore_whitespace: false,
            jump_after_merge: true,
            include: String::new(),
            exclude: ".git;.vs;node_modules;target".into(),
            left_label: "Left".into(),
            right_label: "Right".into(),
            tree_percent: 35,
            scrolloff: 5,
            tab_width: 4,
            poll_seconds: 3,
            colors: Theme::default(),
            keys: default_keys(),
        }
    }
}

pub fn default_keys() -> BTreeMap<String, String> {
    [
        ("quit", "q"),
        ("help", "?"),
        ("scroll_down", "j"),
        ("scroll_up", "k"),
        ("scroll_right", "l"),
        ("scroll_left", "h"),
        ("open", "enter"),
        ("reload", "r"),
        ("expand_all", "e"),
        ("collapse_all", "c"),
        ("all", "a"),
        ("differences", "d"),
        ("find", "/"),
        ("copy_right", "alt+l"),
        ("copy_left", "alt+h"),
        ("merge_right", "L"),
        ("merge_left", "H"),
        ("edit_left", "i"),
        ("edit_right", "I"),
        ("delete_both", "dd"),
        ("delete_left", "dh"),
        ("delete_right", "dl"),
        ("undo", "u"),
        ("save_left", "ctrl+s"),
        ("save_right", "ctrl+alt+s"),
        ("save_both", "alt+s"),
        ("accept_edit", "alt+enter"),
        ("next", "n"),
        ("previous", "N"),
        ("select", "v"),
        ("top", "gg"),
        ("bottom", "G"),
        ("center", "zz"),
        ("paths", "p"),
        ("include", "f"),
        ("exclude", "F"),
        ("whitespace", "w"),
        ("focus", "tab"),
    ]
    .into_iter()
    .map(|(action, key)| (action.into(), key.into()))
    .collect()
}

impl Config {
    pub fn load(explicit: Option<&Path>) -> Result<Self> {
        let path = config_path(explicit)?;
        Self::load_path(&path, explicit.is_none())
    }

    fn load_path(path: &Path, create_missing: bool) -> Result<Self> {
        let source = match fs::read_to_string(path) {
            Ok(source) => source,
            Err(error) if create_missing && error.kind() == ErrorKind::NotFound => {
                write_defaults(path, false)?;
                fs::read_to_string(path)
                    .with_context(|| format!("Reading config {}", path.display()))?
            }
            Err(error) => {
                return Err(error).with_context(|| format!("Reading config {}", path.display()));
            }
        };
        Self::from_lua(&source, &path.to_string_lossy())
            .with_context(|| format!("Invalid config {}. Use --reset-config to restore defaults (with --config FILE for a custom path)", path.display()))
    }

    pub fn reset(explicit: Option<&Path>) -> Result<PathBuf> {
        let path = config_path(explicit)?;
        write_defaults(&path, true)?;
        Ok(path)
    }

    fn from_lua(source: &str, name: &str) -> Result<Self> {
        let lua = Lua::new();
        let value: Value = lua
            .load(source)
            .set_name(name)
            .eval()
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        if !matches!(value, Value::Table(_)) {
            bail!("The Lua config must return a table, for example: return {{ show_logo = true }}");
        }
        let mut config: Self = lua
            .from_value(value)
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        // Older config files used tree-specific names for the same navigation keys.
        for (old, new) in [
            ("down", "scroll_down"),
            ("up", "scroll_up"),
            ("expand", "scroll_right"),
            ("collapse", "scroll_left"),
        ] {
            if let Some(binding) = config.keys.remove(old)
                && config.keys.insert(new.into(), binding).is_some()
            {
                bail!("Use keys.{new} only; keys.{old} is its legacy name");
            }
        }
        let defaults = default_keys();
        for action in config.keys.keys() {
            if !defaults.contains_key(action) {
                bail!("Unknown key action: {action}");
            }
        }
        // Older configs bound top to one character that was pressed twice in a
        // file. Keep that meaning, now in the folder tree as well: "g" is "gg".
        if let Some(key) = config.keys.get_mut("top")
            && key.chars().count() == 1
        {
            *key = key.repeat(2);
        }
        // Migrate the original defaults: Alt+s now saves both sides, including
        // when a user copied the old complete example into their Lua file.
        if config.keys.get("save_right").is_some_and(|key| {
            matches!(
                parse_binding(key),
                Ok((KeyCode::Char('s' | 'S'), KeyModifiers::ALT))
            )
        }) {
            config
                .keys
                .insert("save_right".into(), defaults["save_right"].clone());
            if config
                .keys
                .get("save_both")
                .is_some_and(|key| key == "alt+S")
            {
                config
                    .keys
                    .insert("save_both".into(), defaults["save_both"].clone());
            }
        }
        let mut keys = defaults;
        keys.extend(config.keys);
        config.keys = keys;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        if !(10..=80).contains(&self.tree_percent) {
            bail!("tree_percent must be between 10 and 80");
        }
        if !(1..=16).contains(&self.tab_width) {
            bail!("tab_width must be between 1 and 16");
        }
        if !(1..=3600).contains(&self.poll_seconds) {
            bail!("poll_seconds must be between 1 and 3600");
        }
        if self.scrolloff > 100 {
            bail!("scrolloff must be at most 100");
        }
        for name in COLOR_NAMES {
            parse_color(self.colors.value(name)).with_context(|| format!("colors.{name}"))?;
        }
        let mut bindings = HashMap::new();
        let mut sequences = HashMap::new();
        let defaults = default_keys();
        for (action, key) in &self.keys {
            if !defaults.contains_key(action) {
                bail!("Unknown key action: {action}");
            }
            if key.is_empty() {
                continue;
            }
            let previous = if let Some(sequence) = parse_sequence(key) {
                if action == "accept_edit" {
                    bail!("keys.accept_edit must be a single key; the editor types characters");
                }
                sequences.insert(sequence, action)
            } else {
                let binding = parse_binding(key).with_context(|| format!("keys.{action}"))?;
                bindings.insert(binding, action)
            };
            if let Some(previous) = previous {
                bail!(
                    "Key {key:?} is assigned to both {previous} and {action}; set one binding to \"\" to disable it"
                );
            }
        }
        // A sequence waits for its second key, so its first key cannot be a single
        // key where both actions work: "d" filters the tree, "dl" deletes in a file.
        for (&(first, _), sequence) in &sequences {
            if let Some(action) = bindings.get(&(KeyCode::Char(first), KeyModifiers::NONE))
                && [true, false]
                    .into_iter()
                    .any(|tree| applies(action, tree) && applies(sequence, tree))
            {
                bail!(
                    "Key {:?} of {action} also starts {sequence}; set one binding to \"\" to disable it",
                    first.to_string()
                );
            }
        }
        Ok(())
    }

    pub fn action(&self, event: &KeyEvent) -> Option<&str> {
        let binding = normalize(event.code, event.modifiers);
        self.keys.iter().find_map(|(action, key)| {
            (!key.is_empty() && parse_binding(key).ok() == Some(binding)).then_some(action.as_str())
        })
    }

    /// The action of a two-character sequence that starts with `first` and, when
    /// given, continues with `second`, among the actions of the tree or a file.
    pub fn sequence(&self, first: char, second: Option<char>, tree: bool) -> Option<&str> {
        self.keys.iter().find_map(|(action, key)| {
            let (start, end) = parse_sequence(key)?;
            (start == first && second.is_none_or(|second| second == end) && applies(action, tree))
                .then_some(action.as_str())
        })
    }
}

/// Actions that only work in the folder tree or only in an open file.
const TREE_ACTIONS: &[&str] = &["open", "expand_all", "collapse_all", "all", "differences"];
const FILE_ACTIONS: &[&str] = &[
    "merge_right",
    "merge_left",
    "edit_left",
    "edit_right",
    "delete_both",
    "delete_left",
    "delete_right",
    "undo",
    "next",
    "previous",
    "select",
];

/// Whether an action works in the folder tree (`tree`) or in an open file.
pub fn applies(action: &str, tree: bool) -> bool {
    !if tree { FILE_ACTIONS } else { TREE_ACTIONS }.contains(&action)
}

pub fn default_path() -> Option<PathBuf> {
    default_path_for(
        std::env::consts::OS,
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from),
        crate::input::home_dir(),
    )
}

fn default_path_for(
    platform: &str,
    xdg_config: Option<PathBuf>,
    local_app_data: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Option<PathBuf> {
    xdg_config
        .filter(|path| !path.as_os_str().is_empty())
        .or_else(|| match platform {
            "windows" => local_app_data
                .filter(|path| !path.as_os_str().is_empty())
                .or_else(|| home.map(|home| home.join("AppData/Local"))),
            "macos" => home.map(|home| home.join("Library/Application Support")),
            _ => home.map(|home| home.join(".config")),
        })
        .map(|base| base.join("lazymerge/config.lua"))
}

fn config_path(explicit: Option<&Path>) -> Result<PathBuf> {
    explicit
        .map(Path::to_path_buf)
        .or_else(default_path)
        .context("Cannot determine the config path; set XDG_CONFIG_HOME or use --config FILE")
}

fn write_defaults(path: &Path, overwrite: bool) -> Result<()> {
    let parent = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)
        .with_context(|| format!("Creating config directory {}", parent.display()))?;
    // Publish the complete file atomically. Startup must never replace an existing config.
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("Creating config {}", path.display()))?;
    temporary.write_all(DEFAULT_CONFIG.as_bytes())?;
    temporary.as_file().sync_all()?;
    let result = if overwrite {
        temporary.persist(path)
    } else {
        temporary.persist_noclobber(path)
    };
    match result {
        Ok(_) => Ok(()),
        Err(error) if !overwrite && error.error.kind() == ErrorKind::AlreadyExists => Ok(()),
        Err(error) => {
            Err(error.error).with_context(|| format!("Writing config {}", path.display()))
        }
    }
}

type Binding = (KeyCode, KeyModifiers);

fn normalize(code: KeyCode, mut modifiers: KeyModifiers) -> Binding {
    // Terminals differ in whether Shift is reported alongside uppercase/punctuation.
    if matches!(code, KeyCode::Char(_)) {
        modifiers.remove(KeyModifiers::SHIFT);
    }
    (code, modifiers)
}

fn parse_binding(value: &str) -> Result<Binding> {
    if value == "+" {
        return Ok((KeyCode::Char('+'), KeyModifiers::NONE));
    }
    let mut pieces: Vec<&str> = value.split('+').collect();
    let key = pieces.pop().unwrap_or_default();
    let mut modifiers = KeyModifiers::NONE;
    for modifier in pieces {
        modifiers |= match modifier.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => KeyModifiers::CONTROL,
            "alt" => KeyModifiers::ALT,
            "shift" => KeyModifiers::SHIFT,
            _ => bail!("Unknown key modifier {modifier:?}"),
        };
    }
    let mut code = match key.to_ascii_lowercase().as_str() {
        "enter" | "return" => KeyCode::Enter,
        "esc" | "escape" => KeyCode::Esc,
        "tab" => KeyCode::Tab,
        "backtab" => KeyCode::BackTab,
        "space" => KeyCode::Char(' '),
        "backspace" => KeyCode::Backspace,
        "delete" | "del" => KeyCode::Delete,
        "insert" => KeyCode::Insert,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        _ if key.chars().count() == 1 => KeyCode::Char(key.chars().next().unwrap()),
        _ if key.starts_with(['f', 'F']) => {
            let number: u8 = key[1..]
                .parse()
                .context("Expected a function key from f1 to f24")?;
            if !(1..=24).contains(&number) {
                bail!("Expected a function key from f1 to f24");
            }
            KeyCode::F(number)
        }
        _ => bail!("Unknown key {key:?}"),
    };
    if modifiers.contains(KeyModifiers::SHIFT) {
        if let KeyCode::Char(character) = code {
            code = KeyCode::Char(character.to_ascii_uppercase());
        } else if code == KeyCode::Tab {
            code = KeyCode::BackTab;
        }
    }
    Ok(normalize(code, modifiers))
}

/// Two characters such as "gg" or "dl" are typed one after the other. Named keys
/// take precedence, so "f2" remains a function key.
fn parse_sequence(value: &str) -> Option<(char, char)> {
    let mut characters = value.chars();
    let sequence = (characters.next()?, characters.next()?);
    let plain = |character: char| !character.is_control() && !character.is_whitespace();
    (characters.next().is_none()
        && plain(sequence.0)
        && plain(sequence.1)
        && parse_binding(value).is_err())
    .then_some(sequence)
}

pub const COLOR_NAMES: [&str; 15] = [
    "bg",
    "fg",
    "muted",
    "border",
    "selected",
    "different",
    "one_sided",
    "minor",
    "logo_lazy",
    "logo_merge",
    "added",
    "removed",
    "changed",
    "accent",
    "warning",
];

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct Theme {
    pub bg: String,
    pub fg: String,
    pub muted: String,
    pub border: String,
    pub selected: String,
    pub different: String,
    pub one_sided: String,
    pub minor: String,
    pub logo_lazy: String,
    pub logo_merge: String,
    pub added: String,
    pub removed: String,
    pub changed: String,
    pub accent: String,
    pub warning: String,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            bg: "#1F1F28".into(),
            fg: "#DCD7BA".into(),
            muted: "#727169".into(),
            border: "#54546D".into(),
            selected: "#223249".into(),
            different: "#E46876".into(),
            one_sided: "#957FB8".into(),
            minor: "#7FB4CA".into(),
            logo_lazy: "#957FB8".into(),
            logo_merge: "#98BB6C".into(),
            added: "#2B3328".into(),
            removed: "#43242B".into(),
            changed: "#49443C".into(),
            accent: "#E6C384".into(),
            warning: "#FFA066".into(),
        }
    }
}

impl Theme {
    fn value(&self, name: &str) -> &str {
        match name {
            "bg" => &self.bg,
            "fg" => &self.fg,
            "muted" => &self.muted,
            "border" => &self.border,
            "selected" => &self.selected,
            "different" => &self.different,
            "one_sided" => &self.one_sided,
            "minor" => &self.minor,
            "logo_lazy" => &self.logo_lazy,
            "logo_merge" => &self.logo_merge,
            "added" => &self.added,
            "removed" => &self.removed,
            "changed" => &self.changed,
            "accent" => &self.accent,
            "warning" => &self.warning,
            _ => &self.fg,
        }
    }

    pub fn color(&self, name: &str) -> Color {
        parse_color(self.value(name)).unwrap_or(Color::Reset)
    }
}

pub fn parse_color(value: &str) -> Result<Color> {
    let Some(hex) = value.strip_prefix('#') else {
        bail!("Expected a #RRGGBB color, got {value:?}");
    };
    if hex.len() != 6 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("Expected a #RRGGBB color, got {value:?}");
    }
    let value = u32::from_str_radix(hex, 16)?;
    Ok(Color::Rgb(
        (value >> 16) as u8,
        (value >> 8) as u8,
        value as u8,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_lua_tables_keep_defaults_and_execute_lua() {
        let config = Config::from_lua("local percent = 20; return { tree_percent = percent + 5, colors = { bg = '#000000' }, keys = { quit = 'ctrl+q' } }", "test.lua").unwrap();
        assert_eq!(config.tree_percent, 25);
        assert_eq!(config.colors.bg, "#000000");
        assert_eq!(config.colors.logo_lazy, Theme::default().logo_lazy);
        assert_eq!(
            config.action(&KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL)),
            Some("quit")
        );
        assert_eq!(
            config.action(&KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE)),
            Some("scroll_down")
        );
    }

    #[test]
    fn invalid_values_fail_with_actionable_errors() {
        for source in [
            "return { tree_percent = 101 }",
            "return { colors = { bg = 'red' } }",
            "return { keys = { quite = 'q' } }",
            "return { keys = { quit = 'j' } }",
            "return { typo = true }",
            "return 3",
        ] {
            assert!(Config::from_lua(source, "test.lua").is_err(), "{source}");
        }
    }

    #[test]
    fn uppercase_binding_handles_shift_reporting() {
        let config = Config::default();
        config.validate().unwrap();
        for modifiers in [KeyModifiers::NONE, KeyModifiers::SHIFT] {
            assert_eq!(
                config.action(&KeyEvent::new(KeyCode::Char('F'), modifiers)),
                Some("exclude")
            );
        }
        assert_eq!(
            config.action(&KeyEvent::new(KeyCode::Char('s'), KeyModifiers::ALT)),
            Some("save_both")
        );
        assert_eq!(
            config.action(&KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT)),
            Some("accept_edit")
        );
        assert_eq!(
            parse_binding("shift+tab").unwrap(),
            (KeyCode::BackTab, KeyModifiers::SHIFT)
        );
    }

    #[test]
    fn example_is_valid() {
        assert_eq!(
            Config::from_lua(DEFAULT_CONFIG, "config.example.lua").unwrap(),
            Config::default()
        );
    }

    #[test]
    fn missing_default_is_created_and_existing_settings_are_preserved() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("nested/lazymerge/config.lua");
        assert_eq!(Config::load_path(&path, true).unwrap(), Config::default());
        assert_eq!(fs::read_to_string(&path).unwrap(), DEFAULT_CONFIG);
        let custom = "return { show_logo = false, jump_after_merge = false }\n";
        fs::write(&path, custom).unwrap();
        let config = Config::load_path(&path, true).unwrap();
        assert!(!config.show_logo);
        assert!(!config.jump_after_merge);
        assert_eq!(fs::read_to_string(&path).unwrap(), custom);
        // Another process creating a config during startup must not lose its settings.
        write_defaults(&path, false).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), custom);
    }

    #[test]
    fn broken_config_is_preserved_until_reset_and_missing_custom_paths_are_not_created() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("custom/config.lua");
        assert!(Config::load(Some(&path)).is_err());
        assert!(!path.exists());
        assert_eq!(Config::reset(Some(&path)).unwrap(), path);
        assert_eq!(fs::read_to_string(&path).unwrap(), DEFAULT_CONFIG);
        fs::write(&path, "not valid Lua!!!").unwrap();
        let error = Config::load_path(&path, true).unwrap_err();
        assert!(error.to_string().contains("--reset-config"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "not valid Lua!!!");
        // Reset must work even when the old file is not valid text or Lua.
        fs::write(&path, [0xff, 0xfe]).unwrap();
        Config::reset(Some(&path)).unwrap();
        assert_eq!(Config::load(Some(&path)).unwrap(), Config::default());
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn platform_config_paths_use_xdg_local_app_data_and_native_home_directories() {
        for (platform, expected) in [
            ("linux", "home/.config/lazymerge/config.lua"),
            ("windows", "local/lazymerge/config.lua"),
            (
                "macos",
                "home/Library/Application Support/lazymerge/config.lua",
            ),
        ] {
            assert_eq!(
                default_path_for(platform, None, Some("local".into()), Some("home".into())),
                Some(PathBuf::from(expected))
            );
            assert_eq!(
                default_path_for(platform, Some("xdg".into()), Some("local".into()), None),
                Some(PathBuf::from("xdg/lazymerge/config.lua"))
            );
        }
        assert_eq!(
            default_path_for(
                "windows",
                Some("".into()),
                Some("".into()),
                Some("home".into())
            ),
            Some(PathBuf::from("home/AppData/Local/lazymerge/config.lua"))
        );
        assert_eq!(default_path_for("linux", None, None, None), None);
    }

    #[test]
    fn navigation_names_support_custom_bindings_and_legacy_configs() {
        for source in [
            "return { keys = { scroll_down = '2', scroll_up = '8', scroll_left = '4', scroll_right = '6' } }",
            "return { keys = { down = '2', up = '8', collapse = '4', expand = '6' } }",
        ] {
            let config = Config::from_lua(source, "navigation.lua").unwrap();
            for (character, action) in [
                ('2', "scroll_down"),
                ('8', "scroll_up"),
                ('4', "scroll_left"),
                ('6', "scroll_right"),
            ] {
                assert_eq!(
                    config.action(&KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE)),
                    Some(action)
                );
            }
        }
        assert!(
            Config::from_lua(
                "return { keys = { down = 'j', scroll_down = '2' } }",
                "ambiguous.lua"
            )
            .is_err()
        );
    }

    #[test]
    fn two_character_sequences_resolve_per_view_and_reject_conflicts() {
        let config = Config::default();
        assert_eq!(config.sequence('g', Some('g'), true), Some("top"));
        assert_eq!(config.sequence('g', Some('g'), false), Some("top"));
        assert_eq!(config.sequence('d', Some('d'), false), Some("delete_both"));
        assert_eq!(config.sequence('d', Some('h'), false), Some("delete_left"));
        assert_eq!(config.sequence('d', Some('l'), false), Some("delete_right"));
        assert!(config.sequence('d', None, false).is_some());
        // In the tree, d still shows only the differences.
        assert_eq!(config.sequence('d', None, true), None);
        assert_eq!(config.sequence('d', Some('d'), true), None);
        // zz centers the cursor row in a file and the selected row in the tree.
        assert_eq!(config.sequence('z', Some('z'), false), Some("center"));
        assert_eq!(config.sequence('z', Some('z'), true), Some("center"));
        assert_eq!(
            config.action(&KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE)),
            Some("differences")
        );
        assert_eq!(
            config.action(&KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE)),
            None
        );
        for (source, top) in [
            ("return { keys = { top = 'g' } }", "gg"),
            ("return { keys = { top = 'home' } }", "home"),
            ("return { keys = { top = 'f2' } }", "f2"),
        ] {
            assert_eq!(
                Config::from_lua(source, "top.lua").unwrap().keys["top"],
                top
            );
        }
        for source in [
            "return { keys = { undo = 'd' } }",
            "return { keys = { differences = 'g' } }",
            "return { keys = { delete_left = 'dl' } }",
            "return { keys = { delete_both = 'dh' } }",
            "return { keys = { undo = 'z' } }",
            "return { keys = { accept_edit = 'xx' } }",
        ] {
            assert!(
                Config::from_lua(source, "conflict.lua").is_err(),
                "{source}"
            );
        }
        assert!(
            Config::from_lua(
                "return { keys = { differences = '', undo = 'd' } }",
                "x.lua"
            )
            .is_err()
        );
        Config::from_lua(
            "return { keys = { delete_both = '', delete_left = '', delete_right = '', undo = 'd', differences = 'D' } }",
            "free.lua",
        )
        .unwrap();
    }

    #[test]
    fn old_save_defaults_migrate_without_a_duplicate_binding_error() {
        for source in [
            "return { keys = { save_right = 'alt+s' } }",
            "return { keys = { save_right = 'alt+s', save_both = 'alt+S' } }",
        ] {
            let config = Config::from_lua(source, "legacy.lua").unwrap();
            assert_eq!(config.keys["save_right"], "ctrl+alt+s");
            assert_eq!(config.keys["save_both"], "alt+s");
        }
    }
}
