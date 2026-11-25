use anyhow::{Context, Result, bail};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::{ffi::OsString, fs, path::PathBuf};

fn is_path_separator(character: char, windows: bool) -> bool {
    character == '/' || (windows && character == '\\')
}

fn completion_prefix(input: &str, windows: bool) -> (String, String) {
    if input == "~" {
        (if windows { "~\\" } else { "~/" }.into(), String::new())
    } else if let Some(separator) = input.rfind(|character| is_path_separator(character, windows)) {
        (input[..separator + 1].into(), input[separator + 1..].into())
    } else if windows
        && input.len() == 2
        && input.as_bytes()[0].is_ascii_alphabetic()
        && input.as_bytes()[1] == b':'
    {
        (format!("{input}\\"), String::new())
    } else {
        (String::new(), input.into())
    }
}

fn home_relative(value: &str, windows: bool) -> Option<&str> {
    if value == "~" {
        Some("")
    } else if value.starts_with("~/") || (windows && value.starts_with("~\\")) {
        Some(&value[2..])
    } else {
        None
    }
}

fn home_from_env(
    windows: bool,
    mut variable: impl FnMut(&str) -> Option<OsString>,
) -> Option<PathBuf> {
    if let Some(home) = variable("HOME").filter(|home| !home.is_empty()) {
        return Some(home.into());
    }
    if !windows {
        return None;
    }
    if let Some(profile) = variable("USERPROFILE").filter(|profile| !profile.is_empty()) {
        return Some(profile.into());
    }
    let mut drive = variable("HOMEDRIVE").filter(|drive| !drive.is_empty())?;
    drive.push(variable("HOMEPATH").filter(|path| !path.is_empty())?);
    Some(drive.into())
}

pub(crate) fn home_dir() -> Option<PathBuf> {
    home_from_env(cfg!(windows), |name| std::env::var_os(name))
}

#[derive(Clone, Debug, Default)]
pub struct Input {
    pub value: String,
    /// Byte offset, always on a UTF-8 character boundary.
    pub cursor: usize,
    pub completions: Vec<String>,
    pub completion_index: Option<usize>,
    completion_value: Option<String>,
    completion_cursor: Option<usize>,
    completion_cursors: Vec<usize>,
}

impl Input {
    pub fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        Self {
            cursor: value.len(),
            value,
            ..Self::default()
        }
    }

    fn reset_completion(&mut self) {
        self.completions.clear();
        self.completion_index = None;
        self.completion_value = None;
        self.completion_cursor = None;
        self.completion_cursors.clear();
    }

    pub fn insert(&mut self, text: &str) {
        self.reset_completion();
        // A pasted path or search is one line, even when copied with a trailing newline.
        let text: String = text
            .chars()
            .filter(|character| !character.is_control())
            .collect();
        self.value.insert_str(self.cursor, &text);
        self.cursor += text.len();
    }

    /// Edit a single-line field. Returns whether its text changed.
    pub fn handle(&mut self, event: KeyEvent) -> bool {
        let old = self.value.clone();
        self.reset_completion();
        let control = event.modifiers.contains(KeyModifiers::CONTROL);
        match event.code {
            KeyCode::Home | KeyCode::Char('a') if event.code == KeyCode::Home || control => {
                self.cursor = 0
            }
            KeyCode::End | KeyCode::Char('e') if event.code == KeyCode::End || control => {
                self.cursor = self.value.len()
            }
            KeyCode::Char('u') if control => {
                self.value.drain(..self.cursor);
                self.cursor = 0;
            }
            KeyCode::Char('w') if control => {
                let before = &self.value[..self.cursor];
                let trimmed = before.trim_end_matches(char::is_whitespace);
                let start = trimmed.rfind(char::is_whitespace).map_or(0, |offset| {
                    offset + trimmed[offset..].chars().next().unwrap().len_utf8()
                });
                self.value.drain(start..self.cursor);
                self.cursor = start;
            }
            KeyCode::Left => {
                self.cursor = self.value[..self.cursor]
                    .char_indices()
                    .next_back()
                    .map_or(0, |(offset, _)| offset);
            }
            KeyCode::Right => {
                if let Some(character) = self.value[self.cursor..].chars().next() {
                    self.cursor += character.len_utf8();
                }
            }
            KeyCode::Backspace if self.cursor > 0 => {
                let previous = self.value[..self.cursor]
                    .char_indices()
                    .next_back()
                    .unwrap()
                    .0;
                self.value.drain(previous..self.cursor);
                self.cursor = previous;
            }
            KeyCode::Delete if self.cursor < self.value.len() => {
                let next =
                    self.cursor + self.value[self.cursor..].chars().next().unwrap().len_utf8();
                self.value.drain(self.cursor..next);
            }
            KeyCode::Char(character)
                if !event.modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                ) && !character.is_control() =>
            {
                self.value.insert(self.cursor, character);
                self.cursor += character.len_utf8();
            }
            _ => {}
        }
        self.value != old
    }

    /// Complete the directory component under the cursor, with `files` also file
    /// names. Later components remain intact. Repeated Tab / Shift+Tab cycles
    /// candidates. Quotes are a field wrapper, not shell syntax; completing an
    /// unfinished wrapper closes it.
    pub fn complete(&mut self, backward: bool, files: bool) -> Result<()> {
        if self.completion_value.as_deref() == Some(&self.value)
            && self.completion_cursor == Some(self.cursor)
            && !self.completions.is_empty()
        {
            let length = self.completions.len();
            let index = self.completion_index.unwrap_or(0);
            self.completion_index = Some(if backward {
                (index + length - 1) % length
            } else {
                (index + 1) % length
            });
        } else {
            self.reset_completion();
            let quote = self
                .value
                .chars()
                .next()
                .filter(|character| matches!(character, '\'' | '"'));
            let body_start = usize::from(quote.is_some());
            let body_end =
                if quote.is_some_and(|quote| self.value.len() > 1 && self.value.ends_with(quote)) {
                    self.value.len() - 1
                } else {
                    self.value.len()
                };
            let body = &self.value[body_start..body_end];
            let cursor = self.cursor.clamp(body_start, body_end) - body_start;
            let input = &body[..cursor];
            let (prefix, fragment) = completion_prefix(input, cfg!(windows));
            let directory = if prefix.is_empty() {
                PathBuf::from(".")
            } else {
                expand_path(&prefix)?
            };
            let entries = fs::read_dir(&directory)
                .with_context(|| format!("Cannot complete paths in {}", directory.display()))?;
            // Replace the remainder of the current component, preserving any
            // components after it rather than appending to the partial name.
            let suffix_start = body[cursor..]
                .find(|character| is_path_separator(character, cfg!(windows)))
                .map_or(body.len(), |offset| cursor + offset);
            let suffix = &body[suffix_start..];
            let separator = prefix
                .chars()
                .next_back()
                .filter(|character| is_path_separator(*character, cfg!(windows)))
                .unwrap_or(std::path::MAIN_SEPARATOR);
            let suffix = suffix
                .strip_prefix(|character| is_path_separator(character, cfg!(windows)))
                .unwrap_or(suffix);
            let mut candidates = Vec::new();
            let folded_fragment = cfg!(windows).then(|| fragment.to_lowercase());
            for entry in entries {
                let entry = entry?;
                // Follow directory symlinks for path entry; the chosen root is canonicalized later.
                // A file name ends the path: offer files only without later components.
                let directory = entry.path().is_dir();
                if !directory && !(files && suffix.is_empty() && entry.path().is_file()) {
                    continue;
                }
                let name = entry.file_name();
                let Some(name) = name.to_str() else { continue };
                let matches = folded_fragment.as_ref().map_or_else(
                    || name.starts_with(&fragment),
                    |fragment| name.to_lowercase().starts_with(fragment),
                );
                if matches && (fragment.starts_with('.') || !name.starts_with('.')) {
                    let (completed, cursor) = if directory {
                        (
                            format!("{prefix}{name}{separator}{suffix}"),
                            body_start + prefix.len() + name.len() + separator.len_utf8(),
                        )
                    } else {
                        (
                            format!("{prefix}{name}"),
                            body_start + prefix.len() + name.len(),
                        )
                    };
                    let completed = match quote {
                        Some(quote) => format!("{quote}{completed}{quote}"),
                        None => completed,
                    };
                    candidates.push((completed, cursor));
                }
            }
            candidates.sort_by(|left, right| left.0.cmp(&right.0));
            (self.completions, self.completion_cursors) = candidates.into_iter().unzip();
            if self.completions.is_empty() {
                return Ok(());
            }
            self.completion_index = Some(if backward {
                self.completions.len() - 1
            } else {
                0
            });
        }
        if let Some(index) = self.completion_index {
            self.value = self.completions[index].clone();
            self.cursor = self.completion_cursors[index];
            self.completion_value = Some(self.value.clone());
            self.completion_cursor = Some(self.cursor);
        }
        Ok(())
    }
}

pub fn expand_path(value: &str) -> Result<PathBuf> {
    let value = if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        &value[1..value.len() - 1]
    } else {
        value
    };
    if value.is_empty() {
        bail!("Enter a folder path");
    }
    if let Some(relative) = home_relative(value, cfg!(windows)) {
        let mut path = home_dir().context("Cannot expand ~ because no home folder is set")?;
        if !relative.is_empty() {
            path.push(relative);
        }
        return Ok(path);
    }
    if value.starts_with('~') {
        bail!("Use ~ or ~/ for your home folder; ~user paths are not supported");
    }
    Ok(PathBuf::from(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn edits_unicode_without_splitting_characters() {
        let mut input = Input::new("a界é");
        input.handle(key(KeyCode::Left));
        input.handle(key(KeyCode::Backspace));
        assert_eq!(input.value, "aé");
        assert_eq!(input.cursor, 1);
        input.insert("ö\n");
        assert_eq!(input.value, "aöé");
        input.handle(key(KeyCode::Delete));
        assert_eq!(input.value, "aö");
    }

    #[test]
    fn completion_cycles_directories_with_spaces_and_descends() {
        let directory = tempfile::tempdir().unwrap();
        for name in ["my alpha", "my beta", "my alpha/child"] {
            fs::create_dir_all(directory.path().join(name)).unwrap();
        }
        fs::write(directory.path().join("my file"), "").unwrap();
        let mut input = Input::new(format!("{}/my ", directory.path().display()));
        input.complete(false, false).unwrap();
        assert!(input.value.ends_with("/my alpha/"));
        input.complete(false, false).unwrap();
        assert!(input.value.ends_with("/my beta/"));
        input.complete(true, false).unwrap();
        assert!(input.value.ends_with("/my alpha/"));
        input.handle(key(KeyCode::Char('c')));
        input.complete(false, false).unwrap();
        assert!(input.value.ends_with("/my alpha/child/"));
    }

    #[test]
    fn file_completion_offers_files_without_a_trailing_separator() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("my folder")).unwrap();
        fs::write(directory.path().join("my file.txt"), "").unwrap();
        let mut input = Input::new(format!("{}/my ", directory.path().display()));
        input.complete(false, true).unwrap();
        assert!(input.value.ends_with("/my file.txt"));
        assert_eq!(input.cursor, input.value.len());
        input.complete(false, true).unwrap();
        assert!(input.value.ends_with("/my folder/"));
        // A file cannot contain the components after the cursor.
        let mut input = Input::new(format!("{}/my /rest", directory.path().display()));
        input.cursor = format!("{}/my ", directory.path().display()).len();
        input.complete(false, true).unwrap();
        assert!(input.value.ends_with("/my folder/rest"));
        input.complete(false, true).unwrap();
        assert!(
            input.value.ends_with("/my folder/rest"),
            "no other candidate"
        );
    }

    #[test]
    fn completes_closed_and_unfinished_quotes_and_keeps_cursor_inside() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir_all(directory.path().join("my folder/child")).unwrap();
        for quote in ['\'', '"'] {
            for closing in ["", &quote.to_string()] {
                let mut input = Input::new(format!(
                    "{quote}{}/my f{closing}",
                    directory.path().display()
                ));
                input.complete(false, false).unwrap();
                assert_eq!(
                    input.value,
                    format!("{quote}{}/my folder/{quote}", directory.path().display())
                );
                assert_eq!(input.cursor, input.value.len() - 1);
                assert_eq!(
                    expand_path(&input.value).unwrap(),
                    directory.path().join("my folder")
                );
                input.handle(key(KeyCode::Char('c')));
                input.complete(false, false).unwrap();
                assert!(input.value.ends_with(&format!("/my folder/child/{quote}")));
                assert_eq!(input.cursor, input.value.len() - 1);
            }
        }
    }

    #[test]
    fn completion_replaces_component_at_cursor_preserving_later_path() {
        let directory = tempfile::tempdir().unwrap();
        for name in ["界 alpha/child", "界 beta/child"] {
            fs::create_dir_all(directory.path().join(name)).unwrap();
        }
        let mut input = Input::new(format!("\"{}/界 wrong/child\"", directory.path().display()));
        input.cursor = format!("\"{}/界 ", directory.path().display()).len();
        input.complete(false, false).unwrap();
        assert_eq!(
            input.value,
            format!("\"{}/界 alpha/child\"", directory.path().display())
        );
        assert_eq!(&input.value[input.cursor..], "child\"");
        input.complete(false, false).unwrap();
        assert_eq!(
            input.value,
            format!("\"{}/界 beta/child\"", directory.path().display())
        );
        assert_eq!(&input.value[input.cursor..], "child\"");
        input.complete(true, false).unwrap();
        assert!(input.value.contains("界 alpha"));
        // Moving the cursor ends cycling and completes the next component.
        input.handle(key(KeyCode::Right));
        input.complete(false, false).unwrap();
        assert_eq!(
            input.value,
            format!("\"{}/界 alpha/child/\"", directory.path().display())
        );
    }

    #[test]
    fn preserves_spaces_and_expands_quoted_paths() {
        assert_eq!(
            expand_path("  folder  ").unwrap(),
            PathBuf::from("  folder  ")
        );
        assert_eq!(
            expand_path("'my folder'").unwrap(),
            PathBuf::from("my folder")
        );
        assert!(expand_path("").is_err());
    }

    #[test]
    fn windows_completion_parses_drive_roots_and_both_separator_styles() {
        for (input, prefix, fragment) in [
            ("C:", "C:\\", ""),
            ("C:\\", "C:\\", ""),
            ("C:/", "C:/", ""),
            ("C:\\Users\\my f", "C:\\Users\\", "my f"),
            ("C:/Users/my f", "C:/Users/", "my f"),
            ("C:\\Users/界 a", "C:\\Users/", "界 a"),
            ("\\\\server\\share\\my f", "\\\\server\\share\\", "my f"),
            ("~\\my f", "~\\", "my f"),
            ("~/my f", "~/", "my f"),
        ] {
            assert_eq!(
                completion_prefix(input, true),
                (prefix.into(), fragment.into()),
                "{input}"
            );
        }
        assert_eq!(completion_prefix("~", true), ("~\\".into(), String::new()));
        assert_eq!(completion_prefix("~", false), ("~/".into(), String::new()));
        assert_eq!(
            completion_prefix("folder\\name", false),
            (String::new(), "folder\\name".into())
        );
        assert_eq!(
            completion_prefix("outer/folder\\name", false),
            ("outer/".into(), "folder\\name".into())
        );
    }

    #[test]
    fn home_expansion_uses_windows_profile_fallbacks_without_changing_unix_paths() {
        fn resolve(windows: bool, values: &[(&str, &str)]) -> Option<PathBuf> {
            home_from_env(windows, |name| {
                values
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| OsString::from(value))
            })
        }
        assert_eq!(
            resolve(
                true,
                &[("HOME", "preferred home"), ("USERPROFILE", "other home")]
            ),
            Some(PathBuf::from("preferred home"))
        );
        assert_eq!(
            resolve(true, &[("HOME", ""), ("USERPROFILE", r"C:\Users\My User")]),
            Some(PathBuf::from(r"C:\Users\My User"))
        );
        assert_eq!(
            resolve(
                true,
                &[("HOMEDRIVE", "D:"), ("HOMEPATH", r"\Users\My User")]
            ),
            Some(PathBuf::from(r"D:\Users\My User"))
        );
        assert_eq!(resolve(false, &[("USERPROFILE", "windows only")]), None);
        assert_eq!(resolve(true, &[("HOMEDRIVE", "D:")]), None);
        assert_eq!(home_relative(r"~\Projects", true), Some("Projects"));
        assert_eq!(home_relative(r"~\Projects", false), None);
        assert_eq!(home_relative("~/Projects", true), Some("Projects"));
        assert_eq!(home_relative("~other", true), None);
    }

    #[cfg(unix)]
    #[test]
    fn unix_completion_keeps_backslashes_as_literal_filename_characters() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join(r"folder\name")).unwrap();
        let mut input = Input::new(format!("{}/folder\\na", directory.path().display()));
        input.complete(false, false).unwrap();
        assert!(input.value.ends_with("/folder\\name/"));
        assert!(expand_path(&input.value).unwrap().is_dir());
    }

    #[cfg(windows)]
    #[test]
    fn windows_completion_matches_native_filename_case() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("CaseFolder")).unwrap();
        let mut input = Input::new(format!("{}/casef", directory.path().display()));
        input.complete(false, false).unwrap();
        assert!(input.value.ends_with("/CaseFolder/"));
        assert!(expand_path(&input.value).unwrap().is_dir());
    }
}
