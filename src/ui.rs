use crate::{
    app::{App, Focus, PromptKind},
    highlight::{self, HighlightCache},
    model::{Session, Status},
};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};
use std::{borrow::Cow, cell::RefCell, ops::Range};
use unicode_width::UnicodeWidthStr;

fn header_height(app: &App, area: Rect) -> u16 {
    let (width, rows) = logo_size();
    let height = rows.saturating_add(1);
    if app.config.show_logo
        && area.width >= width.saturating_add(2)
        && area.height >= height.saturating_add(if app.session.is_some() { 24 } else { 10 })
    {
        height
    } else {
        2
    }
}

fn logo_size() -> (u16, u16) {
    let rows = include_str!("logo.txt").lines();
    let width = rows
        .clone()
        .map(|row| row.trim_end().width())
        .max()
        .unwrap_or(0);
    (
        width.min(u16::MAX as usize) as u16,
        rows.count().min(u16::MAX as usize) as u16,
    )
}

fn modal_space(app: &App, area: Rect) -> Rect {
    let height = header_height(app, area).min(area.height);
    Rect::new(area.x, area.y + height, area.width, area.height - height)
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    app.mouse_state.reset_layout();
    let area = frame.area();
    let theme = &app.config.colors;
    let base = Style::default().fg(theme.color("fg")).bg(theme.color("bg"));
    frame.render_widget(Block::default().style(base), area);
    if area.width < 32 || area.height < 10 {
        frame.render_widget(Paragraph::new("LazyMerge\nResize the terminal to at least 32 × 10.\nq: quit (confirmation required)").style(base).alignment(Alignment::Center).wrap(Wrap { trim: false }), area);
        if app.confirmation.is_some() {
            confirmation(frame, app);
        }
        return;
    }
    let logo_height = header_height(app, area);
    let parts = Layout::vertical([
        Constraint::Length(logo_height),
        Constraint::Length(2),
        Constraint::Min(3),
        Constraint::Length(if app.file_change_notice.is_some() {
            if area.height >= 12 { 4 } else { 3 }
        } else {
            2
        }),
    ])
    .split(area);
    header(frame, app, parts[0], logo_height > 2);
    toolbar(frame, app, parts[1]);
    if app.roots.is_some() {
        if app.session.is_some() && app.compares_files() {
            // Two compared files: no folder tree, the file view fills the body.
            diff(frame, app, parts[2]);
        } else if app.session.is_some() {
            let body = Layout::vertical([
                Constraint::Percentage(app.config.tree_percent),
                Constraint::Min(5),
            ])
            .split(parts[2]);
            tree(frame, app, body[0]);
            diff(frame, app, body[1]);
        } else {
            tree(frame, app, parts[2]);
        }
    } else {
        frame.render_widget(Paragraph::new("Choose two folders or two files to compare\n\nTab completes paths. Paths may contain spaces.\nFiles are written only when you save or confirm a copy.")
            .alignment(Alignment::Center).style(base.fg(app.config.colors.color("muted"))), centered(parts[2], 74, 7));
    }
    footer(frame, app, parts[3]);
    if app.help {
        help(frame, app);
    }
    if app.prompt.is_some() {
        prompt(frame, app);
    }
    if app.editor.is_some() {
        editor(frame, app);
    }
    if app.confirmation.is_some() {
        confirmation(frame, app);
    }
}

fn header(frame: &mut Frame, app: &App, area: Rect, large: bool) {
    let t = &app.config.colors;
    let mut lines = vec![];
    if large {
        let (width, height) = logo_size();
        for row in include_str!("logo.txt").lines() {
            let row = row.trim_end();
            let boundary = row
                .char_indices()
                .nth(22)
                .map_or(row.len(), |(index, _)| index);
            lines.push(Line::from(vec![
                Span::styled(
                    row[..boundary].to_owned(),
                    Style::default().fg(t.color("logo_lazy")),
                ),
                Span::styled(
                    row[boundary..].to_owned(),
                    Style::default().fg(t.color("logo_merge")),
                ),
                Span::raw(" ".repeat(usize::from(width).saturating_sub(row.width()))),
            ]));
        }
        let art = Rect::new(area.x + (area.width - width) / 2, area.y, width, height);
        frame.render_widget(Paragraph::new(lines), art);
    } else {
        lines.push(Line::from(vec![
            Span::styled(
                "Lazy",
                Style::default()
                    .fg(t.color("logo_lazy"))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "Merge",
                Style::default()
                    .fg(t.color("logo_merge"))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                if area.width >= 34 {
                    "  folder compare & merge"
                } else {
                    ""
                },
                Style::default().fg(t.color("muted")),
            ),
        ]));
        frame.render_widget(Paragraph::new(lines).alignment(Alignment::Center), area);
    }
    let title = Line::from(vec![
        Span::styled(" Lazy", Style::default().fg(t.color("logo_lazy"))),
        Span::styled("Merge ", Style::default().fg(t.color("logo_merge"))),
        Span::styled(
            format!("· v{} ", env!("CARGO_PKG_VERSION")),
            Style::default().fg(t.color("muted")),
        ),
    ]);
    frame.render_widget(
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(t.color("border")))
            .title_bottom(title)
            .title_alignment(Alignment::Center),
        area,
    );
}

fn toolbar(frame: &mut Frame, app: &App, area: Rect) {
    let t = &app.config.colors;
    let key = |action: &str| app.config.keys.get(action).cloned().unwrap_or_default();
    let mode = if app.config.differences_only {
        "Differences"
    } else {
        "All files"
    };
    let ws = if app.config.ignore_whitespace {
        "[w] ignore whitespace ✓"
    } else {
        "[w] ignore whitespace"
    };
    if app.compares_files() {
        // The folder filters and the tree's file modes do not apply to two files.
        let first = Line::from(vec![
            Span::styled(
                " [Two files] ",
                Style::default()
                    .fg(t.color("accent"))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(
                "{} reload  {} paths  {} help",
                key("reload"),
                key("paths"),
                key("help")
            )),
        ]);
        let second = Line::styled(format!(" {ws}"), Style::default().fg(t.color("muted")));
        frame.render_widget(Paragraph::new(vec![first, second]), area);
        return;
    }
    let first = Line::from(vec![
        Span::styled(
            format!(" [{mode}] "),
            Style::default()
                .fg(t.color("accent"))
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(
            "{} reload  {} all  {} differences  {} paths  {} help",
            key("reload"),
            key("all"),
            key("differences"),
            key("paths"),
            key("help")
        )),
    ]);
    let second = Line::from(vec![Span::styled(
        format!(
            " [{}] Filter: {}  [{}] Exclude: {}  {ws}",
            key("include"),
            if app.config.include.is_empty() {
                "*"
            } else {
                &app.config.include
            },
            key("exclude"),
            app.config.exclude
        ),
        Style::default().fg(t.color("muted")),
    )]);
    frame.render_widget(Paragraph::new(vec![first, second]), area);
}

fn panel(app: &App, title: impl Into<Line<'static>>, active: bool) -> Block<'static> {
    Block::bordered().title(title).border_style(
        Style::default().fg(app
            .config
            .colors
            .color(if active { "accent" } else { "border" })),
    )
}

fn tree(frame: &mut Frame, app: &mut App, area: Rect) {
    app.mouse_state.tree_pane = area;
    let title = if app.scanning {
        " Folders · comparing… ".to_owned()
    } else {
        format!(
            " Folders{} · {} files{} ",
            if app.focus == Focus::Tree {
                " [active]"
            } else {
                ""
            },
            app.nodes.iter().filter(|n| !n.is_dir).count(),
            if app.tree_query.is_empty() {
                String::new()
            } else {
                format!(" · find: {}", app.tree_query)
            }
        )
    };
    let block = panel(app, title, app.focus == Focus::Tree);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 {
        return;
    }
    let columns = Layout::horizontal([
        Constraint::Percentage(48),
        Constraint::Length(3),
        Constraint::Min(0),
    ])
    .split(inner);
    for side in 0..2 {
        let col = columns[side * 2];
        let path = app
            .roots
            .as_ref()
            .map(|r| r.paths[side].display().to_string())
            .unwrap_or_default();
        frame.render_widget(
            Paragraph::new(format!(" {} · {path}", app.label(side))).style(
                Style::default()
                    .fg(app
                        .config
                        .colors
                        .color(if side == 0 { "logo_lazy" } else { "logo_merge" }))
                    .add_modifier(Modifier::BOLD),
            ),
            Rect { height: 1, ..col },
        );
    }
    app.tree_height = inner.height.saturating_sub(2) as usize;
    app.mouse_state.tree_rows =
        Rect::new(inner.x, inner.y + 1, inner.width, app.tree_height as u16);
    let visible = app.visible();
    let count = visible.len();
    if app.selected < app.tree_scroll {
        app.tree_scroll = app.selected;
    }
    if app.selected >= app.tree_scroll + app.tree_height.max(1) {
        app.tree_scroll = app.selected + 1 - app.tree_height.max(1);
    }
    app.tree_scroll = app.tree_scroll.min(count.saturating_sub(app.tree_height));
    for (screen, &(index, depth)) in visible
        .iter()
        .skip(app.tree_scroll)
        .take(app.tree_height)
        .enumerate()
    {
        let node = &app.nodes[index];
        let y = inner.y + 1 + screen as u16;
        let selected = app.tree_scroll + screen == app.selected;
        let bg = app
            .config
            .colors
            .color(if selected { "selected" } else { "bg" });
        let fg = status_color(app, node.status);
        frame.render_widget(
            Block::default().style(Style::default().bg(bg)),
            Rect::new(inner.x, y, inner.width, 1),
        );
        for side in 0..2 {
            let col = columns[side * 2];
            if !node.present[side] {
                continue;
            }
            let arrow = if node.is_dir {
                if app.expanded.contains(&node.path) || !app.tree_query.is_empty() {
                    "▾ "
                } else {
                    "▸ "
                }
            } else {
                "  "
            };
            let name = format!(
                " {}{arrow}{}{}",
                "  ".repeat(depth),
                node.path.file_name().unwrap_or_default().to_string_lossy(),
                if node.is_dir { "/" } else { "" }
            );
            let size = node.sizes[side].map(size_text).unwrap_or_default();
            let available = col.width.saturating_sub(if col.width > 28 { 9 } else { 0 });
            frame.render_widget(
                Paragraph::new(name).style(Style::default().fg(fg).bg(bg)),
                Rect::new(col.x, y, available, 1),
            );
            if col.width > 28 {
                frame.render_widget(
                    Paragraph::new(size)
                        .alignment(Alignment::Right)
                        .style(Style::default().fg(app.config.colors.color("muted")).bg(bg)),
                    Rect::new(col.x + available, y, col.width - available, 1),
                );
            }
        }
        frame.render_widget(
            Paragraph::new(node.status.mark())
                .alignment(Alignment::Center)
                .style(Style::default().fg(fg).bg(bg)),
            Rect::new(columns[1].x, y, columns[1].width, 1),
        );
    }
    if count == 0 && inner.height > 1 {
        frame.render_widget(
            Paragraph::new(if app.scanning {
                " Comparing folders…"
            } else if app.config.differences_only {
                " No differences (minor formatting changes are hidden)."
            } else {
                " No files match the current filters."
            })
            .style(Style::default().fg(app.config.colors.color("muted"))),
            Rect {
                y: inner.y + 1,
                height: 1,
                ..inner
            },
        );
    }
    if inner.height > 1 {
        let mut counts = [0; 6];
        for n in app.nodes.iter().filter(|n| !n.is_dir) {
            counts[match n.status {
                Status::Different => 0,
                Status::LeftOnly => 1,
                Status::RightOnly => 2,
                Status::Minor => 3,
                Status::Same => 4,
                Status::Error => 5,
            }] += 1;
        }
        let summary = format!(
            " {} different · {} left · {} right · {} minor · {} identical · {} errors",
            counts[0], counts[1], counts[2], counts[3], counts[4], counts[5]
        );
        let error = visible
            .get(app.selected)
            .and_then(|(i, _)| app.nodes[*i].error.as_deref());
        let status = visible
            .get(app.selected)
            .map(|(i, _)| app.nodes[*i].status.label())
            .unwrap_or("");
        let summary = format!("{summary} · {status}");
        frame.render_widget(
            Paragraph::new(error.unwrap_or(&summary)).style(Style::default().fg(
                app.config.colors.color(if error.is_some() {
                    "different"
                } else {
                    "muted"
                }),
            )),
            Rect::new(inner.x, inner.bottom() - 1, inner.width, 1),
        );
    }
}

fn diff(frame: &mut Frame, app: &mut App, area: Rect) {
    app.mouse_state.diff_pane = area;
    let s = app.session.as_ref().unwrap();
    let title = format!(
        " {} · {} sections{} ",
        app.roots.as_ref().map_or_else(
            || s.path.display().to_string(),
            |roots| roots.title(&s.path)
        ),
        s.diff.hunks.len(),
        if app.focus == Focus::Diff {
            " [active]"
        } else {
            ""
        }
    );
    let block = panel(app, title, app.focus == Focus::Diff);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height < 2 {
        return;
    }
    let columns = Layout::horizontal([
        Constraint::Length(1),
        Constraint::Percentage(48),
        Constraint::Length(3),
        Constraint::Min(0),
    ])
    .split(inner);
    for side in 0..2 {
        let doc = &s.docs[side];
        // Without the folder tree, its header with the chosen paths is not shown.
        let path = match &app.roots {
            Some(roots) if roots.files => format!(" · {}", roots.paths[side].display()),
            _ => String::new(),
        };
        let encoding = format!(
            " {} · {} {}{}{}{}{}{path}",
            app.label(side),
            doc.encoding,
            match doc.eol.as_str() {
                "\r\n" => "CRLF",
                "\r" => "CR",
                _ => "LF",
            },
            if doc.bom { " BOM" } else { "" },
            if !doc.exists { " [missing]" } else { "" },
            if doc.dirty() { " [+ unsaved]" } else { "" },
            if s.stale[side] {
                " [changed on disk]"
            } else if doc.readonly {
                " [read-only]"
            } else {
                ""
            }
        );
        frame.render_widget(
            Paragraph::new(encoding).style(Style::default().fg(app.config.colors.color(
                if s.stale[side] {
                    "warning"
                } else if doc.dirty() {
                    "logo_merge"
                } else {
                    "muted"
                },
            ))),
            Rect {
                height: 1,
                ..columns[1 + side * 2]
            },
        );
    }
    app.diff_height = inner.height.saturating_sub(2) as usize;
    let height = app.diff_height.max(1);
    let margin = app.config.scrolloff.min(height.saturating_sub(1) / 2);
    if s.cursor < app.diff_scroll + margin {
        app.diff_scroll = s.cursor.saturating_sub(margin);
    }
    if s.cursor + margin >= app.diff_scroll + height {
        app.diff_scroll = (s.cursor + margin + 1).saturating_sub(height);
    }
    app.diff_scroll = app
        .diff_scroll
        .min(s.diff.rows.len().saturating_sub(height));
    if s.docs.iter().any(|d| d.binary) {
        let message = if app.compares_files() {
            "Binary file — compare is unavailable."
        } else {
            "Binary file — compare is unavailable.\nPress Tab to focus the tree, then Alt+l / Alt+h to copy the whole file (asks first)."
        };
        frame.render_widget(
            Paragraph::new(message)
                .alignment(Alignment::Center)
                .style(Style::default().fg(app.config.colors.color("muted")))
                .wrap(Wrap { trim: true }),
            Rect::new(
                inner.x + 1,
                inner.y + 1,
                inner.width.saturating_sub(2),
                inner.height.saturating_sub(2),
            ),
        );
    } else {
        app.mouse_state.diff_rows = [columns[1], columns[3]]
            .map(|col| Rect::new(col.x, inner.y + 1, col.width, app.diff_height as u16));
        app.mouse_state.diff_map = Rect::new(
            columns[0].x,
            inner.y + 1,
            columns[0].width,
            app.diff_height as u16,
        );
        app.mouse_state.diff_gutter = Rect::new(
            columns[2].x,
            inner.y + 1,
            columns[2].width,
            app.diff_height as u16,
        );
        for (line_index, row) in s
            .diff
            .rows
            .iter()
            .enumerate()
            .skip(app.diff_scroll)
            .take(height)
        {
            let y = inner.y + 1 + (line_index - app.diff_scroll) as u16;
            let cursor = line_index == s.cursor;
            let selected = s.anchor.is_some_and(|anchor| {
                (anchor.min(s.cursor)..=anchor.max(s.cursor)).contains(&line_index)
            });
            for side in 0..2 {
                let col = columns[1 + side * 2];
                let index = if side == 0 { row.left } else { row.right };
                let other = if side == 0 { row.right } else { row.left };
                let bg_name = if selected {
                    "selected"
                } else if row.hunk.is_some() && index.is_some() {
                    if side == 0 { "removed" } else { "added" }
                } else {
                    "bg"
                };
                let bg = app.config.colors.color(bg_name);
                frame.render_widget(
                    Block::default().style(Style::default().bg(bg)),
                    Rect::new(col.x, y, col.width, 1),
                );
                let number = if let Some(i) = index {
                    format!("{:>4} {}", i + 1, if cursor { "›" } else { " " })
                } else {
                    "     ·".into()
                };
                let gutter_width = col.width.min(7);
                frame.render_widget(
                    Paragraph::new(number).style(
                        Style::default()
                            .fg(app
                                .config
                                .colors
                                .color(if cursor { "accent" } else { "muted" }))
                            .bg(if cursor {
                                app.config.colors.color("selected")
                            } else {
                                bg
                            }),
                    ),
                    Rect::new(col.x, y, gutter_width, 1),
                );
                if let Some(index) = index {
                    let text = &s.docs[side].lines[index];
                    let opposite = other.map(|i| s.docs[1 - side].lines[i].as_str());
                    let content = code_line(
                        app,
                        text,
                        opposite,
                        row.hunk.is_some(),
                        SyntaxSource {
                            filename: &s.path.to_string_lossy(),
                            document: Some((s, side, index)),
                        },
                        bg,
                        (
                            s.horizontal,
                            col.width.saturating_sub(gutter_width) as usize,
                        ),
                    );
                    frame.render_widget(
                        Paragraph::new(content),
                        Rect::new(
                            col.x + gutter_width,
                            y,
                            col.width.saturating_sub(gutter_width),
                            1,
                        ),
                    );
                } else if other.is_some() {
                    // A line missing on this side: red diagonal stripes, like the
                    // browser app's hatched gaps.
                    let (stripes, ground) = if selected {
                        ("removed", "selected")
                    } else {
                        ("bg", "removed")
                    };
                    let width = col.width.saturating_sub(gutter_width);
                    frame.render_widget(
                        Paragraph::new("╱".repeat(width as usize)).style(
                            Style::default()
                                .fg(app.config.colors.color(stripes))
                                .bg(app.config.colors.color(ground)),
                        ),
                        Rect::new(col.x + gutter_width, y, width, 1),
                    );
                } else {
                    frame.render_widget(
                        Paragraph::new(if s.docs[side].final_newline {
                            "↵ final newline"
                        } else {
                            "∅ no final newline"
                        })
                        .style(Style::default().fg(app.config.colors.color("accent"))),
                        Rect::new(
                            col.x + gutter_width,
                            y,
                            col.width.saturating_sub(gutter_width),
                            1,
                        ),
                    );
                }
            }
            frame.render_widget(
                Paragraph::new(if row.hunk.is_some() { "←→" } else { "│" })
                    .alignment(Alignment::Center)
                    .style(
                        Style::default().fg(app.config.colors.color(if row.hunk.is_some() {
                            "accent"
                        } else {
                            "border"
                        })),
                    ),
                Rect::new(columns[2].x, y, columns[2].width, 1),
            );
        }
        for screen in 0..height {
            let row = screen * s.diff.rows.len() / height;
            let next = ((screen + 1) * s.diff.rows.len() / height).max(row + 1);
            let changed = s
                .diff
                .hunks
                .iter()
                .any(|h| h.rows.start < next && h.rows.end > row);
            let viewport = row >= app.diff_scroll && row < app.diff_scroll + height;
            frame.render_widget(
                Paragraph::new(if changed {
                    "┃"
                } else if viewport {
                    "│"
                } else {
                    " "
                })
                .style(Style::default().fg(app.config.colors.color(if changed {
                    "accent"
                } else {
                    "border"
                }))),
                Rect::new(columns[0].x, inner.y + 1 + screen as u16, 1, 1),
            );
        }
    }
    let key = |action: &str| {
        app.config
            .keys
            .get(action)
            .map(String::as_str)
            .unwrap_or("")
    };
    let controls = format!(
        " {}/{} section ←/→  {}/{} all  {} delete  {} undo  {} save both{}",
        key("merge_left"),
        key("merge_right"),
        key("copy_left"),
        key("copy_right"),
        key("delete_both"),
        key("undo"),
        key("save_both"),
        if app.compares_files() {
            ""
        } else {
            "  Esc close"
        }
    );
    frame.render_widget(
        Paragraph::new(controls).style(Style::default().fg(app.config.colors.color("muted"))),
        Rect::new(inner.x, inner.bottom() - 1, inner.width, 1),
    );
}

/// A rendered grapheme, retaining its original byte range for highlighting.
struct VisibleGlyph<'a> {
    source: Range<usize>,
    display: Cow<'a, str>,
}

fn visible_glyphs(
    text: &str,
    horizontal: usize,
    width: usize,
    tab_width: usize,
) -> Vec<VisibleGlyph<'_>> {
    if width == 0 {
        return Vec::new();
    }
    let end_column = horizontal.saturating_add(width);
    // ASCII prefixes without tabs have exactly one display cell per byte. Jump
    // over them with the standard library's fast byte scans rather than asking
    // the grapheme iterator to visit millions of offscreen characters.
    let prefix = horizontal.min(text.len());
    let prefix_bytes = &text.as_bytes()[..prefix];
    let mut byte = if prefix_bytes.is_ascii() && !prefix_bytes.contains(&b'\t') {
        prefix
    } else {
        0
    };
    if text
        .as_bytes()
        .get(byte)
        .is_some_and(|byte| !byte.is_ascii())
    {
        // The last ASCII character may begin a combining/keycap cluster.
        byte = byte.saturating_sub(1);
    }
    let mut column = byte;
    let raw = Span::raw(&text[byte..]);
    // Ratatui's grapheme iterator omits control characters. Handle those directly
    // and use the iterator for the remaining clusters, including emoji and accents.
    let mut graphemes = raw.styled_graphemes(Style::default());
    let mut visible = Vec::new();
    while byte < text.len() && column < end_column {
        let first = text[byte..].chars().next().unwrap();
        let (symbol, cells, control) = if first.is_control() {
            let symbol = &text[byte..byte + first.len_utf8()];
            let cells = if first == '\t' {
                tab_width - column % tab_width
            } else {
                1
            };
            (symbol, cells, true)
        } else {
            let Some(grapheme) = graphemes.next() else {
                break;
            };
            (grapheme.symbol, grapheme.symbol.width(), false)
        };
        let source = byte..byte + symbol.len();
        byte = source.end;
        let next_column = column.saturating_add(cells);
        if next_column > horizontal && cells != 0 {
            let left = column.max(horizontal);
            let right = next_column.min(end_column);
            let shown = right.saturating_sub(left);
            let display = if first == '\t' || left != column || right != next_column {
                Cow::Owned(" ".repeat(shown))
            } else if control || symbol.len() > 4096 {
                // A pathological combining cluster can be megabytes long while
                // occupying one cell. Keep rendering bounded without changing data.
                Cow::Owned("�".repeat(shown))
            } else {
                Cow::Borrowed(&text[source.clone()])
            };
            if shown != 0 {
                visible.push(VisibleGlyph { source, display });
            }
        }
        column = next_column;
    }
    visible
}

fn inline_change(text: &str, opposite: Option<&str>, changed: bool) -> Range<usize> {
    if !changed {
        return 0..0;
    }
    let Some(other) = opposite else {
        return 0..text.len();
    };
    let prefix = text
        .chars()
        .zip(other.chars())
        .take_while(|(a, b)| a == b)
        .map(|(ch, _)| ch.len_utf8())
        .sum::<usize>();
    let suffix = text[prefix..]
        .chars()
        .rev()
        .zip(other[prefix..].chars().rev())
        .take_while(|(a, b)| a == b)
        .map(|(ch, _)| ch.len_utf8())
        .sum::<usize>();
    prefix..text.len() - suffix
}

/// Search only the displayed slice and enough context to catch matches crossing
/// either viewport edge. Map lowercase expansions back to original UTF-8 ranges.
fn search_ranges(text: &str, query: &str, visible: Range<usize>) -> Vec<Range<usize>> {
    if query.is_empty() || visible.is_empty() {
        return Vec::new();
    }
    let query = query.to_lowercase();
    let count = query.chars().count();
    let start = text[..visible.start]
        .char_indices()
        .rev()
        .nth(count.saturating_sub(1))
        .map_or(0, |(i, _)| i);
    let end = text[visible.end..]
        .char_indices()
        .nth(count)
        .map_or(text.len(), |(i, _)| visible.end + i);
    let mut folded = String::new();
    let mut originals = Vec::new();
    for (i, ch) in text[start..end].char_indices() {
        for lower in ch.to_lowercase() {
            folded.push(lower);
            originals.push(start + i..start + i + ch.len_utf8());
        }
    }
    let mut matches = Vec::new();
    for (index, (byte, _)) in folded.char_indices().enumerate() {
        if folded[byte..].starts_with(&query) {
            let range = originals[index].start..originals[index + count - 1].end;
            if range.start < visible.end && range.end > visible.start {
                matches.push(range);
            }
        }
    }
    matches
}

struct SyntaxSource<'a> {
    filename: &'a str,
    document: Option<(&'a Session, usize, usize)>,
}

impl<'a> From<&'a str> for SyntaxSource<'a> {
    fn from(filename: &'a str) -> Self {
        Self {
            filename,
            document: None,
        }
    }
}

thread_local! {
    static HIGHLIGHTER: RefCell<HighlightCache> = RefCell::new(HighlightCache::default());
}

fn code_line(
    app: &App,
    text: &str,
    opposite: Option<&str>,
    changed: bool,
    source: SyntaxSource<'_>,
    bg: Color,
    (horizontal, width): (usize, usize),
) -> Line<'static> {
    let glyphs = visible_glyphs(text, horizontal, width, app.config.tab_width);
    let Some(first) = glyphs.first() else {
        return Line::default();
    };
    let visible = first.source.start..glyphs.last().unwrap().source.end;
    let changed = inline_change(text, opposite, changed);
    let matches = search_ranges(text, &app.diff_query, visible.clone());
    let syntax = if let Some((session, side, line)) = source.document {
        HIGHLIGHTER.with(|cache| cache.borrow_mut().ranges(session, side, line, visible))
    } else {
        highlight::single_line(
            text,
            highlight::language(std::path::Path::new(source.filename)),
            visible,
        )
    };
    let theme = &app.config.colors;
    let mut spans: Vec<Span<'static>> = Vec::new();
    for glyph in glyphs {
        let overlaps =
            |range: &Range<usize>| range.start < glyph.source.end && range.end > glyph.source.start;
        let foreground = syntax
            .iter()
            .find(|(range, _)| overlaps(range))
            .map_or("fg", |(_, color)| *color);
        let mut style = Style::default().fg(theme.color(foreground)).bg(bg);
        if overlaps(&changed) {
            style = style.bg(theme.color("changed"));
        }
        if matches.iter().any(overlaps) {
            style = style.bg(theme.color("accent")).fg(theme.color("bg"));
        }
        if let Some(previous) = spans.last_mut().filter(|previous| previous.style == style) {
            previous.content.to_mut().push_str(&glyph.display);
        } else {
            spans.push(Span::styled(glyph.display.into_owned(), style));
        }
    }
    Line::from(spans)
}

fn footer(frame: &mut Frame, app: &App, area: Rect) {
    let t = &app.config.colors;
    let keys = &app.config.keys;
    let hint = if app.prompt.is_some() {
        "Enter accept · Esc cancel · Tab complete paths".to_owned()
    } else if app.editor.is_some() {
        format!(
            "{} apply edit · Alt+s save both · Esc cancel · Enter new line",
            keys["accept_edit"]
        )
    } else if app.focus == Focus::Tree {
        format!(
            "j/k move · Enter open · {}/{} copy ←/→ · Tab switch pane · {} find · {} quit",
            keys["copy_left"], keys["copy_right"], keys["find"], keys["quit"]
        )
    } else {
        format!(
            "j/k move · n/N section · v select · i/I edit · / find{} · {} quit",
            if app.compares_files() {
                ""
            } else {
                " · Tab tree"
            },
            keys["quit"]
        )
    };
    if let Some(notice) = &app.file_change_notice {
        let height = if area.height >= 4 {
            area.height - 1
        } else {
            area.height
        };
        let box_area = Rect::new(area.x, area.y, area.width, height);
        let block = Block::bordered()
            .title(" Files changed ")
            .border_style(Style::default().fg(t.color("warning")));
        let inner = block.inner(box_area);
        frame.render_widget(block, box_area);
        frame.render_widget(
            Paragraph::new(notice.as_str()).wrap(Wrap { trim: false }),
            inner,
        );
        if area.height > height {
            let status = if app.message.is_empty() || app.message == *notice {
                hint
            } else {
                format!("{} · {hint}", app.message)
            };
            frame.render_widget(
                Paragraph::new(format!(" {status}")).style(Style::default().fg(t.color("muted"))),
                Rect::new(area.x, area.bottom() - 1, area.width, 1),
            );
        }
        return;
    }
    frame.render_widget(
        Paragraph::new(vec![
            Line::styled(
                format!(" {}", app.message),
                Style::default().fg(t.color("accent")),
            ),
            Line::styled(format!(" {hint}"), Style::default().fg(t.color("muted"))),
        ]),
        area,
    );
}

fn popup(frame: &mut Frame, app: &App, area: Rect, title: &str) -> Rect {
    frame.render_widget(Clear, area);
    let block = Block::bordered()
        .title(format!(" {title} "))
        .style(
            Style::default()
                .fg(app.config.colors.color("fg"))
                .bg(app.config.colors.color("bg")),
        )
        .border_style(Style::default().fg(app.config.colors.color("accent")));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    inner
}

fn prompt(frame: &mut Frame, app: &App) {
    let p = app.prompt.as_ref().unwrap();
    let (title, path) = match p.kind {
        PromptKind::Path(0) => ("Left folder or file · step 1 of 2", true),
        PromptKind::Path(_) => ("Right folder or file · step 2 of 2", true),
        PromptKind::FindTree => ("Find file name", false),
        PromptKind::FindDiff => (
            "Find text · Enter next · Shift+Enter previous · Esc return",
            false,
        ),
        PromptKind::Include => (
            "Include patterns · e.g. *.rs;*.json · empty includes all",
            false,
        ),
        PromptKind::Exclude => ("Exclude patterns · semicolon or comma separated", false),
    };
    let available = modal_space(app, frame.area());
    let area = centered(
        available,
        frame.area().width.saturating_sub(8).min(100),
        if path { 13 } else { 6 },
    );
    let inner = popup(frame, app, area, title);
    let cursor_width = p.input.value[..p.input.cursor].width();
    let scroll = cursor_width.saturating_sub(inner.width.saturating_sub(3) as usize);
    frame.render_widget(
        Paragraph::new(format!("> {}", p.input.value))
            .scroll((0, scroll.min(u16::MAX as usize) as u16)),
        Rect { height: 1, ..inner },
    );
    if inner.width > 2 {
        frame.set_cursor_position((
            inner.x
                + 2
                + (cursor_width - scroll).min(inner.width.saturating_sub(3) as usize) as u16,
            inner.y,
        ));
    }
    if path && inner.height > 2 {
        let mut preview = p.input.clone();
        if preview.completions.is_empty() {
            let files = matches!(p.kind, PromptKind::Path(side) if app.path_completes_files(side));
            let _ = preview.complete(false, files);
        }
        let candidates = preview
            .completions
            .iter()
            .enumerate()
            .take(inner.height.saturating_sub(4) as usize)
            .map(|(i, c)| {
                Line::styled(
                    format!("  {c}"),
                    Style::default().fg(app.config.colors.color(
                        if p.input.completion_index == Some(i) {
                            "accent"
                        } else {
                            "muted"
                        },
                    )),
                )
            })
            .collect::<Vec<_>>();
        frame.render_widget(
            Paragraph::new(candidates),
            Rect::new(
                inner.x,
                inner.y + 2,
                inner.width,
                inner.height.saturating_sub(4),
            ),
        );
    }
    if inner.height > 1 {
        let msg = if p.error.is_empty() {
            if path {
                "Tab / Shift+Tab cycle matches · type to descend · Enter confirm · Esc cancel"
            } else {
                "Enter apply · Esc cancel"
            }
        } else {
            &p.error
        };
        frame.render_widget(
            Paragraph::new(msg)
                .style(
                    Style::default().fg(app.config.colors.color(if p.error.is_empty() {
                        "muted"
                    } else {
                        "different"
                    })),
                )
                .wrap(Wrap { trim: false }),
            Rect::new(
                inner.x,
                inner.bottom().saturating_sub(2),
                inner.width,
                2.min(inner.height),
            ),
        );
    }
}

fn confirmation(frame: &mut Frame, app: &mut App) {
    let dialog = app.confirmation.as_ref().unwrap();
    let area = centered(modal_space(app, frame.area()), 78, 8);
    let inner = popup(frame, app, area, "Confirm");
    frame.render_widget(
        Paragraph::new(dialog.message.as_str()).wrap(Wrap { trim: false }),
        Rect {
            height: inner.height.saturating_sub(2),
            ..inner
        },
    );
    let buttons = Line::from(vec![
        Span::styled(
            " [No / Esc] ",
            Style::default().bg(app.config.colors.color(if !dialog.yes {
                "selected"
            } else {
                "bg"
            })),
        ),
        Span::raw("   "),
        Span::styled(
            " [Yes / y] ",
            Style::default().bg(app.config.colors.color(if dialog.yes {
                "selected"
            } else {
                "bg"
            })),
        ),
        Span::styled(
            "   Tab choose · Enter accept",
            Style::default().fg(app.config.colors.color("muted")),
        ),
    ]);
    if inner.height > 0 {
        app.mouse_state.confirm_no = Rect::new(inner.x, inner.bottom() - 1, inner.width.min(12), 1);
        app.mouse_state.confirm_yes = Rect::new(
            inner.x + inner.width.min(15),
            inner.bottom() - 1,
            inner.width.saturating_sub(15).min(11),
            1,
        );
        frame.render_widget(
            Paragraph::new(buttons),
            Rect::new(inner.x, inner.bottom() - 1, inner.width, 1),
        );
    }
}

fn editor(frame: &mut Frame, app: &mut App) {
    let title = {
        let e = app.editor.as_ref().unwrap();
        format!(
            "Edit {} · {} apply · Esc cancel",
            app.label(e.side),
            app.config.keys["accept_edit"]
        )
    };
    let area = centered(
        modal_space(app, frame.area()),
        frame.area().width.saturating_sub(6),
        frame.area().height.saturating_sub(6),
    );
    let inner = popup(frame, app, area, &title);
    let e = app.editor.as_mut().unwrap();
    if e.row < e.scroll {
        e.scroll = e.row;
    }
    if e.row >= e.scroll + inner.height as usize {
        e.scroll = (e.row + 1).saturating_sub(inner.height as usize);
    }
    let cursor_width = e.lines[e.row].value[..e.lines[e.row].cursor]
        .split('\t')
        .enumerate()
        .fold(0, |column, (index, segment)| {
            let start = if index == 0 {
                column
            } else {
                column + app.config.tab_width - column % app.config.tab_width
            };
            start + segment.width()
        });
    let horizontal = cursor_width.saturating_sub(inner.width.saturating_sub(2) as usize);
    app.mouse_state.editor = Some(crate::mouse::EditorHit {
        area: inner,
        horizontal,
    });
    let lines: Vec<Line> = e
        .lines
        .iter()
        .skip(e.scroll)
        .take(inner.height as usize)
        .map(|l| {
            Line::raw(
                visible_glyphs(
                    &l.value,
                    horizontal,
                    inner.width as usize,
                    app.config.tab_width,
                )
                .into_iter()
                .map(|glyph| glyph.display)
                .collect::<String>(),
            )
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
    if inner.width > 0 && inner.height > 0 {
        frame.set_cursor_position((
            inner.x
                + (cursor_width - horizontal).min(inner.width.saturating_sub(1) as usize) as u16,
            inner.y + (e.row - e.scroll) as u16,
        ));
    }
}

fn help(frame: &mut Frame, app: &App) {
    let area = centered(
        modal_space(app, frame.area()),
        100,
        frame.area().height.saturating_sub(4),
    );
    let inner = popup(frame, app, area, "LazyMerge keys · ↑/↓ scroll · Esc close");
    let mut lines = vec![
        Line::styled(
            "Edits stay in memory until saved. Copies write after confirmation.",
            Style::default().fg(app.config.colors.color("accent")),
        ),
        Line::raw(""),
        Line::raw(
            "Folders: Enter open · arrows navigate · gg/G top/bottom · PageUp/PageDown scroll",
        ),
        Line::raw("Diff: arrows navigate/scroll · gg/G top/bottom · zz cursor row to the middle"),
        Line::raw("Diff: Ctrl+d/u half a page down/up, then the cursor row in the middle"),
        Line::raw("Diff: L/H merge section or selected lines · Alt+l/h merge all"),
        Line::raw(
            "Diff: dd delete the line or selected lines on both sides · dh/dl left/right · u undo",
        ),
        Line::raw("Two files (lazymerge LEFT_FILE RIGHT_FILE) open without the folder tree"),
        Line::raw("Editor: Alt+Enter apply · Alt+s save both · Esc cancel · Enter new line"),
        Line::raw("Prompts: Tab / Shift+Tab complete paths · Ctrl+u clear · Ctrl+w word"),
        Line::raw("Confirmations: y accept · n/Esc cancel · Tab choose · Enter accept"),
        Line::raw("Ctrl+C asks before quitting, including in path entry and the editor."),
        Line::raw(""),
        Line::styled(
            "Configured bindings",
            Style::default().add_modifier(Modifier::BOLD),
        ),
    ];
    for (action, binding) in &app.config.keys {
        lines.push(Line::raw(format!(
            "  {:<16} {}",
            if binding.is_empty() {
                "(disabled)"
            } else {
                binding
            },
            action.replace('_', " ")
        )));
    }
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .scroll((app.help_scroll, 0))
            .wrap(Wrap { trim: false }),
        inner,
    );
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}

fn status_color(app: &App, status: Status) -> Color {
    app.config.colors.color(match status {
        Status::Same => "muted",
        Status::Minor => "minor",
        Status::Different | Status::Error => "different",
        Status::LeftOnly | Status::RightOnly => "one_sided",
    })
}

fn size_text(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MiB", bytes as f64 / 1048576.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::Config,
        model::{Document, Session},
        storage::Roots,
    };
    use ratatui::{Terminal, backend::TestBackend, crossterm::event::KeyCode};
    use std::path::PathBuf;

    #[test]
    fn screens_render_at_multiple_sizes_with_a_persistent_brand_and_confirmation() {
        let mut app = App::new(Config::default(), None, None).unwrap();
        for (w, h) in [(120, 40), (80, 24), (40, 12), (20, 5)] {
            let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
            terminal.draw(|f| draw(f, &mut app)).unwrap();
            let buffer = terminal.backend().buffer();
            let text = buffer
                .content()
                .iter()
                .map(|c| c.symbol())
                .collect::<String>();
            assert!(text.contains("Lazy"), "brand missing at {w}x{h}: {text}");
        }
        app.key(ratatui::crossterm::event::KeyEvent::new(
            KeyCode::Char('c'),
            ratatui::crossterm::event::KeyModifiers::CONTROL,
        ))
        .unwrap();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        assert!(
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|c| c.symbol())
                .collect::<String>()
                .contains("Quit LazyMerge?")
        );
    }

    #[test]
    fn updated_ascii_logo_is_centered_as_one_complete_block() {
        let mut app = App::new(Config::default(), None, None).unwrap();
        app.prompt = None;
        let rows: Vec<_> = include_str!("logo.txt").lines().collect();
        let art_width = rows.iter().map(|row| row.width()).max().unwrap();
        for width in [52, 53, 80, 121] {
            let mut terminal = Terminal::new(TestBackend::new(width, 40)).unwrap();
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            let buffer = terminal.backend().buffer();
            let left = (width as usize - art_width) / 2;
            for (y, line) in rows.iter().enumerate() {
                let actual = (0..width)
                    .map(|x| buffer[(x, y as u16)].symbol())
                    .collect::<String>();
                let expected = format!(
                    "{}{:art_width$}{}",
                    " ".repeat(left),
                    line,
                    " ".repeat(width as usize - left - art_width)
                );
                assert_eq!(actual, expected, "logo row {y} at width {width}");
            }
            assert_eq!(
                buffer[(left as u16 + 1, 0)].fg,
                app.config.colors.color("logo_lazy")
            );
            assert_eq!(
                buffer[(left as u16 + 22, 0)].fg,
                app.config.colors.color("logo_merge")
            );
        }
    }

    #[test]
    fn compact_logo_is_centered_when_the_art_cannot_fit() {
        let mut app = App::new(Config::default(), None, None).unwrap();
        app.prompt = None;
        let mut terminal = Terminal::new(TestBackend::new(32, 12)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        let line = (0..32).map(|x| buffer[(x, 0)].symbol()).collect::<String>();
        assert_eq!(line.trim(), "LazyMerge");
        let left = line.len() - line.trim_start().len();
        let right = line.len() - line.trim_end().len();
        assert!(
            left.abs_diff(right) <= 1,
            "compact logo is not centered: {line:?}"
        );
    }

    #[test]
    fn file_change_notice_keeps_its_kanagawa_orange_border() {
        let mut app = App::new(Config::default(), None, None).unwrap();
        app.prompt = None;
        app.file_change_notice = Some("File changed on disk · staged edits kept".into());
        app.message = "An unrelated status update".into();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, 20)].symbol(), "┌");
        assert_eq!(buffer[(0, 20)].fg, Color::Rgb(255, 160, 102));
        let text = buffer
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("Files changed"));
        assert!(text.contains("staged edits kept"));
        assert!(text.contains("An unrelated status update"));
    }

    #[test]
    fn diff_render_handles_unicode_and_eof_difference() {
        let mut app = App::new(Config::default(), None, None).unwrap();
        app.prompt = None;
        app.roots = Some(Roots {
            paths: [PathBuf::from("/left"), PathBuf::from("/right")],
            files: false,
        });
        app.session = Some(Session::new(
            PathBuf::from("demo.xml"),
            [
                Document::from_bytes(Some("<t>ä界</t>\n".as_bytes().to_vec())),
                Document::from_bytes(Some("<t>ä界</t>".as_bytes().to_vec())),
            ],
            false,
        ));
        app.focus = Focus::Diff;
        let mut terminal = Terminal::new(TestBackend::new(100, 32)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(text.contains("final newline"));
        assert!(text.contains("demo.xml"));
        assert!(!text.contains('╱'), "no line is missing on either side");
    }

    #[test]
    fn missing_lines_are_hatched_red_like_the_browser_app() {
        let mut app = App::new(Config::default(), None, None).unwrap();
        app.prompt = None;
        app.roots = Some(Roots {
            paths: [PathBuf::from("/left"), PathBuf::from("/right")],
            files: false,
        });
        app.session = Some(Session::new(
            PathBuf::from("gap.txt"),
            [
                Document::from_bytes(Some(b"same\ndeleted\nend\n".to_vec())),
                Document::from_bytes(Some(b"same\nend\n".to_vec())),
            ],
            false,
        ));
        app.focus = Focus::Diff;
        let mut terminal = Terminal::new(TestBackend::new(100, 32)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        let hatched: Vec<_> = buffer
            .content()
            .iter()
            .filter(|cell| cell.symbol() == "╱")
            .collect();
        assert!(
            hatched.len() > 20,
            "the gap right of \"deleted\" is hatched"
        );
        // The browser app's colours: stripes of the background on the removed red.
        assert!(hatched.iter().all(|cell| {
            cell.fg == app.config.colors.color("bg")
                && cell.bg == app.config.colors.color("removed")
        }));
        let row = (0..32)
            .find(|&y| (0..100).any(|x| buffer[(x, y)].symbol() == "╱"))
            .unwrap();
        let line = (0..100)
            .map(|x| buffer[(x, row)].symbol())
            .collect::<String>();
        assert!(line.contains("deleted"), "{line}");
    }

    fn screen(app: &mut App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn two_files_fill_the_body_without_a_folder_tree() {
        let mut app = App::new(Config::default(), None, None).unwrap();
        app.prompt = None;
        app.roots = Some(Roots {
            paths: [
                PathBuf::from("/left/old name.rs"),
                PathBuf::from("/right/new name.rs"),
            ],
            files: true,
        });
        app.session = Some(Session::new(
            PathBuf::from("old name.rs"),
            [
                Document::from_bytes(Some(b"fn main() {}\nleft\n".to_vec())),
                Document::from_bytes(Some(b"fn main() {}\nright\n".to_vec())),
            ],
            false,
        ));
        app.focus = Focus::Diff;
        let text = screen(&mut app, 120, 32);
        assert!(
            text.contains("old name.rs ↔ new name.rs · 1 sections"),
            "{text}"
        );
        assert!(text.contains("/left/old name.rs") && text.contains("/right/new name.rs"));
        assert!(text.contains("[Two files]"));
        assert!(!text.contains("Folders"), "{text}");
        assert!(!text.contains("Filter"), "{text}");
        assert!(
            !text.contains("Tab tree") && !text.contains("Esc close"),
            "{text}"
        );
        assert!(text.contains("dd delete"), "{text}");
        assert!(app.mouse_state.tree_pane.area() == 0);
    }

    #[test]
    fn zz_keeps_the_cursor_row_in_the_middle_after_drawing() {
        let mut app = App::new(Config::default(), None, None).unwrap();
        app.prompt = None;
        let text = (0..200).map(|i| format!("row {i}\n")).collect::<String>();
        app.session = Some(Session::new(
            PathBuf::from("rows.txt"),
            [
                Document::from_bytes(Some(text.as_bytes().to_vec())),
                Document::from_bytes(Some(text.as_bytes().to_vec())),
            ],
            false,
        ));
        app.roots = Some(Roots {
            paths: [PathBuf::from("/left"), PathBuf::from("/right")],
            files: false,
        });
        app.focus = Focus::Diff;
        screen(&mut app, 100, 40);
        app.session.as_mut().unwrap().cursor = 100;
        screen(&mut app, 100, 40);
        let margin = app.config.scrolloff.min((app.diff_height - 1) / 2);
        assert_eq!(
            app.diff_scroll + app.diff_height - 1 - margin,
            100,
            "moving keeps only the scrolloff margin"
        );
        app.key(KeyCode::Char('z').into()).unwrap();
        app.key(KeyCode::Char('z').into()).unwrap();
        screen(&mut app, 100, 40);
        assert_eq!(
            100 - app.diff_scroll,
            (app.diff_height - 1) / 2,
            "the cursor row is in the middle"
        );
        app.key(ratatui::crossterm::event::KeyEvent::new(
            KeyCode::Char('d'),
            ratatui::crossterm::event::KeyModifiers::CONTROL,
        ))
        .unwrap();
        screen(&mut app, 100, 40);
        let cursor = app.session.as_ref().unwrap().cursor;
        assert_eq!(cursor, 100 + app.diff_height / 2);
        assert_eq!(cursor - app.diff_scroll, (app.diff_height - 1) / 2);
    }

    #[test]
    fn editor_displays_literal_tabs_at_configured_stops() {
        let mut app = App::new(Config::default(), None, None).unwrap();
        app.prompt = None;
        app.session = Some(Session::new(
            "tab.txt".into(),
            [
                Document::from_bytes(Some(b"a\tb\n".to_vec())),
                Document::from_bytes(Some(b"a\tb\n".to_vec())),
            ],
            false,
        ));
        app.focus = Focus::Diff;
        app.key(KeyCode::Char('i').into()).unwrap();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(text.contains("a   b"));
        assert_eq!(app.editor.as_ref().unwrap().lines[0].value, "a\tb");
    }

    fn text_of(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    fn render_line(app: &App, text: &str, horizontal: usize, width: usize) -> Line<'static> {
        code_line(
            app,
            text,
            None,
            false,
            "sample.txt".into(),
            app.config.colors.color("bg"),
            (horizontal, width),
        )
    }

    #[test]
    fn long_lines_allocate_only_visible_text_and_coalesce_styles() {
        let app = App::new(Config::default(), None, None).unwrap();
        let text = "x".repeat(4 * 1024 * 1024);
        for horizontal in [0, 1_000_000, text.len() - 10, text.len() + 10] {
            let line = render_line(&app, &text, horizontal, 80);
            assert!(line.width() <= 80);
            assert!(text_of(&line).len() <= 80);
            assert!(line.spans.len() <= 1);
        }
        let zero = render_line(&app, &text, 0, 0);
        assert!(zero.spans.is_empty());
    }

    #[test]
    fn clipping_preserves_tab_stops_wide_graphemes_and_combining_accents() {
        let app = App::new(Config::default(), None, None).unwrap();
        assert_eq!(text_of(&render_line(&app, "a界b\tcé", 2, 5)), " b   ");
        assert_eq!(text_of(&render_line(&app, "a界b\tcé", 1, 2)), "界");
        assert_eq!(
            text_of(&render_line(&app, "e\u{301}界x", 0, 3)),
            "e\u{301}界"
        );
        assert_eq!(text_of(&render_line(&app, "e\u{301}界x", 2, 2)), " x");
        assert_eq!(text_of(&render_line(&app, "👨‍👩‍👧‍👦x", 1, 2)), " x");
        assert_eq!(text_of(&render_line(&app, "a\tb\u{1b}c", 0, 8)), "a   b�c");
    }

    #[test]
    fn search_handles_case_expansion_overlaps_and_offscreen_match_starts() {
        let expanded = search_ranges("İx", "i", 0..2);
        assert_eq!(expanded.len(), 1);
        assert_eq!(expanded[0], 0..2);
        assert_eq!(search_ranges("aaa", "aa", 0..3), [0..2, 1..3]);
        let across_edge = search_ranges("before needle after", "needle", 10..14);
        assert_eq!(across_edge.len(), 1);
        assert_eq!(across_edge[0], 7..13);
        let mut app = App::new(Config::default(), None, None).unwrap();
        app.diff_query = "needle".into();
        let line = render_line(&app, "before needle after", 10, 4);
        assert_eq!(text_of(&line), "dle ");
        assert_eq!(line.spans[0].content.as_ref(), "dle");
        assert_eq!(
            line.spans[0].style.bg,
            Some(app.config.colors.color("accent"))
        );
    }

    #[test]
    fn inline_and_syntax_highlights_keep_utf8_boundaries_and_close_quotes() {
        assert_eq!(inline_change("same界newé", Some("same界oldé"), true), 7..10);
        let source = "'it''s' + 12 // comment";
        let tokens = highlight::single_line(source, highlight::Language::Al, 0..source.len());
        assert_eq!(&source[tokens[0].0.clone()], "'it''s'");
        assert_eq!(tokens[0].1, "logo_merge");
        assert_eq!(tokens.last().unwrap().1, "muted");
        let source = "<!-- comment --> <t name='value'>text</t>";
        let tokens = highlight::single_line(source, highlight::Language::Xml, 0..source.len());
        assert_eq!(&source[tokens[0].0.clone()], "<!-- comment -->");
        assert!(
            tokens
                .iter()
                .any(|(range, color)| &source[range.clone()] == "name" && *color == "accent")
        );
        assert!(tokens.iter().any(|(range, color)| &source[range.clone()] == "'value'" && *color == "logo_merge"));
        let app = App::new(Config::default(), None, None).unwrap();
        let line = code_line(
            &app,
            "same界newé",
            Some("same界oldé"),
            true,
            "file.txt".into(),
            app.config.colors.color("removed"),
            (5, 4),
        );
        assert_eq!(text_of(&line), " new");
        assert_eq!(
            line.spans.last().unwrap().style.bg,
            Some(app.config.colors.color("changed"))
        );
    }

    #[test]
    fn cached_syntax_keeps_diff_and_search_backgrounds() {
        let mut app = App::new(Config::default(), None, None).unwrap();
        let session = Session::new(
            "sample.rs".into(),
            [
                Document::from_bytes(Some(b"/* open\nstill comment\n*/\n".to_vec())),
                Document::from_bytes(None),
            ],
            false,
        );
        let source = || SyntaxSource {
            filename: "sample.rs",
            document: Some((&session, 0, 1)),
        };
        let line = code_line(
            &app,
            "still comment",
            None,
            false,
            source(),
            app.config.colors.color("removed"),
            (0, 30),
        );
        assert_eq!(
            line.spans[0].style.fg,
            Some(app.config.colors.color("muted"))
        );
        assert_eq!(
            line.spans[0].style.bg,
            Some(app.config.colors.color("removed"))
        );
        app.diff_query = "comment".into();
        let line = code_line(
            &app,
            "still comment",
            None,
            false,
            source(),
            app.config.colors.color("removed"),
            (0, 30),
        );
        assert!(
            line.spans
                .iter()
                .any(|span| span.content.as_ref() == "comment"
                    && span.style.bg == Some(app.config.colors.color("accent")))
        );
    }
}
