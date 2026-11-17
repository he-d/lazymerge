use crate::{
    config::Config,
    input::{Input, expand_path},
    model::Session,
    mouse::MouseState,
    storage::{self, CopyPlan, Node, Roots},
    ui,
};
use anyhow::{Result, bail};
use ratatui::{
    DefaultTerminal,
    crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
};
use std::{
    collections::HashSet,
    ops::Range,
    path::PathBuf,
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Focus {
    Tree,
    Diff,
}

#[derive(Clone, Copy, Debug)]
pub enum PromptKind {
    Path(usize),
    FindTree,
    FindDiff,
    Include,
    Exclude,
}

pub struct Prompt {
    pub kind: PromptKind,
    pub input: Input,
    pub error: String,
}

pub enum Action {
    Quit,
    Close,
    Reload,
    Rescan,
    Paths,
    Copy(CopyPlan),
    Overwrite(usize),
    OverwriteBoth(usize),
}

pub struct Confirmation {
    pub message: String,
    pub action: Action,
    pub yes: bool,
}

pub struct Editor {
    pub side: usize,
    pub range: Range<usize>,
    pub lines: Vec<Input>,
    pub row: usize,
    pub scroll: usize,
}

impl Editor {
    fn new(side: usize, range: Range<usize>, text: &str) -> Self {
        Self {
            side,
            range,
            lines: text.split('\n').map(|s| Input::new(s.to_owned())).collect(),
            row: 0,
            scroll: 0,
        }
    }

    fn text(&self) -> String {
        self.lines
            .iter()
            .map(|l| l.value.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn key(&mut self, key: KeyEvent, tab_width: usize) {
        match key.code {
            KeyCode::Up => self.row = self.row.saturating_sub(1),
            KeyCode::Down => self.row = (self.row + 1).min(self.lines.len() - 1),
            KeyCode::Enter => {
                let line = &mut self.lines[self.row];
                let tail = line.value.split_off(line.cursor);
                self.row += 1;
                self.lines.insert(self.row, Input::new(tail));
                self.lines[self.row].cursor = 0;
            }
            KeyCode::Backspace if self.lines[self.row].cursor == 0 && self.row > 0 => {
                let tail = self.lines.remove(self.row).value;
                self.row -= 1;
                let line = &mut self.lines[self.row];
                line.cursor = line.value.len();
                line.value.push_str(&tail);
            }
            KeyCode::Delete
                if self.lines[self.row].cursor == self.lines[self.row].value.len()
                    && self.row + 1 < self.lines.len() =>
            {
                let tail = self.lines.remove(self.row + 1).value;
                self.lines[self.row].value.push_str(&tail);
            }
            KeyCode::Tab => self.lines[self.row].insert(&" ".repeat(tab_width)),
            _ => {
                self.lines[self.row].handle(key);
            }
        }
    }

    fn paste(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let mut parts = text.split('\n');
        self.insert_text(parts.next().unwrap_or_default());
        for part in parts {
            self.key(KeyCode::Enter.into(), 4);
            self.insert_text(part);
        }
    }

    fn insert_text(&mut self, text: &str) {
        // Single-line prompts discard control characters. Source-code editing must
        // retain literal tabs and the other bytes represented by the UTF-8 text.
        let line = &mut self.lines[self.row];
        line.value.insert_str(line.cursor, text);
        line.cursor += text.len();
    }
}

pub struct App {
    pub config: Config,
    pub roots: Option<Roots>,
    pub paths: [Option<PathBuf>; 2],
    pub nodes: Vec<Node>,
    pub expanded: HashSet<PathBuf>,
    pub selected: usize,
    pub tree_scroll: usize,
    pub diff_scroll: usize,
    pub tree_height: usize,
    pub diff_height: usize,
    pub focus: Focus,
    pub session: Option<Session>,
    pub prompt: Option<Prompt>,
    pub confirmation: Option<Confirmation>,
    pub editor: Option<Editor>,
    pub help: bool,
    pub help_scroll: u16,
    pub tree_query: String,
    pub diff_query: String,
    pub message: String,
    pub file_change_notice: Option<String>,
    pub scanning: bool,
    pub mouse_state: MouseState,
    scan_rx: Option<Receiver<Result<Vec<Node>>>>,
    rescan_requested: bool,
    scan_notice: Option<String>,
    scan_announce: bool,
    last_poll: Instant,
    exit: bool,
    pending_g: bool,
}

impl App {
    pub fn new(config: Config, left: Option<PathBuf>, right: Option<PathBuf>) -> Result<Self> {
        let paths = [left, right]
            .map(|p| p.map(|p| expand_path(&p.to_string_lossy())).transpose())
            .into_iter()
            .collect::<Result<Vec<_>>>()?;
        for (side, path) in paths.iter().enumerate() {
            if let Some(path) = path
                && !path.is_dir()
            {
                bail!(
                    "{} folder is not an existing directory: {}",
                    if side == 0 { "Left" } else { "Right" },
                    path.display()
                );
            }
        }
        let mut app = Self {
            config,
            roots: None,
            paths: [paths[0].clone(), paths[1].clone()],
            nodes: vec![],
            expanded: HashSet::new(),
            selected: 0,
            tree_scroll: 0,
            diff_scroll: 0,
            tree_height: 10,
            diff_height: 10,
            focus: Focus::Tree,
            session: None,
            prompt: None,
            confirmation: None,
            editor: None,
            help: false,
            help_scroll: 0,
            tree_query: String::new(),
            diff_query: String::new(),
            message: String::new(),
            file_change_notice: None,
            scanning: false,
            mouse_state: MouseState::default(),
            scan_rx: None,
            rescan_requested: false,
            scan_notice: None,
            scan_announce: false,
            last_poll: Instant::now(),
            exit: false,
            pending_g: false,
        };
        if app.paths.iter().all(Option::is_some) {
            app.set_roots()?;
        } else {
            app.path_prompt(if app.paths[0].is_none() { 0 } else { 1 });
        }
        Ok(app)
    }

    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        use ratatui::crossterm::{
            event::{
                DisableBracketedPaste, DisableFocusChange, DisableMouseCapture,
                EnableBracketedPaste, EnableFocusChange, EnableMouseCapture,
            },
            execute,
        };
        struct PasteMode;
        impl Drop for PasteMode {
            fn drop(&mut self) {
                let _ = execute!(
                    std::io::stdout(),
                    DisableMouseCapture,
                    DisableFocusChange,
                    DisableBracketedPaste
                );
            }
        }
        execute!(
            std::io::stdout(),
            EnableBracketedPaste,
            EnableFocusChange,
            EnableMouseCapture
        )?;
        let _paste_mode = PasteMode;
        self.event_loop(terminal)
    }

    fn event_loop(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        let mut redraw = true;
        while !self.exit {
            redraw |= self.receive_scan();
            if self.last_poll.elapsed() >= Duration::from_secs(self.config.poll_seconds) {
                redraw |= self.check_disk_changes();
            }
            if redraw {
                terminal.draw(|frame| ui::draw(frame, self))?;
                redraw = false;
            }
            if event::poll(Duration::from_millis(100))? {
                let event = event::read()?;
                if matches!(&event, Event::Mouse(mouse) if mouse.kind == ratatui::crossterm::event::MouseEventKind::Moved)
                {
                    continue;
                }
                redraw = true;
                let result = match event {
                    Event::Key(key) if key.kind != KeyEventKind::Release => self.key(key),
                    Event::Mouse(mouse) => self.mouse(mouse),
                    Event::FocusGained => {
                        self.check_disk_changes();
                        Ok(())
                    }
                    Event::Paste(text) => {
                        self.paste(&text);
                        Ok(())
                    }
                    _ => Ok(()),
                };
                if let Err(error) = result {
                    self.message = format!("{error:#}");
                }
            }
        }
        Ok(())
    }

    /// Poll content, rather than relying on mtime/size: editors may replace a
    /// file atomically or preserve its timestamp. Staged buffers stay untouched.
    pub(crate) fn check_disk_changes(&mut self) -> bool {
        self.last_poll = Instant::now();
        let mut changed = false;
        if let (Some(roots), Some(session)) = (&self.roots, &mut self.session) {
            let stale = roots.disk_changed(session);
            changed = stale != session.stale;
            session.stale = stale;
            if stale.into_iter().any(|side| side) {
                let can_reload = !session.dirty()
                    && self.editor.is_none()
                    && self.confirmation.is_none()
                    && self.prompt.is_none();
                if can_reload {
                    match roots.open(&session.path, self.config.ignore_whitespace) {
                        Ok(mut fresh) => {
                            fresh.cursor =
                                session.cursor.min(fresh.diff.rows.len().saturating_sub(1));
                            fresh.horizontal = session.horizontal;
                            self.session = Some(fresh);
                            self.message = "File changed on disk · reloaded both sides".into();
                            self.file_change_notice = Some(self.message.clone());
                            changed = true;
                        }
                        Err(error) if changed => {
                            self.message = format!("File changed on disk: {error:#}");
                            self.file_change_notice = Some(self.message.clone());
                        }
                        Err(_) => {}
                    }
                } else if changed {
                    self.message = "File changed on disk · staged edits kept · r reloads; saving asks before overwrite".into();
                    self.file_change_notice = Some(self.message.clone());
                }
            } else if changed {
                self.message = "Files on disk match the loaded versions again".into();
                self.file_change_notice = None;
            }
        }
        // Folder changes, including new/deleted files, also need discovery while
        // no file is open. Keep recurring scans silent and never queue a backlog.
        if !self.scanning && self.roots.is_some() {
            self.start_scan(false);
        }
        changed
    }

    fn set_roots(&mut self) -> Result<()> {
        let left = self.paths[0].as_ref().unwrap();
        let right = self.paths[1].as_ref().unwrap();
        self.roots = Some(Roots::new(left, right)?);
        self.session = None;
        self.file_change_notice = None;
        self.focus = Focus::Tree;
        self.nodes.clear();
        self.expanded.clear();
        self.tree_query.clear();
        self.selected = 0;
        self.scan_rx = None;
        self.scanning = false;
        self.scan_notice = None;
        self.rescan_requested = false;
        self.last_poll = Instant::now();
        self.scan();
        Ok(())
    }

    pub fn scan(&mut self) {
        self.start_scan(true);
    }

    fn start_scan(&mut self, announce: bool) {
        if self.scanning {
            if announce {
                self.rescan_requested = true;
            }
            return;
        }
        let Some(roots) = self.roots.clone() else {
            return;
        };
        let include = self.config.include.clone();
        let exclude = self.config.exclude.clone();
        let ignore = self.config.ignore_whitespace;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(storage::scan(&roots, &include, &exclude, ignore));
        });
        self.scan_rx = Some(rx);
        self.scanning = true;
        self.scan_announce = announce;
        if announce {
            self.message = "Comparing folders…".into();
        }
    }

    pub fn receive_scan(&mut self) -> bool {
        let result = self.scan_rx.as_ref().map(|rx| rx.try_recv());
        let changed = matches!(
            &result,
            Some(Ok(_)) | Some(Err(mpsc::TryRecvError::Disconnected))
        );
        match result {
            Some(Ok(result)) => {
                self.scanning = false;
                self.scan_rx = None;
                match result {
                    Ok(nodes) => {
                        let first = self.nodes.is_empty();
                        let nodes_changed = self.nodes != nodes;
                        let selected_path = self.selected_node().map(|node| node.path.clone());
                        self.nodes = nodes;
                        if first {
                            self.expanded.extend(
                                self.nodes
                                    .iter()
                                    .filter(|n| n.is_dir && n.status.is_difference())
                                    .map(|n| n.path.clone()),
                            );
                        }
                        if let Some(path) = selected_path
                            && let Some(index) = self
                                .visible()
                                .iter()
                                .position(|(i, _)| self.nodes[*i].path == path)
                        {
                            self.selected = index;
                        }
                        self.clamp_selection();
                        let summary = format!(
                            "Compared {} files",
                            self.nodes.iter().filter(|n| !n.is_dir).count()
                        );
                        if self.scan_announce || self.scan_notice.is_some() {
                            self.message = if self.rescan_requested {
                                summary
                            } else {
                                self.scan_notice.take().map_or_else(
                                    || summary.clone(),
                                    |notice| format!("{notice} · {summary}"),
                                )
                            };
                        } else if nodes_changed && self.session.is_none() {
                            self.message = format!("Folders updated · {summary}");
                            self.file_change_notice = Some(self.message.clone());
                        }
                    }
                    Err(error) => self.message = format!("Compare failed: {error:#}"),
                }
            }
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.scanning = false;
                self.scan_rx = None;
                self.message = "Folder scan stopped unexpectedly. Press r to retry.".into();
            }
            _ => {}
        }
        if !self.scanning && self.rescan_requested {
            self.rescan_requested = false;
            self.scan();
            return true;
        }
        changed
    }

    pub fn visible(&self) -> Vec<(usize, usize)> {
        let query = self.tree_query.to_lowercase();
        self.nodes
            .iter()
            .enumerate()
            .filter_map(|(i, n)| {
                if self.config.differences_only && !n.status.is_difference() {
                    return None;
                }
                if !query.is_empty() {
                    let matches = |node: &Node| {
                        !node.is_dir
                            && node
                                .path
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .to_lowercase()
                                .contains(&query)
                            && (!self.config.differences_only || node.status.is_difference())
                    };
                    if n.is_dir {
                        if !self
                            .nodes
                            .iter()
                            .any(|child| child.path.starts_with(&n.path) && matches(child))
                        {
                            return None;
                        }
                    } else if !matches(n) {
                        return None;
                    }
                } else {
                    for parent in n
                        .path
                        .ancestors()
                        .skip(1)
                        .filter(|p| !p.as_os_str().is_empty())
                    {
                        if !self.expanded.contains(parent) {
                            return None;
                        }
                    }
                }
                Some((i, n.path.components().count().saturating_sub(1)))
            })
            .collect()
    }

    fn clamp_selection(&mut self) {
        self.selected = self.selected.min(self.visible().len().saturating_sub(1));
    }

    fn selected_node(&self) -> Option<&Node> {
        self.visible()
            .get(self.selected)
            .map(|(i, _)| &self.nodes[*i])
    }

    fn path_prompt(&mut self, side: usize) {
        let value = self.paths[side]
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.prompt = Some(Prompt {
            kind: PromptKind::Path(side),
            input: Input::new(value),
            error: String::new(),
        });
    }

    fn confirm(&mut self, action: Action, message: impl Into<String>) {
        self.confirmation = Some(Confirmation {
            action,
            message: message.into(),
            yes: false,
        });
    }

    fn guarded(&mut self, action: Action, message: &str) -> Result<()> {
        if self.session.as_ref().is_some_and(Session::dirty) {
            self.confirm(
                action,
                format!("{message}\nUnsaved edits will be discarded."),
            );
            Ok(())
        } else {
            self.perform(action)
        }
    }

    fn perform(&mut self, action: Action) -> Result<()> {
        match action {
            Action::Quit => self.exit = true,
            Action::Close => {
                self.session = None;
                self.file_change_notice = None;
                self.focus = Focus::Tree;
            }
            Action::Reload => {
                if self.session.is_none() {
                    self.file_change_notice = None;
                }
                if let Some(session) = &self.session {
                    let cursor = session.cursor;
                    let mut fresh = self
                        .roots
                        .as_ref()
                        .unwrap()
                        .open(&session.path, self.config.ignore_whitespace)?;
                    fresh.cursor = cursor.min(fresh.diff.rows.len().saturating_sub(1));
                    self.session = Some(fresh);
                    self.file_change_notice = None;
                    self.message = "Files reloaded from disk".into();
                }
            }
            Action::Rescan => {
                self.perform(Action::Reload)?;
                self.scan();
            }
            Action::Paths => self.path_prompt(0),
            Action::Copy(plan) => {
                let count = self.roots.as_ref().unwrap().execute_copy(plan)?;
                self.message = format!("Copied {count} files");
                if self.session.is_some() {
                    self.perform(Action::Reload)?;
                }
                self.scan_notice = Some(format!("Copied {count} files"));
                self.scan();
            }
            Action::Overwrite(side) => self.save(side, true)?,
            Action::OverwriteBoth(side) => {
                self.save(side, true)?;
                if side == 0 {
                    self.save(1, false)?;
                }
            }
        }
        Ok(())
    }

    fn paste(&mut self, text: &str) {
        if let Some(prompt) = &mut self.prompt {
            prompt.input.insert(&text.replace(['\r', '\n'], ""));
            self.update_query();
        } else if let Some(editor) = &mut self.editor {
            editor.paste(text);
        }
    }

    fn update_query(&mut self) {
        if let Some(prompt) = &self.prompt {
            match prompt.kind {
                PromptKind::FindTree => {
                    self.tree_query = prompt.input.value.clone();
                    self.selected = 0;
                }
                PromptKind::FindDiff => {
                    self.diff_query = prompt.input.value.clone();
                    self.find_diff(1, true);
                }
                _ => {}
            }
        }
    }

    fn prompt_key(&mut self, key: KeyEvent) -> Result<()> {
        let mut prompt = self.prompt.take().unwrap();
        match key.code {
            KeyCode::Esc => {
                match prompt.kind {
                    PromptKind::Path(_) if self.roots.is_none() => {
                        self.prompt = Some(prompt);
                        self.confirm(Action::Quit, "Quit LazyMerge?");
                    }
                    PromptKind::Path(_) => {
                        if let Some(roots) = &self.roots {
                            self.paths = roots.paths.clone().map(Some);
                        }
                    }
                    PromptKind::FindTree => {
                        self.tree_query.clear();
                        self.clamp_selection();
                    }
                    _ => {}
                }
                return Ok(());
            }
            KeyCode::Enter => {
                let result = match prompt.kind {
                    PromptKind::Path(side) => (|| {
                        let path = expand_path(&prompt.input.value)?;
                        if !path.is_dir() {
                            bail!("Enter an existing directory");
                        }
                        self.paths[side] = Some(path);
                        if side == 0 && (self.paths[1].is_none() || self.roots.is_some()) {
                            self.path_prompt(1);
                        } else {
                            self.set_roots()?;
                        }
                        Ok(())
                    })(),
                    PromptKind::Include => {
                        self.config.include = prompt.input.value.clone();
                        self.scan();
                        Ok(())
                    }
                    PromptKind::Exclude => {
                        self.config.exclude = prompt.input.value.clone();
                        self.scan();
                        Ok(())
                    }
                    PromptKind::FindDiff => {
                        self.find_diff(
                            if key.modifiers.contains(KeyModifiers::SHIFT) {
                                -1
                            } else {
                                1
                            },
                            false,
                        );
                        self.prompt = Some(prompt);
                        return Ok(());
                    }
                    PromptKind::FindTree => Ok(()),
                };
                if let Err(error) = result {
                    prompt.error = format!("{error:#}");
                    self.prompt = Some(prompt);
                }
                return Ok(());
            }
            KeyCode::Tab | KeyCode::BackTab if matches!(prompt.kind, PromptKind::Path(_)) => {
                if let Err(error) = prompt.input.complete(key.code == KeyCode::BackTab) {
                    prompt.error = error.to_string();
                }
            }
            _ => {
                prompt.input.handle(key);
                prompt.error.clear();
            }
        }
        self.prompt = Some(prompt);
        self.update_query();
        Ok(())
    }

    pub fn key(&mut self, key: KeyEvent) -> Result<()> {
        // Alt+s always saves both sides of an open view, even with an older Lua
        // config that still binds this shortcut to save_right.
        let mut modifiers = key.modifiers;
        modifiers.remove(KeyModifiers::SHIFT);
        let save_both = self.session.is_some()
            && self.prompt.is_none()
            && matches!(key.code, KeyCode::Char('s' | 'S'))
            && modifiers == KeyModifiers::ALT;
        let action = if save_both {
            Some("save_both".to_owned())
        } else {
            self.config.action(&key).map(str::to_owned)
        }
        .or_else(|| {
            (self.focus == Focus::Tree && key.code == KeyCode::Char('F') && modifiers.is_empty())
                .then(|| "exclude".to_owned())
        });
        let interrupted =
            key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL);
        if let Some(dialog) = &mut self.confirmation {
            match key.code {
                KeyCode::Char('y' | 'Y') => {
                    let dialog = self.confirmation.take().unwrap();
                    self.perform(dialog.action)?;
                }
                KeyCode::Char('n' | 'N') | KeyCode::Esc => {
                    self.confirmation = None;
                }
                KeyCode::Tab | KeyCode::Left | KeyCode::Right => dialog.yes = !dialog.yes,
                KeyCode::Enter => {
                    let dialog = self.confirmation.take().unwrap();
                    if dialog.yes {
                        self.perform(dialog.action)?;
                    }
                }
                _ => {}
            }
            return Ok(());
        }
        if interrupted {
            self.confirm(Action::Quit, self.quit_message());
            return Ok(());
        }
        if self.editor.is_some()
            && matches!(
                action.as_deref(),
                Some("save_left" | "save_right" | "save_both")
            )
        {
            self.apply_editor()?;
            return self.save_action(action.as_deref().unwrap());
        }
        if let Some(editor) = &mut self.editor {
            if key.code == KeyCode::Esc {
                self.editor = None;
                return Ok(());
            }
            if action.as_deref() == Some("accept_edit")
                || (action.is_none()
                    && key.code == KeyCode::Enter
                    && key.modifiers == KeyModifiers::CONTROL)
            {
                self.apply_editor()?;
            } else {
                editor.key(key, self.config.tab_width);
            }
            return Ok(());
        }
        if self.help {
            match key.code {
                KeyCode::Esc | KeyCode::Char('?') => self.help = false,
                KeyCode::Down | KeyCode::Char('j') => {
                    self.help_scroll = self.help_scroll.saturating_add(1)
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.help_scroll = self.help_scroll.saturating_sub(1)
                }
                KeyCode::PageDown => self.help_scroll = self.help_scroll.saturating_add(10),
                KeyCode::PageUp => self.help_scroll = self.help_scroll.saturating_sub(10),
                _ if action.as_deref() == Some("quit") => {
                    self.confirm(Action::Quit, self.quit_message())
                }
                _ => {}
            }
            return Ok(());
        }
        if self.prompt.is_some() {
            return self.prompt_key(key);
        }
        match action.as_deref() {
            Some("quit") => self.confirm(Action::Quit, self.quit_message()),
            Some("help") => {
                self.help = true;
                self.help_scroll = 0;
            }
            Some("paths") => self.guarded(Action::Paths, "Choose new folders?")?,
            Some("focus") => {
                if self.session.is_some() {
                    self.focus = if self.focus == Focus::Tree {
                        Focus::Diff
                    } else {
                        Focus::Tree
                    };
                }
            }
            Some("include" | "exclude") => {
                let include = action.as_deref() == Some("include");
                self.prompt = Some(Prompt {
                    kind: if include {
                        PromptKind::Include
                    } else {
                        PromptKind::Exclude
                    },
                    input: Input::new(if include {
                        self.config.include.clone()
                    } else {
                        self.config.exclude.clone()
                    }),
                    error: String::new(),
                });
            }
            Some("whitespace") => {
                self.config.ignore_whitespace = !self.config.ignore_whitespace;
                if let Some(session) = &mut self.session {
                    session.refresh_diff(self.config.ignore_whitespace);
                }
                self.scan();
            }
            Some("find") => {
                let tree = self.focus == Focus::Tree;
                self.prompt = Some(Prompt {
                    kind: if tree {
                        PromptKind::FindTree
                    } else {
                        PromptKind::FindDiff
                    },
                    input: Input::new(if tree {
                        self.tree_query.clone()
                    } else {
                        self.diff_query.clone()
                    }),
                    error: String::new(),
                });
            }
            Some("save_left" | "save_right" | "save_both") => {
                self.save_action(action.as_deref().unwrap())?
            }
            _ => match self.focus {
                Focus::Tree => self.tree_key(key, action.as_deref())?,
                Focus::Diff => self.diff_key(key, action.as_deref())?,
            },
        }
        Ok(())
    }

    fn quit_message(&self) -> String {
        let dirty = self.session.as_ref().is_some_and(Session::dirty) || self.editor.is_some();
        format!(
            "Quit LazyMerge?{}",
            if dirty {
                "\nUnsaved edits will be discarded."
            } else {
                ""
            }
        )
    }

    pub(crate) fn mouse_open_selected(&mut self) -> Result<()> {
        self.tree_key(KeyCode::Enter.into(), Some("open"))
    }

    pub(crate) fn mouse_edit_side(&mut self, side: usize) -> Result<()> {
        self.edit(side)
    }

    fn tree_key(&mut self, key: KeyEvent, action: Option<&str>) -> Result<()> {
        match (key.code, action) {
            (KeyCode::Down, None) | (_, Some("down")) => {
                self.selected = (self.selected + 1).min(self.visible().len().saturating_sub(1))
            }
            (KeyCode::Up, None) | (_, Some("up")) => {
                self.selected = self.selected.saturating_sub(1)
            }
            (KeyCode::PageDown, None) => {
                self.selected =
                    (self.selected + self.tree_height).min(self.visible().len().saturating_sub(1))
            }
            (KeyCode::PageUp, None) => {
                self.selected = self.selected.saturating_sub(self.tree_height)
            }
            (KeyCode::Home, None) | (_, Some("top")) => self.selected = 0,
            (KeyCode::End, None) | (_, Some("bottom")) => {
                self.selected = self.visible().len().saturating_sub(1)
            }
            (_, Some("reload")) => self.guarded(Action::Rescan, "Reload from disk?")?,
            (_, Some("all")) => {
                self.config.differences_only = false;
                self.clamp_selection();
            }
            (_, Some("differences")) => {
                self.config.differences_only = true;
                self.clamp_selection();
            }
            (_, Some("expand_all")) => self.expanded.extend(
                self.nodes
                    .iter()
                    .filter(|n| n.is_dir)
                    .map(|n| n.path.clone()),
            ),
            (_, Some("collapse_all")) => {
                self.expanded.clear();
                self.clamp_selection();
            }
            (KeyCode::Enter, None) | (_, Some("open")) => {
                if let Some(node) = self.selected_node() {
                    let path = node.path.clone();
                    if node.is_dir {
                        if !self.expanded.remove(&path) {
                            self.expanded.insert(path);
                        }
                    } else {
                        if self.session.as_ref().is_some_and(Session::dirty) {
                            bail!("Save or close the current file before opening another");
                        }
                        self.session = Some(
                            self.roots
                                .as_ref()
                                .unwrap()
                                .open(&path, self.config.ignore_whitespace)?,
                        );
                        self.focus = Focus::Diff;
                        self.file_change_notice = None;
                        self.diff_scroll = 0;
                        self.diff_query.clear();
                    }
                }
            }
            (KeyCode::Right, None) | (_, Some("expand")) => {
                if let Some(node) = self.selected_node().filter(|n| n.is_dir) {
                    self.expanded.insert(node.path.clone());
                }
            }
            (KeyCode::Left, None) | (_, Some("collapse")) => {
                if let Some(node) = self.selected_node() {
                    let path = node.path.clone();
                    if !self.expanded.remove(&path)
                        && let Some(parent) = path.parent()
                        && let Some(index) = self
                            .visible()
                            .iter()
                            .position(|(i, _)| self.nodes[*i].path == parent)
                    {
                        self.selected = index;
                    }
                }
            }
            (_, Some("copy_right" | "copy_left")) => {
                if self.scanning {
                    bail!("Wait for the folder comparison to finish");
                }
                if self.session.as_ref().is_some_and(Session::dirty) {
                    bail!("Save or close the current file before copying");
                }
                if let Some(node) = self.selected_node() {
                    let to = usize::from(action == Some("copy_right"));
                    let plan =
                        self.roots
                            .as_ref()
                            .unwrap()
                            .copy_plan(&self.nodes, &node.path, to)?;
                    if plan.count == 0 {
                        self.message = "No files to copy in that direction".into();
                    } else {
                        let message = format!(
                            "Copy {} files from {} to {}?\nExisting destination files will be overwritten.",
                            plan.count,
                            plan.selected.display(),
                            self.label(to)
                        );
                        self.confirm(Action::Copy(plan), message);
                    }
                }
            }
            (KeyCode::Esc, None) => {
                self.tree_query.clear();
                self.clamp_selection();
            }
            _ => {}
        }
        Ok(())
    }

    fn diff_key(&mut self, key: KeyEvent, action: Option<&str>) -> Result<()> {
        if self.session.is_none() {
            self.focus = Focus::Tree;
            return Ok(());
        }
        match (key.code, action) {
            (KeyCode::Down, None) | (_, Some("down")) => self.move_cursor(1),
            (KeyCode::Up, None) | (_, Some("up")) => self.move_cursor(-1),
            (KeyCode::PageDown, None) => self.move_cursor(self.diff_height.max(1) as isize),
            (KeyCode::PageUp, None) => self.move_cursor(-(self.diff_height.max(1) as isize)),
            (KeyCode::Char('d'), None) if key.modifiers == KeyModifiers::CONTROL => {
                self.move_cursor((self.diff_height / 2).max(1) as isize)
            }
            (KeyCode::Char('u'), None) if key.modifiers == KeyModifiers::CONTROL => {
                self.move_cursor(-((self.diff_height / 2).max(1) as isize))
            }
            (KeyCode::Home, None) => self.session.as_mut().unwrap().cursor = 0,
            (_, Some("top")) => {
                if self.pending_g {
                    self.session.as_mut().unwrap().cursor = 0;
                }
                self.pending_g = !self.pending_g;
                return Ok(());
            }
            (KeyCode::End, None) | (_, Some("bottom")) => {
                let s = self.session.as_mut().unwrap();
                s.cursor = s.diff.rows.len().saturating_sub(1);
            }
            (KeyCode::Left, None) | (_, Some("collapse")) => {
                let s = self.session.as_mut().unwrap();
                s.horizontal = s.horizontal.saturating_sub(4);
            }
            (KeyCode::Right, None) | (_, Some("expand")) => {
                self.session.as_mut().unwrap().horizontal += 4
            }
            (_, Some("next" | "previous")) => {
                self.next_hunk(if action == Some("next") { 1 } else { -1 })
            }
            (_, Some("select")) => {
                let s = self.session.as_mut().unwrap();
                s.anchor = if s.anchor.is_some() {
                    None
                } else {
                    Some(s.cursor)
                };
            }
            (_, Some("merge_right" | "merge_left" | "copy_right" | "copy_left")) => {
                let to = usize::from(matches!(action, Some("merge_right" | "copy_right")));
                let all = matches!(action, Some("copy_right" | "copy_left"));
                self.session
                    .as_mut()
                    .unwrap()
                    .merge(to, all, self.config.ignore_whitespace)?;
                self.message = "Merge staged. Save to write to disk; u to undo.".into();
            }
            (_, Some("edit_left" | "edit_right")) => {
                self.edit(usize::from(action == Some("edit_right")))?
            }
            (_, Some("undo")) => {
                if self.session.as_mut().unwrap().undo() {
                    self.session
                        .as_mut()
                        .unwrap()
                        .refresh_diff(self.config.ignore_whitespace);
                    self.message = "Change undone".into();
                }
            }
            (_, Some("reload")) => self.guarded(Action::Reload, "Reload both files from disk?")?,
            (KeyCode::Esc, None) => {
                if self.session.as_ref().unwrap().anchor.is_some() {
                    self.session.as_mut().unwrap().anchor = None;
                } else {
                    self.guarded(Action::Close, "Close this file?")?;
                }
            }
            _ => {}
        }
        self.pending_g = false;
        Ok(())
    }

    fn move_cursor(&mut self, delta: isize) {
        if let Some(s) = &mut self.session {
            s.cursor = s
                .cursor
                .saturating_add_signed(delta)
                .min(s.diff.rows.len().saturating_sub(1));
        }
    }

    fn next_hunk(&mut self, direction: isize) {
        let s = self.session.as_mut().unwrap();
        let current = s.diff.rows.get(s.cursor).and_then(|r| r.hunk);
        let found = if direction > 0 {
            s.diff
                .hunks
                .iter()
                .enumerate()
                .find(|(i, h)| Some(*i) != current && h.rows.start > s.cursor)
                .or_else(|| s.diff.hunks.iter().enumerate().next())
        } else {
            s.diff
                .hunks
                .iter()
                .enumerate()
                .rev()
                .find(|(i, h)| Some(*i) != current && h.rows.start < s.cursor)
                .or_else(|| s.diff.hunks.iter().enumerate().next_back())
        };
        if let Some((_, hunk)) = found {
            s.cursor = hunk.rows.start;
        }
    }

    fn find_diff(&mut self, direction: isize, inclusive: bool) {
        let Some(s) = &mut self.session else {
            return;
        };
        let query = self.diff_query.to_lowercase();
        if query.is_empty() {
            return;
        }
        let count = s.diff.rows.len();
        for step in usize::from(!inclusive)..count + usize::from(!inclusive) {
            let index = (s.cursor as isize + direction * step as isize)
                .rem_euclid(count.max(1) as isize) as usize;
            let row = &s.diff.rows[index];
            if [
                row.left.and_then(|i| s.docs[0].lines.get(i)),
                row.right.and_then(|i| s.docs[1].lines.get(i)),
            ]
            .into_iter()
            .flatten()
            .any(|line| line.to_lowercase().contains(&query))
            {
                s.cursor = index;
                return;
            }
        }
        self.message = "No matching text".into();
    }

    fn edit(&mut self, side: usize) -> Result<()> {
        let s = self.session.as_ref().unwrap();
        if s.docs[side].readonly {
            bail!("This encoding is read-only; copy the whole file from the tree instead");
        }
        let rows = if let Some(anchor) = s.anchor {
            anchor.min(s.cursor)..anchor.max(s.cursor) + 1
        } else if let Some(hunk) = s.diff.rows.get(s.cursor).and_then(|r| r.hunk) {
            s.diff.hunks[hunk].rows.clone()
        } else {
            s.cursor..s.cursor + 1
        };
        let range = s.range(side, rows);
        let mut text = s.docs[side].lines[range.clone()].join("\n");
        // A trailing separator represents an empty selected line as well. At EOF
        // its presence also retains the file's original final-newline state.
        if !range.is_empty() && (range.end < s.docs[side].lines.len() || s.docs[side].final_newline)
        {
            text.push('\n');
        }
        self.editor = Some(Editor::new(side, range, &text));
        Ok(())
    }

    fn apply_editor(&mut self) -> Result<()> {
        let Some(editor) = self.editor.take() else {
            return Ok(());
        };
        self.session.as_mut().unwrap().replace(
            editor.side,
            editor.range.clone(),
            &editor.text(),
            self.config.ignore_whitespace,
        )?;
        self.message = "Edit staged. Save to write to disk; u to undo.".into();
        Ok(())
    }

    fn save_action(&mut self, action: &str) -> Result<()> {
        match action {
            "save_left" => self.save(0, false)?,
            "save_right" => self.save(1, false)?,
            "save_both" => {
                self.save(0, false)?;
                if let Some(dialog) = &mut self.confirmation {
                    if matches!(dialog.action, Action::Overwrite(0)) {
                        dialog.action = Action::OverwriteBoth(0);
                    }
                } else {
                    self.save(1, false)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn save(&mut self, side: usize, overwrite: bool) -> Result<()> {
        let (Some(roots), Some(session)) = (&self.roots, &mut self.session) else {
            return Ok(());
        };
        if !session.docs[side].dirty() {
            self.message = "No unsaved changes on that side".into();
            return Ok(());
        }
        if !overwrite && roots.disk_changed(session)[side] {
            self.confirm(
                Action::Overwrite(side),
                format!(
                    "{} changed on disk. Overwrite with your staged edits?",
                    self.label(side)
                ),
            );
            return Ok(());
        }
        roots.save(session, side, overwrite)?;
        if !session.stale.into_iter().any(|stale| stale) {
            self.file_change_notice = None;
        }
        self.message = format!("Saved {}", self.label(side));
        self.scan_notice = Some(match self.scan_notice.take() {
            Some(notice) if notice.starts_with("Saved ") => {
                format!("{notice}, {}", self.label(side))
            }
            _ => self.message.clone(),
        });
        self.scan();
        Ok(())
    }

    pub fn label(&self, side: usize) -> &str {
        if side == 0 {
            &self.config.left_label
        } else {
            &self.config.right_label
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Document;
    use std::{fs, path::Path};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn app_with_text(text: &str) -> App {
        let mut app = App::new(Config::default(), None, None).unwrap();
        app.prompt = None;
        app.session = Some(Session::new(
            "sample.txt".into(),
            [
                Document::from_bytes(Some(text.as_bytes().to_vec())),
                Document::from_bytes(Some(text.as_bytes().to_vec())),
            ],
            false,
        ));
        app.focus = Focus::Diff;
        app
    }

    fn folder_app() -> (tempfile::TempDir, App) {
        let directory = tempfile::tempdir().unwrap();
        let left = directory.path().join("left");
        let right = directory.path().join("right");
        fs::create_dir(&left).unwrap();
        fs::create_dir(&right).unwrap();
        fs::write(left.join("sample.txt"), "left\n").unwrap();
        fs::write(right.join("sample.txt"), "right\n").unwrap();
        let mut app = App::new(Config::default(), Some(left), Some(right)).unwrap();
        // These tests control refreshes directly; discard the startup receiver.
        app.scan_rx = None;
        app.scanning = false;
        app.session = Some(
            app.roots
                .as_ref()
                .unwrap()
                .open(Path::new("sample.txt"), false)
                .unwrap(),
        );
        app.focus = Focus::Diff;
        (directory, app)
    }

    #[test]
    fn opening_and_applying_blank_lines_preserves_content() {
        for (text, cursor) in [("before\n\nend\n", 1), ("before\n\n", 1), ("\n", 0)] {
            let mut app = app_with_text(text);
            app.session.as_mut().unwrap().cursor = cursor;
            app.edit(0).unwrap();
            assert_eq!(app.editor.as_ref().unwrap().text(), "\n");
            app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL))
                .unwrap();
            let session = app.session.as_ref().unwrap();
            assert_eq!(session.docs[0].encode(), text.as_bytes());
            assert!(!session.dirty());
        }
    }

    #[test]
    fn editor_paste_preserves_tabs_unicode_and_cursor_tail() {
        let mut editor = Editor::new(0, 0..1, "prefixTAIL");
        editor.lines[0].cursor = 6;
        editor.paste("\tα\r\n\tβ");
        assert_eq!(editor.text(), "prefix\tα\n\tβTAIL");
        assert_eq!(editor.row, 1);
        assert_eq!(editor.lines[1].cursor, "\tβ".len());
        editor.key(key(KeyCode::Enter), 4);
        assert_eq!(editor.text(), "prefix\tα\n\tβ\nTAIL");
    }

    #[test]
    fn text_fields_keep_q_and_interrupt_always_asks_before_quit() {
        let mut app = App::new(Config::default(), None, None).unwrap();
        app.key(key(KeyCode::Char('q'))).unwrap();
        assert_eq!(app.prompt.as_ref().unwrap().input.value, "q");
        assert!(app.confirmation.is_none());
        app.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))
            .unwrap();
        assert!(app.confirmation.is_some());
        assert!(!app.exit);
        app.key(key(KeyCode::Enter)).unwrap();
        assert!(!app.exit);
        assert!(app.confirmation.is_none());

        let mut app = app_with_text("original\n");
        app.edit(0).unwrap();
        app.key(key(KeyCode::Char('q'))).unwrap();
        assert!(app.editor.as_ref().unwrap().text().contains('q'));
        assert!(app.confirmation.is_none());
        app.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))
            .unwrap();
        assert!(
            app.confirmation
                .as_ref()
                .unwrap()
                .message
                .contains("Unsaved")
        );
        app.key(key(KeyCode::Char('y'))).unwrap();
        assert!(app.exit);
    }

    #[test]
    fn control_page_motion_and_custom_bindings_have_correct_precedence() {
        let text = (0..100).map(|i| format!("line {i}\n")).collect::<String>();
        let mut app = app_with_text(&text);
        app.session.as_mut().unwrap().cursor = 50;
        app.diff_height = 10;
        app.key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL))
            .unwrap();
        assert_eq!(app.session.as_ref().unwrap().cursor, 55);
        app.key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL))
            .unwrap();
        assert_eq!(app.session.as_ref().unwrap().cursor, 50);
        app.key(key(KeyCode::Down)).unwrap();
        assert_eq!(app.session.as_ref().unwrap().cursor, 51);
        app.key(key(KeyCode::Up)).unwrap();
        assert_eq!(app.session.as_ref().unwrap().cursor, 50);
        app.config.keys.insert("undo".into(), "ctrl+d".into());
        app.session
            .as_mut()
            .unwrap()
            .replace(0, 0..1, "changed\n", false)
            .unwrap();
        app.key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL))
            .unwrap();
        assert!(!app.session.as_ref().unwrap().dirty());
    }

    #[test]
    fn named_key_bindings_override_default_navigation() {
        let mut app = app_with_text("before\nafter\n");
        app.config.keys.insert("undo".into(), "down".into());
        app.session
            .as_mut()
            .unwrap()
            .replace(0, 0..1, "changed\n", false)
            .unwrap();
        app.key(key(KeyCode::Down)).unwrap();
        assert!(!app.session.as_ref().unwrap().dirty());
        app.config.keys.insert("bottom".into(), "right".into());
        app.key(key(KeyCode::Right)).unwrap();
        assert_eq!(app.session.as_ref().unwrap().cursor, 1);
        assert_eq!(app.session.as_ref().unwrap().horizontal, 0);
    }

    #[test]
    fn save_shortcut_applies_open_editor_before_writing() {
        let (_directory, mut app) = folder_app();
        app.edit(0).unwrap();
        app.editor.as_mut().unwrap().lines[0] = Input::new("edited\tleft");
        app.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))
            .unwrap();
        assert!(app.editor.is_none());
        let session = app.session.as_ref().unwrap();
        assert!(!session.docs[0].dirty());
        assert_eq!(
            fs::read(app.roots.as_ref().unwrap().paths[0].join(&session.path)).unwrap(),
            b"edited\tleft\n"
        );
    }

    #[test]
    fn save_both_resumes_after_confirming_external_overwrite() {
        let (_directory, mut app) = folder_app();
        let roots = app.roots.as_ref().unwrap().clone();
        let session = app.session.as_mut().unwrap();
        session.replace(0, 0..1, "edited left\n", false).unwrap();
        session.replace(1, 0..1, "edited right\n", false).unwrap();
        fs::write(roots.paths[0].join("sample.txt"), "external\n").unwrap();
        app.key(KeyEvent::new(
            KeyCode::Char('S'),
            KeyModifiers::ALT | KeyModifiers::SHIFT,
        ))
        .unwrap();
        assert!(matches!(
            app.confirmation.as_ref().unwrap().action,
            Action::OverwriteBoth(0)
        ));
        assert_eq!(
            fs::read(roots.paths[0].join("sample.txt")).unwrap(),
            b"external\n"
        );
        app.key(key(KeyCode::Char('y'))).unwrap();
        assert_eq!(
            fs::read(roots.paths[0].join("sample.txt")).unwrap(),
            b"edited left\n"
        );
        assert_eq!(
            fs::read(roots.paths[1].join("sample.txt")).unwrap(),
            b"edited right\n"
        );
        assert!(!app.session.as_ref().unwrap().dirty());
    }

    #[test]
    fn filter_changes_during_scan_schedule_a_fresh_scan() {
        let (_directory, mut app) = folder_app();
        let (tx, rx) = mpsc::channel();
        app.scan_rx = Some(rx);
        app.scanning = true;
        app.config.include = "*.rs".into();
        app.scan();
        assert!(app.rescan_requested);
        tx.send(Ok(Vec::new())).unwrap();
        app.receive_scan();
        assert!(app.scanning);
        assert!(!app.rescan_requested);
        let result = app
            .scan_rx
            .as_ref()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn cancelling_folder_change_restores_current_paths() {
        let (directory, mut app) = folder_app();
        let original = app.roots.as_ref().unwrap().paths.clone().map(Some);
        let replacement = directory.path().join("new left");
        fs::create_dir(&replacement).unwrap();
        app.perform(Action::Paths).unwrap();
        app.prompt.as_mut().unwrap().input = Input::new(replacement.to_string_lossy());
        app.key(key(KeyCode::Enter)).unwrap();
        assert!(matches!(
            app.prompt.as_ref().unwrap().kind,
            PromptKind::Path(1)
        ));
        app.key(key(KeyCode::Esc)).unwrap();
        assert_eq!(app.paths, original);
        assert!(app.session.is_some());
    }

    #[test]
    fn shift_f_opens_exclude_and_alt_enter_stages_an_edit() {
        let mut app = app_with_text("original\n");
        app.focus = Focus::Tree;
        app.key(KeyEvent::new(KeyCode::Char('F'), KeyModifiers::SHIFT))
            .unwrap();
        assert!(matches!(
            app.prompt.as_ref().unwrap().kind,
            PromptKind::Exclude
        ));
        app.key(key(KeyCode::Esc)).unwrap();
        app.focus = Focus::Diff;
        app.edit(0).unwrap();
        app.editor.as_mut().unwrap().lines[0] = Input::new("edited");
        app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT))
            .unwrap();
        assert!(app.editor.is_none());
        let session = app.session.as_ref().unwrap();
        assert_eq!(session.docs[0].lines, ["edited"]);
        assert!(session.dirty());
        assert_eq!(
            session.docs[0].baseline.as_deref(),
            Some(b"original\n".as_slice())
        );
    }

    #[test]
    fn alt_s_saves_both_dirty_sides_with_legacy_bindings() {
        let (_directory, mut app) = folder_app();
        app.config.keys.insert("save_right".into(), "alt+s".into());
        app.config.keys.insert("save_both".into(), "alt+S".into());
        let session = app.session.as_mut().unwrap();
        session.replace(0, 0..1, "staged left\n", false).unwrap();
        session.replace(1, 0..1, "staged right\n", false).unwrap();
        app.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::ALT))
            .unwrap();
        let roots = app.roots.as_ref().unwrap();
        assert_eq!(
            fs::read(roots.paths[0].join("sample.txt")).unwrap(),
            b"staged left\n"
        );
        assert_eq!(
            fs::read(roots.paths[1].join("sample.txt")).unwrap(),
            b"staged right\n"
        );
        assert!(!app.session.as_ref().unwrap().dirty());
    }

    fn finish_scan(app: &mut App) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.scanning {
            app.receive_scan();
            assert!(Instant::now() < deadline, "folder scan did not finish");
            if app.scanning {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }

    #[test]
    fn polling_reloads_clean_content_even_with_unchanged_size_and_timestamp() {
        let (_directory, mut app) = folder_app();
        let path = app.roots.as_ref().unwrap().paths[1].join("sample.txt");
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        fs::write(&path, "other\n").unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(modified))
            .unwrap();
        app.session.as_mut().unwrap().horizontal = 4;
        assert!(app.check_disk_changes());
        let session = app.session.as_ref().unwrap();
        assert_eq!(session.docs[1].lines, ["other"]);
        assert_eq!(session.horizontal, 4);
        assert_eq!(session.stale, [false, false]);
        assert!(!session.dirty());
        assert!(app.message.contains("reloaded"));
        assert!(
            app.file_change_notice
                .as_ref()
                .unwrap()
                .contains("reloaded")
        );
        finish_scan(&mut app);
        assert!(app.message.contains("reloaded"));
        app.perform(Action::Reload).unwrap();
        assert!(app.file_change_notice.is_none());
    }

    #[test]
    fn polling_keeps_staged_and_open_editor_text_and_checks_overwrites() {
        let (_directory, mut app) = folder_app();
        let path = app.roots.as_ref().unwrap().paths[0].join("sample.txt");
        app.session
            .as_mut()
            .unwrap()
            .replace(0, 0..1, "mine\n", false)
            .unwrap();
        fs::write(&path, "disk\n").unwrap();
        assert!(app.check_disk_changes());
        assert_eq!(app.session.as_ref().unwrap().docs[0].lines, ["mine"]);
        assert_eq!(app.session.as_ref().unwrap().stale, [true, false]);
        app.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::ALT))
            .unwrap();
        assert!(matches!(
            app.confirmation.as_ref().unwrap().action,
            Action::OverwriteBoth(0)
        ));
        assert_eq!(fs::read(&path).unwrap(), b"disk\n");
        app.key(key(KeyCode::Char('n'))).unwrap();
        app.perform(Action::Reload).unwrap();
        app.edit(0).unwrap();
        app.editor.as_mut().unwrap().lines[0] = Input::new("draft");
        fs::write(&path, "new disk\n").unwrap();
        app.check_disk_changes();
        assert_eq!(app.editor.as_ref().unwrap().lines[0].value, "draft");
        assert_eq!(app.session.as_ref().unwrap().docs[0].lines, ["disk"]);
        app.key(key(KeyCode::Esc)).unwrap();
        app.check_disk_changes();
        assert_eq!(app.session.as_ref().unwrap().docs[0].lines, ["new disk"]);
    }

    #[test]
    fn polling_overview_discovers_changes_additions_and_deletions() {
        let (_directory, mut app) = folder_app();
        app.session = None;
        app.focus = Focus::Tree;
        let roots = app.roots.as_ref().unwrap().clone();
        app.nodes = storage::scan(&roots, "", "", false).unwrap();
        fs::write(roots.paths[0].join("sample.txt"), "right\n").unwrap();
        fs::write(roots.paths[0].join("new.txt"), "new\n").unwrap();
        app.check_disk_changes();
        finish_scan(&mut app);
        assert_eq!(
            app.nodes
                .iter()
                .find(|n| n.path == Path::new("sample.txt"))
                .unwrap()
                .status,
            crate::model::Status::Same
        );
        assert_eq!(
            app.nodes
                .iter()
                .find(|n| n.path == Path::new("new.txt"))
                .unwrap()
                .status,
            crate::model::Status::LeftOnly
        );
        fs::remove_file(roots.paths[0].join("sample.txt")).unwrap();
        fs::remove_file(roots.paths[1].join("sample.txt")).unwrap();
        app.check_disk_changes();
        finish_scan(&mut app);
        assert!(!app.nodes.iter().any(|n| n.path == Path::new("sample.txt")));
    }
}
