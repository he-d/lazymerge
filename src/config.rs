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
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub show_logo: bool,
    pub differences_only: bool,
    pub ignore_whitespace: bool,
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
        ("down", "j"),
        ("up", "k"),
        ("expand", "l"),
        ("collapse", "h"),
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
        ("undo", "u"),
        ("save_left", "ctrl+s"),
        ("save_right", "ctrl+alt+s"),
        ("save_both", "alt+s"),
        ("accept_edit", "alt+enter"),
        ("next", "n"),
        ("previous", "N"),
        ("select", "v"),
        ("top", "g"),
        ("bottom", "G"),
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
        let path = explicit.map(Path::to_path_buf).or_else(default_path);
        let Some(path) = path else {
            return Ok(Self::default());
        };
        let source = match fs::read_to_string(&path) {
            Ok(source) => source,
            Err(error) if explicit.is_none() && error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => {
                return Err(error).with_context(|| format!("Reading config {}", path.display()));
            }
        };
        Self::from_lua(&source, &path.to_string_lossy())
            .with_context(|| format!("Invalid config {}", path.display()))
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
        let defaults = default_keys();
        for action in config.keys.keys() {
            if !defaults.contains_key(action) {
                bail!("Unknown key action: {action}");
            }
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
        let defaults = default_keys();
        for (action, key) in &self.keys {
            if !defaults.contains_key(action) {
                bail!("Unknown key action: {action}");
            }
            if key.is_empty() {
                continue;
            }
            let binding = parse_binding(key).with_context(|| format!("keys.{action}"))?;
            if let Some(previous) = bindings.insert(binding, action) {
                bail!(
                    "Key {key:?} is assigned to both {previous} and {action}; set one binding to \"\" to disable it"
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
}

pub fn default_path() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            #[cfg(windows)]
            if let Some(app_data) = std::env::var_os("APPDATA").filter(|value| !value.is_empty()) {
                return Some(PathBuf::from(app_data));
            }
            crate::input::home_dir().map(|home| home.join(".config"))
        })
        .map(|base| base.join("lazymerge/config.lua"))
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

#[derive(Clone, Debug, Deserialize)]
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
            Some("down")
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
        Config::from_lua(include_str!("../config.example.lua"), "config.example.lua").unwrap();
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
