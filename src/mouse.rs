//! Mouse hit testing uses the rectangles rendered by the most recent frame.
use crate::app::{App, Focus};
use anyhow::Result;
use ratatui::{
    crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind},
    layout::Rect,
};
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthChar;

#[derive(Clone, Copy, Debug)]
pub struct EditorHit {
    pub area: Rect,
    pub horizontal: usize,
}

#[derive(Default)]
pub struct MouseState {
    pub tree_pane: Rect,
    pub tree_rows: Rect,
    pub diff_pane: Rect,
    pub diff_rows: [Rect; 2],
    pub diff_map: Rect,
    pub diff_gutter: Rect,
    pub editor: Option<EditorHit>,
    pub confirm_no: Rect,
    pub confirm_yes: Rect,
    drag_anchor: Option<usize>,
    last_diff_click: Option<(Instant, usize, usize)>,
}

impl MouseState {
    pub fn reset_layout(&mut self) {
        self.tree_pane = Rect::default();
        self.tree_rows = Rect::default();
        self.diff_pane = Rect::default();
        self.diff_rows = [Rect::default(); 2];
        self.diff_map = Rect::default();
        self.diff_gutter = Rect::default();
        self.editor = None;
        self.confirm_no = Rect::default();
        self.confirm_yes = Rect::default();
    }
}

fn contains(rect: Rect, event: MouseEvent) -> bool {
    event.column >= rect.x
        && event.column < rect.right()
        && event.row >= rect.y
        && event.row < rect.bottom()
}

impl App {
    pub fn mouse(&mut self, event: MouseEvent) -> Result<()> {
        if event.kind == MouseEventKind::Up(MouseButton::Left) {
            self.mouse_state.drag_anchor = None;
        }
        if self.confirmation.is_some() {
            if event.kind == MouseEventKind::Down(MouseButton::Left) {
                if contains(self.mouse_state.confirm_no, event) {
                    self.key(KeyCode::Esc.into())?;
                } else if contains(self.mouse_state.confirm_yes, event) {
                    self.key(KeyCode::Char('y').into())?;
                }
            }
            return Ok(());
        }
        if self.prompt.is_some() {
            return Ok(());
        }
        if self.editor.is_some() {
            self.editor_mouse(event);
            return Ok(());
        }
        if self.help {
            match event.kind {
                MouseEventKind::ScrollDown => self.help_scroll = self.help_scroll.saturating_add(3),
                MouseEventKind::ScrollUp => self.help_scroll = self.help_scroll.saturating_sub(3),
                _ => {}
            }
            return Ok(());
        }
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => self.mouse_down(event)?,
            MouseEventKind::Drag(MouseButton::Left) => self.mouse_drag(event),
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let direction = if event.kind == MouseEventKind::ScrollUp {
                    -3
                } else {
                    3
                };
                if contains(self.mouse_state.tree_pane, event) {
                    self.focus = Focus::Tree;
                    self.scroll_tree(direction);
                } else if contains(self.mouse_state.diff_pane, event) {
                    self.focus = Focus::Diff;
                    if event.modifiers.contains(KeyModifiers::SHIFT) {
                        self.scroll_horizontal(direction);
                    } else {
                        self.scroll_diff(direction);
                    }
                }
            }
            MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight
                if contains(self.mouse_state.diff_pane, event) =>
            {
                self.focus = Focus::Diff;
                self.scroll_horizontal(if event.kind == MouseEventKind::ScrollLeft {
                    -4
                } else {
                    4
                });
            }
            _ => {}
        }
        Ok(())
    }

    fn mouse_down(&mut self, event: MouseEvent) -> Result<()> {
        self.mouse_state.drag_anchor = None;
        if contains(self.mouse_state.tree_pane, event) {
            self.focus = Focus::Tree;
            self.mouse_state.last_diff_click = None;
            if contains(self.mouse_state.tree_rows, event) {
                let selected =
                    self.tree_scroll + usize::from(event.row - self.mouse_state.tree_rows.y);
                if selected < self.visible().len() {
                    self.selected = selected;
                    self.mouse_open_selected()?;
                }
            }
            return Ok(());
        }
        if !contains(self.mouse_state.diff_pane, event) {
            return Ok(());
        }
        self.focus = Focus::Diff;
        let Some(session) = &mut self.session else {
            return Ok(());
        };
        let count = session.diff.rows.len();
        if count == 0 || session.docs.iter().any(|doc| doc.binary) {
            return Ok(());
        }
        if contains(self.mouse_state.diff_map, event) {
            let position = usize::from(event.row - self.mouse_state.diff_map.y);
            let target = position * count / usize::from(self.mouse_state.diff_map.height.max(1));
            session.cursor = session
                .diff
                .hunks
                .iter()
                .min_by_key(|hunk| hunk.rows.start.abs_diff(target))
                .map_or(target.min(count - 1), |hunk| hunk.rows.start);
            session.anchor = None;
            return Ok(());
        }
        if contains(self.mouse_state.diff_gutter, event) {
            let row = self.diff_scroll + usize::from(event.row - self.mouse_state.diff_gutter.y);
            if session
                .diff
                .rows
                .get(row)
                .is_some_and(|row| row.hunk.is_some())
            {
                session.cursor = row;
                session.anchor = None;
                let to = usize::from(event.column > self.mouse_state.diff_gutter.x);
                session.merge(to, false, self.config.ignore_whitespace)?;
                self.message = "Merge staged. Save to write to disk; u to undo.".into();
            }
            return Ok(());
        }
        if let Some(side) = self
            .mouse_state
            .diff_rows
            .iter()
            .position(|&rect| contains(rect, event))
        {
            let row =
                self.diff_scroll + usize::from(event.row - self.mouse_state.diff_rows[side].y);
            if row >= count {
                return Ok(());
            }
            let shift = event.modifiers.contains(KeyModifiers::SHIFT);
            session.anchor = if shift {
                Some(session.anchor.unwrap_or(session.cursor))
            } else {
                None
            };
            session.cursor = row;
            self.mouse_state.drag_anchor = Some(session.anchor.unwrap_or(row));
            let now = Instant::now();
            let double_click = !shift
                && self.mouse_state.last_diff_click.is_some_and(
                    |(when, previous_side, previous_row)| {
                        previous_side == side
                            && previous_row == row
                            && now.duration_since(when) <= Duration::from_millis(400)
                    },
                );
            if double_click {
                self.mouse_state.last_diff_click = None;
                self.mouse_state.drag_anchor = None;
                self.mouse_edit_side(side)?;
            } else {
                self.mouse_state.last_diff_click = Some((now, side, row));
            }
        }
        Ok(())
    }

    fn mouse_drag(&mut self, event: MouseEvent) {
        let Some(anchor) = self.mouse_state.drag_anchor else {
            return;
        };
        let Some(session) = &mut self.session else {
            return;
        };
        if session.diff.rows.is_empty() {
            return;
        }
        let rect = self.mouse_state.diff_rows[0];
        if rect.height == 0 {
            return;
        }
        let row = if event.row < rect.y {
            self.diff_scroll.saturating_sub(1)
        } else if event.row >= rect.bottom() {
            self.diff_scroll.saturating_add(usize::from(rect.height))
        } else {
            self.diff_scroll + usize::from(event.row - rect.y)
        };
        session.anchor = Some(anchor);
        session.cursor = row.min(session.diff.rows.len() - 1);
    }

    fn scroll_tree(&mut self, direction: isize) {
        let count = self.visible().len();
        let height = self.tree_height.max(1);
        self.tree_scroll = self
            .tree_scroll
            .saturating_add_signed(direction)
            .min(count.saturating_sub(height));
        self.selected = self
            .selected
            .max(self.tree_scroll)
            .min((self.tree_scroll + height - 1).min(count.saturating_sub(1)));
    }

    fn scroll_diff(&mut self, direction: isize) {
        let Some(session) = &mut self.session else {
            return;
        };
        let count = session.diff.rows.len();
        if count == 0 {
            return;
        }
        let height = self.diff_height.max(1);
        let margin = self.config.scrolloff.min(height.saturating_sub(1) / 2);
        self.diff_scroll = self
            .diff_scroll
            .saturating_add_signed(direction)
            .min(count.saturating_sub(height));
        let first = (self.diff_scroll + margin).min(count - 1);
        let last = (self.diff_scroll + height - 1 - margin).min(count - 1);
        session.cursor = session.cursor.clamp(first.min(last), last);
    }

    fn scroll_horizontal(&mut self, direction: isize) {
        if let Some(session) = &mut self.session {
            session.horizontal = session.horizontal.saturating_add_signed(direction);
        }
    }

    fn editor_mouse(&mut self, event: MouseEvent) {
        let Some(hit) = self.mouse_state.editor else {
            return;
        };
        if !contains(hit.area, event) {
            return;
        }
        let editor = self.editor.as_mut().unwrap();
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let row = editor.scroll + usize::from(event.row - hit.area.y);
                if let Some(line) = editor.lines.get_mut(row) {
                    let column = hit.horizontal + usize::from(event.column - hit.area.x);
                    line.cursor = cursor_at_column(&line.value, column, self.config.tab_width);
                    editor.row = row;
                }
            }
            MouseEventKind::ScrollDown => editor.row = (editor.row + 3).min(editor.lines.len() - 1),
            MouseEventKind::ScrollUp => editor.row = editor.row.saturating_sub(3),
            _ => {}
        }
    }
}

fn cursor_at_column(text: &str, target: usize, tab_width: usize) -> usize {
    let mut column = 0;
    for (byte, character) in text.char_indices() {
        let width = if character == '\t' {
            tab_width - column % tab_width
        } else {
            character.width().unwrap_or(1)
        };
        if target < column + width {
            return byte;
        }
        column += width;
    }
    text.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::Config,
        model::{Document, Session},
        storage::{self, Roots},
        ui,
    };
    use ratatui::{Terminal, backend::TestBackend};
    use std::{fs, path::PathBuf};

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn draw(app: &mut App) {
        Terminal::new(TestBackend::new(120, 40))
            .unwrap()
            .draw(|frame| ui::draw(frame, app))
            .unwrap();
    }

    fn diff_app(left: &str, right: &str) -> App {
        let mut app = App::new(Config::default(), None, None).unwrap();
        app.prompt = None;
        app.roots = Some(Roots {
            paths: [PathBuf::from("/left"), PathBuf::from("/right")],
        });
        app.session = Some(Session::new(
            "sample.txt".into(),
            [
                Document::from_bytes(Some(left.as_bytes().to_vec())),
                Document::from_bytes(Some(right.as_bytes().to_vec())),
            ],
            false,
        ));
        app.focus = Focus::Diff;
        draw(&mut app);
        app
    }

    #[test]
    fn folder_clicks_toggle_and_open_using_existing_unsaved_edit_guards() {
        let directory = tempfile::tempdir().unwrap();
        let left = directory.path().join("left");
        let right = directory.path().join("right");
        for root in [&left, &right] {
            fs::create_dir_all(root.join("folder")).unwrap();
            fs::write(root.join("folder/first.txt"), "first\n").unwrap();
            fs::write(root.join("folder/second.txt"), "second\n").unwrap();
        }
        let roots = Roots::new(&left, &right).unwrap();
        let mut app = App::new(Config::default(), None, None).unwrap();
        app.prompt = None;
        app.nodes = storage::scan(&roots, "", "", false).unwrap();
        app.roots = Some(roots);
        draw(&mut app);
        let rect = app.mouse_state.tree_rows;
        app.mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            rect.x + 3,
            rect.y,
        ))
        .unwrap();
        assert!(app.expanded.contains(&PathBuf::from("folder")));
        draw(&mut app);
        let rect = app.mouse_state.tree_rows;
        app.mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            rect.x + 3,
            rect.y + 1,
        ))
        .unwrap();
        assert_eq!(
            app.session.as_ref().unwrap().path,
            PathBuf::from("folder/first.txt")
        );
        app.session
            .as_mut()
            .unwrap()
            .replace(0, 0..1, "edited\n", false)
            .unwrap();
        draw(&mut app);
        let rect = app.mouse_state.tree_rows;
        assert!(
            app.mouse(mouse(
                MouseEventKind::Down(MouseButton::Left),
                rect.x + 3,
                rect.y + 2
            ))
            .is_err()
        );
        assert_eq!(
            app.session.as_ref().unwrap().path,
            PathBuf::from("folder/first.txt")
        );
        assert!(app.session.as_ref().unwrap().dirty());
    }

    #[test]
    fn diff_drag_shift_click_and_wheel_use_rendered_rows() {
        let text = (0..100)
            .map(|line| format!("line {line}\n"))
            .collect::<String>();
        let mut app = diff_app(&text, &text);
        let rect = app.mouse_state.diff_rows[0];
        app.mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            rect.x + 8,
            rect.y + 1,
        ))
        .unwrap();
        app.mouse(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            rect.x + 8,
            rect.y + 4,
        ))
        .unwrap();
        app.mouse(mouse(
            MouseEventKind::Up(MouseButton::Left),
            rect.x + 8,
            rect.y + 4,
        ))
        .unwrap();
        assert_eq!(app.session.as_ref().unwrap().anchor, Some(1));
        assert_eq!(app.session.as_ref().unwrap().cursor, 4);
        let mut event = mouse(
            MouseEventKind::Down(MouseButton::Left),
            rect.x + 8,
            rect.y + 5,
        );
        event.modifiers = KeyModifiers::SHIFT;
        app.mouse(event).unwrap();
        assert_eq!(app.session.as_ref().unwrap().anchor, Some(1));
        assert_eq!(app.session.as_ref().unwrap().cursor, 5);
        app.mouse(mouse(MouseEventKind::ScrollDown, rect.x + 8, rect.y))
            .unwrap();
        draw(&mut app);
        assert_eq!(app.diff_scroll, 3);
        let rect = app.mouse_state.diff_rows[0];
        app.mouse(mouse(MouseEventKind::ScrollRight, rect.x + 8, rect.y))
            .unwrap();
        assert_eq!(app.session.as_ref().unwrap().horizontal, 4);
    }

    #[test]
    fn overview_and_direction_arrows_jump_and_stage_without_writing() {
        let mut app = diff_app("left\nsame\nlast left\n", "right\nsame\nlast right\n");
        let map = app.mouse_state.diff_map;
        app.mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            map.x,
            map.bottom() - 1,
        ))
        .unwrap();
        assert_eq!(app.session.as_ref().unwrap().cursor, 2);
        draw(&mut app);
        let gutter = app.mouse_state.diff_gutter;
        let row = gutter.y + (app.session.as_ref().unwrap().cursor - app.diff_scroll) as u16;
        app.mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            gutter.x + 1,
            row,
        ))
        .unwrap();
        let session = app.session.as_ref().unwrap();
        assert_eq!(session.docs[1].lines[2], "last left");
        assert!(session.docs[1].dirty());
        assert_eq!(
            session.docs[1].baseline.as_deref(),
            Some(b"right\nsame\nlast right\n".as_slice())
        );
    }

    #[test]
    fn double_click_edits_and_editor_click_respects_tabs_and_wide_characters() {
        let mut app = diff_app("a\t界b\n", "a\t界b\n");
        let rect = app.mouse_state.diff_rows[0];
        let event = mouse(MouseEventKind::Down(MouseButton::Left), rect.x + 8, rect.y);
        app.mouse(event).unwrap();
        app.mouse(event).unwrap();
        assert!(app.editor.is_some());
        draw(&mut app);
        let editor = app.mouse_state.editor.unwrap();
        app.mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            editor.area.x + 5,
            editor.area.y,
        ))
        .unwrap();
        assert_eq!(app.editor.as_ref().unwrap().lines[0].cursor, 2);
        assert_eq!(cursor_at_column("a\t界b", 6, 4), 5);
    }

    #[test]
    fn confirmation_buttons_consume_clicks_and_cancellation_preserves_edits() {
        let mut app = diff_app("old\n", "other\n");
        app.session
            .as_mut()
            .unwrap()
            .replace(0, 0..1, "new\n", false)
            .unwrap();
        app.key(KeyCode::Char('q').into()).unwrap();
        draw(&mut app);
        let no = app.mouse_state.confirm_no;
        app.mouse(mouse(MouseEventKind::Down(MouseButton::Left), no.x, no.y))
            .unwrap();
        assert!(app.confirmation.is_none());
        assert!(app.session.as_ref().unwrap().dirty());
    }
}
