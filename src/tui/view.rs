use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
enum WidthMode {
    Wide,
    Medium,
    Compact,
}

struct UiLayout {
    mode: WidthMode,
    content: Rect,
    header: Rect,
    body: Rect,
    footer: Rect,
    files: Rect,
    keys: Rect,
    details: Rect,
}

fn rect(x: u16, y: u16, width: u16, height: u16) -> Rect {
    Rect::new(x, y, width, height)
}

fn split_vertical(area: Rect, top: u16) -> (Rect, Rect) {
    let top = top.min(area.height);
    (
        rect(area.x, area.y, area.width, top),
        rect(area.x, area.y + top, area.width, area.height - top),
    )
}

fn layout(screen: Rect, pane: Pane, key_count: usize) -> Option<UiLayout> {
    if screen.width < 50 || screen.height < 16 {
        return None;
    }
    let content = content_area(screen);
    let (header, remaining) = split_vertical(content, 3);
    let body_height = remaining.height.saturating_sub(3);
    let body = rect(remaining.x, remaining.y, remaining.width, body_height);
    let footer = rect(
        remaining.x,
        body.bottom(),
        remaining.width,
        remaining.height - body_height,
    );
    let mode = if screen.height < 22 || screen.width < 72 {
        WidthMode::Compact
    } else if screen.width < 110 {
        WidthMode::Medium
    } else {
        WidthMode::Wide
    };
    let mut files = rect(0, 0, 0, 0);
    let mut keys = files;
    let mut details = files;
    match mode {
        WidthMode::Wide => {
            let file_width = body.width * 30 / 100;
            let key_width = body.width * 30 / 100;
            files = rect(body.x, body.y, file_width, body.height);
            keys = rect(files.right() + 2, body.y, key_width, body.height);
            details = rect(
                keys.right() + 2,
                body.y,
                body.right() - keys.right() - 2,
                body.height,
            );
        }
        WidthMode::Medium => {
            let file_width = (body.width / 3).clamp(24, 32);
            files = rect(body.x, body.y, file_width, body.height);
            let right = rect(
                files.right() + 2,
                body.y,
                body.right() - files.right() - 2,
                body.height,
            );
            let max_keys = body.height.saturating_sub(9);
            let key_height = ((key_count as u16).saturating_add(2)).clamp(5, max_keys.max(5));
            keys = rect(right.x, right.y, right.width, key_height);
            details = rect(
                right.x,
                keys.bottom() + 1,
                right.width,
                right.bottom() - keys.bottom() - 1,
            );
        }
        WidthMode::Compact => match pane {
            Pane::Files => files = body,
            Pane::Keys => keys = body,
            Pane::Details => details = body,
        },
    }
    Some(UiLayout {
        mode,
        content,
        header,
        body,
        footer,
        files,
        keys,
        details,
    })
}

fn line(frame: &mut Frame, area: Rect, y: u16, text: impl Into<Line<'static>>, style: Style) {
    if area.width == 0 || y >= area.bottom() {
        return;
    }
    frame.render_widget(
        Paragraph::new(text.into()).style(style),
        rect(area.x, y, area.width, 1),
    );
}

fn label(text: &str, width: usize) -> String {
    let len = text.chars().count();
    if len <= width {
        return text.to_owned();
    }
    if width <= 3 {
        return ".".repeat(width);
    }
    let head = (width - 1) / 2;
    let tail = width - 1 - head;
    let start: String = text.chars().take(head).collect();
    let end: String = text.chars().skip(len - tail).collect();
    format!("{start}…{end}")
}

#[allow(clippy::too_many_arguments)]
fn button(
    frame: &mut Frame,
    hits: &mut HitAreas,
    area: Rect,
    y: u16,
    x: u16,
    title: &str,
    click: Click,
    selected: bool,
    theme: &Theme,
) {
    let width = title.chars().count() as u16;
    if y >= area.bottom() || x >= area.right() {
        return;
    }
    let width = width.min(area.right() - x);
    let box_area = rect(x, y, width, 1);
    frame.render_widget(
        Paragraph::new(title).style(
            Style::default()
                .fg(if selected { theme.accent } else { Color::Reset })
                .add_modifier(if selected {
                    Modifier::BOLD | Modifier::UNDERLINED
                } else {
                    Modifier::empty()
                }),
        ),
        box_area,
    );
    hits.clicks.push((box_area, click));
}

fn heading(frame: &mut Frame, area: Rect, title: &str, active: bool, theme: &Theme) {
    let text = if active {
        format!("▸ {title}")
    } else {
        format!("  {title}")
    };
    line(
        frame,
        area,
        area.y,
        text,
        Style::default()
            .fg(if active { theme.accent } else { theme.muted })
            .add_modifier(Modifier::BOLD),
    );
}

fn draw_header(frame: &mut Frame, app: &App, ui: &UiLayout, hits: &mut HitAreas, theme: &Theme) {
    let project = app.project.as_ref().map_or("projects", |p| p.name.as_str());
    let worktree = app
        .repo
        .as_ref()
        .and_then(|repo| repo.root.file_name())
        .and_then(|name| name.to_str())
        .unwrap_or("store");
    let title = if ui.mode == WidthMode::Compact {
        format!(
            "envrun / {project}  [{}]",
            match app.pane() {
                Pane::Files => "Files",
                Pane::Keys => "Keys",
                Pane::Details => "Details",
            }
        )
    } else {
        format!("envrun / {project}     worktree: {worktree}")
    };
    line(
        frame,
        ui.header,
        ui.header.y,
        title,
        Style::default().add_modifier(Modifier::BOLD),
    );
    button(
        frame,
        hits,
        ui.header,
        ui.header.y,
        ui.header.right().saturating_sub(7),
        "? help",
        Click::Help,
        false,
        theme,
    );
    if let Some(path) = app.selected_file() {
        let detail = format!(
            "{}  {}  {}",
            path.display(),
            app.selected_status(),
            app.source_label()
        );
        line(
            frame,
            ui.header,
            ui.header.y + 1,
            label(&detail, ui.header.width.saturating_sub(12) as usize),
            Style::default().fg(theme.muted),
        );
    } else {
        line(
            frame,
            ui.header,
            ui.header.y + 1,
            "Select a file",
            Style::default().fg(theme.muted),
        );
    }
    button(
        frame,
        hits,
        ui.header,
        ui.header.y + 1,
        ui.header.right().saturating_sub(10),
        "F5 refresh",
        Click::Refresh,
        false,
        theme,
    );
    line(
        frame,
        ui.header,
        ui.header.y + 2,
        "─".repeat(ui.header.width as usize),
        Style::default().fg(theme.muted),
    );
}

fn draw_projects(frame: &mut Frame, app: &App, area: Rect, hits: &mut HitAreas, theme: &Theme) {
    heading(
        frame,
        area,
        &format!("PROJECTS  {}", app.available.len()),
        true,
        theme,
    );
    let list_area = rect(
        area.x,
        area.y + 1,
        area.width,
        area.height.saturating_sub(1),
    );
    if app.available.is_empty() {
        line(
            frame,
            list_area,
            list_area.y + 2,
            "No project opened. n New project",
            Style::default(),
        );
        return;
    }
    let items: Vec<_> = app
        .available
        .iter()
        .map(|project| ListItem::new(project.name.clone()))
        .collect();
    let mut state = ListState::default().with_selected(Some(app.project_index));
    frame.render_stateful_widget(
        List::new(items)
            .highlight_symbol("› ")
            .highlight_style(Style::default().add_modifier(Modifier::BOLD)),
        list_area,
        &mut state,
    );
    hits.files = list_area;
    hits.file_offset = state.offset();
}

fn draw_files(frame: &mut Frame, app: &App, area: Rect, hits: &mut HitAreas, theme: &Theme) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let files = app.visible_files();
    heading(
        frame,
        area,
        &format!("FILES  {}", files.len()),
        app.pane() == Pane::Files,
        theme,
    );
    let actions_height = 3.min(area.height.saturating_sub(2));
    let list_area = rect(
        area.x,
        area.y + 1,
        area.width,
        area.height.saturating_sub(1 + actions_height),
    );
    if files.is_empty() {
        let message = if !app.filter.is_empty() {
            format!("No files match '{}'. Esc clears", app.filter)
        } else {
            "No env files yet. i Import or a Add".into()
        };
        line(
            frame,
            list_area,
            list_area.y + 1,
            label(&message, area.width as usize),
            Style::default().fg(theme.muted),
        );
    } else {
        let items: Vec<_> = files
            .iter()
            .map(|path| {
                let status = app.file_labels.get(path).copied().unwrap_or("LOCAL");
                let width = area.width.saturating_sub(status.len() as u16 + 4) as usize;
                ListItem::new(format!(
                    "{:<width$} {status}",
                    label(&path.display().to_string(), width)
                ))
            })
            .collect();
        let mut state = ListState::default().with_selected(Some(app.file_index));
        frame.render_stateful_widget(
            List::new(items).highlight_symbol("› ").highlight_style(
                Style::default()
                    .fg(if app.pane() == Pane::Files {
                        theme.accent
                    } else {
                        Color::Reset
                    })
                    .add_modifier(Modifier::BOLD),
            ),
            list_area,
            &mut state,
        );
        hits.files = list_area;
        hits.file_offset = state.offset();
    }
    let start = area.bottom() - actions_height;
    button(
        frame,
        hits,
        area,
        start,
        area.x,
        "a Add",
        Click::Key('a'),
        false,
        theme,
    );
    button(
        frame,
        hits,
        area,
        start,
        area.x + 8,
        "i Import",
        Click::Key('i'),
        false,
        theme,
    );
    button(
        frame,
        hits,
        area,
        start,
        area.x + 18,
        if app.selected_status() == "BOTH" {
            "l Relink"
        } else {
            "l Link"
        },
        Click::Key('l'),
        false,
        theme,
    );
    button(
        frame,
        hits,
        area,
        start + 1,
        area.x,
        "u Unlink",
        Click::Key('u'),
        false,
        theme,
    );
    button(
        frame,
        hits,
        area,
        start + 1,
        area.x + 11,
        "d Remove",
        Click::Key('d'),
        false,
        theme,
    );
    button(
        frame,
        hits,
        area,
        start + 2,
        area.x,
        "/ Filter",
        Click::Key('/'),
        false,
        theme,
    );
}

fn draw_keys(
    frame: &mut Frame,
    app: &App,
    area: Rect,
    hits: &mut HitAreas,
    theme: &Theme,
    compact: bool,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    heading(
        frame,
        area,
        &format!("KEYS  {}", app.keys.len()),
        app.pane() == Pane::Keys,
        theme,
    );
    let preview_height = if compact && app.file_error.is_none() && !app.keys.is_empty() {
        3.min(area.height.saturating_sub(3))
    } else {
        0
    };
    let list_area = rect(
        area.x,
        area.y + 1,
        area.width,
        area.height.saturating_sub(1 + preview_height),
    );
    if app.selected_file().is_none() {
        line(
            frame,
            list_area,
            list_area.y + 1,
            "Select an env file",
            Style::default().fg(theme.muted),
        );
    } else if app.selected_status() == "BOTH" && app.source == Source::Auto {
        line(
            frame,
            list_area,
            list_area.y + 1,
            "Two independent copies exist.",
            Style::default().fg(theme.warning),
        );
        line(
            frame,
            list_area,
            list_area.y + 3,
            "Choose which copy to inspect:",
            Style::default(),
        );
        button(
            frame,
            hits,
            list_area,
            list_area.y + 5,
            list_area.x,
            "[Local]",
            Click::Source(Source::Local),
            false,
            theme,
        );
        button(
            frame,
            hits,
            list_area,
            list_area.y + 6,
            list_area.x,
            "[Shared]",
            Click::Source(Source::Shared),
            false,
            theme,
        );
    } else if let Some(error) = &app.file_error {
        line(
            frame,
            list_area,
            list_area.y + 1,
            label(error, list_area.width as usize),
            Style::default().fg(theme.error),
        );
        line(
            frame,
            list_area,
            list_area.y + 3,
            "e Edit file  ·  F5 Refresh",
            Style::default().fg(theme.muted),
        );
    } else if app.keys.is_empty() {
        line(
            frame,
            list_area,
            list_area.y + 1,
            "No keys in this file. n Add key",
            Style::default().fg(theme.muted),
        );
    } else {
        let items: Vec<_> = app
            .keys
            .iter()
            .map(|key| ListItem::new(label(key, list_area.width.saturating_sub(3) as usize)))
            .collect();
        let mut state = ListState::default().with_selected(Some(app.key_index));
        frame.render_stateful_widget(
            List::new(items).highlight_symbol("› ").highlight_style(
                Style::default()
                    .fg(if app.pane() == Pane::Keys {
                        theme.accent
                    } else {
                        Color::Reset
                    })
                    .add_modifier(Modifier::BOLD),
            ),
            list_area,
            &mut state,
        );
        hits.keys = list_area;
        hits.key_offset = state.offset();
    }
    if preview_height > 0 && app.file_error.is_none() {
        let at = area.bottom() - preview_height;
        line(
            frame,
            area,
            at,
            "─".repeat(area.width as usize),
            Style::default().fg(theme.muted),
        );
        if let Some(key) = app.selected_key() {
            line(
                frame,
                area,
                at + 1,
                label(&format!("Selected: {key}"), area.width as usize),
                Style::default().add_modifier(Modifier::BOLD),
            );
            let value = if app.reveal {
                visible_value(app.values.get(&key).map(String::as_str).unwrap_or(""))
            } else {
                "••••••••".into()
            };
            line(
                frame,
                area,
                at + 2,
                label(
                    &format!("Value: {value}"),
                    area.width.saturating_sub(10) as usize,
                ),
                Style::default(),
            );
            button(
                frame,
                hits,
                area,
                at + 2,
                area.right().saturating_sub(9),
                if app.reveal { "[r Hide]" } else { "[r Reveal]" },
                Click::Key('r'),
                false,
                theme,
            );
        }
    }
}

fn visible_value(value: &str) -> String {
    let mut safe = String::new();
    for ch in value.chars() {
        if ch == '\n' {
            safe.push('\n');
        } else if ch.is_control() {
            safe.extend(ch.escape_default());
        } else {
            safe.push(ch);
        }
    }
    safe
}

fn draw_details(frame: &mut Frame, app: &App, area: Rect, hits: &mut HitAreas, theme: &Theme) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    hits.details = area;
    heading(frame, area, "DETAILS", app.pane() == Pane::Details, theme);
    let Some(file) = app.selected_file() else {
        line(
            frame,
            area,
            area.y + 2,
            "Select a file to inspect it",
            Style::default().fg(theme.muted),
        );
        return;
    };
    let key = app.selected_key();
    line(
        frame,
        area,
        area.y + 1,
        label(
            key.as_deref().unwrap_or(&file.display().to_string()),
            area.width as usize,
        ),
        Style::default().add_modifier(Modifier::BOLD),
    );
    line(
        frame,
        area,
        area.y + 2,
        "Source",
        Style::default().fg(theme.muted),
    );
    let choices = [
        (Source::Auto, "Auto"),
        (Source::Local, "Local"),
        (Source::Shared, "Shared"),
    ];
    let mut x = area.x + 8;
    for (source, text) in choices {
        let shown = format!("[{text}]");
        button(
            frame,
            hits,
            area,
            area.y + 2,
            x,
            &shown,
            Click::Source(source),
            app.source == source,
            theme,
        );
        x += shown.len() as u16 + 1;
    }
    line(
        frame,
        area,
        area.y + 3,
        format!(
            "Using: {}",
            app.resolved
                .as_ref()
                .map_or("unavailable", ResolvedFile::label)
        ),
        Style::default().fg(theme.muted),
    );
    let short = area.height < 11;
    if !short
        && app
            .resolved
            .as_ref()
            .is_some_and(|resolved| resolved.shared)
    {
        line(
            frame,
            area,
            area.y + 4,
            "Edits affect linked worktrees",
            Style::default().fg(theme.warning),
        );
    } else if !short && app.resolved.is_some() {
        line(
            frame,
            area,
            area.y + 4,
            "Edits affect this worktree",
            Style::default().fg(theme.muted),
        );
    }
    let value_y = area.y + if short { 4 } else { 6 };
    line(
        frame,
        area,
        value_y,
        "VALUE",
        Style::default().fg(theme.muted),
    );
    if key.is_some() {
        button(
            frame,
            hits,
            area,
            value_y,
            area.x + 8,
            if app.reveal { "[r Hide]" } else { "[r Reveal]" },
            Click::Key('r'),
            false,
            theme,
        );
    }
    let actions_y = area.bottom().saturating_sub(2);
    if key.is_some() {
        button(
            frame,
            hits,
            area,
            actions_y,
            area.x,
            "s Set",
            Click::Key('s'),
            false,
            theme,
        );
        button(
            frame,
            hits,
            area,
            actions_y,
            area.x + 8,
            "x Unset",
            Click::Key('x'),
            false,
            theme,
        );
    } else if app.file_error.is_none() && app.resolved.is_some() {
        button(
            frame,
            hits,
            area,
            actions_y,
            area.x,
            "n Add key",
            Click::Key('n'),
            false,
            theme,
        );
    }
    if app.resolved.is_some() {
        button(
            frame,
            hits,
            area,
            actions_y + 1,
            area.x,
            "e Edit file",
            Click::Key('e'),
            false,
            theme,
        );
    }
    let value_area = rect(
        area.x,
        value_y + 1,
        area.width,
        actions_y.saturating_sub(value_y + 1),
    );
    if value_area.height > 0 {
        let shown = if let Some(selected) = key {
            if app.reveal {
                visible_value(app.values.get(&selected).map(String::as_str).unwrap_or(""))
            } else {
                "••••••••".into()
            }
        } else if let Some(error) = &app.file_error {
            label(error, area.width as usize)
        } else {
            "Select a key".into()
        };
        frame.render_widget(
            Paragraph::new(shown)
                .wrap(Wrap { trim: false })
                .scroll((app.value_scroll, 0)),
            value_area,
        );
    }
}

fn draw_footer(frame: &mut Frame, app: &App, ui: &UiLayout, hits: &mut HitAreas, theme: &Theme) {
    line(
        frame,
        ui.footer,
        ui.footer.y,
        "─".repeat(ui.footer.width as usize),
        Style::default().fg(theme.muted),
    );
    let note = if !app.message.is_empty() {
        app.message.clone()
    } else if let Some(error) = &app.file_error {
        error.clone()
    } else if app.project.is_none() {
        "n New project  ·  a Attach selected project".into()
    } else {
        format!("{}  ·  {}", app.selected_status(), app.source_label())
    };
    line(
        frame,
        ui.footer,
        ui.footer.y + 1,
        label(&note, ui.footer.width as usize),
        Style::default().fg(if app.file_error.is_some() {
            theme.error
        } else {
            theme.muted
        }),
    );
    let hint = match app.pane() {
        Pane::Files => {
            "Files  ↑↓ move  / filter  i import  a add  l link/relink  u unlink  d remove"
        }
        Pane::Keys => "Keys  ↑↓ move  n new  s set  x unset  e edit",
        Pane::Details => "Details  v source  r reveal  ↑↓ scroll  s set  e edit",
    };
    let hint = format!("{hint}  ·  Tab pane  ? help");
    line(
        frame,
        ui.footer,
        ui.footer.y + 2,
        label(&hint, ui.footer.width as usize),
        Style::default().fg(theme.muted),
    );
    button(
        frame,
        hits,
        ui.footer,
        ui.footer.y + 2,
        ui.footer.right().saturating_sub(6),
        "q quit",
        Click::Quit,
        false,
        theme,
    );
}

fn modal_rect(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    rect(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}

fn modal(frame: &mut Frame, hits: &mut HitAreas, area: Rect, title: &str, theme: &Theme) -> Rect {
    frame.render_widget(Clear, area);
    frame.render_widget(
        Block::bordered()
            .title(format!(" {title} "))
            .border_style(Style::default().fg(theme.accent)),
        area,
    );
    hits.clicks.clear();
    rect(
        area.x + 2,
        area.y + 2,
        area.width.saturating_sub(4),
        area.height.saturating_sub(4),
    )
}

fn draw_modal(frame: &mut Frame, app: &App, ui: &UiLayout, hits: &mut HitAreas, theme: &Theme) {
    match &app.mode {
        Mode::Normal => {}
        Mode::Help => {
            let bounds = modal_rect(ui.content, 78, 20);
            let inner = modal(frame, hits, bounds, "Help", theme);
            let help = "FILES   ↑↓/j/k move   / filter   i import   a add   l link/relink\n        u unlink   d remove   Enter open (compact)\nKEYS    ↑↓/j/k move   n new   s set   x unset   e edit\nSOURCE  v cycles Auto → Local → Shared; click a choice\n        Choice persists per file and worktree; Auto refuses\n        separate local and shared copies\nVALUE   r reveal/hide selected key   ↑↓ scroll revealed value\nPANES   Tab/Shift+Tab or ←/→   Esc back   q quit\nOTHER   F5 refresh   ? help\n\nLINKED link to shared store     STORED available, not linked\nLOCAL independent file         BOTH local and shared copies\nCONFLICT unsafe local path     NO DIR parent directory absent\n\nValues stay masked until revealed. Shared edits affect\nevery linked worktree. Relink keeps a backup if copies differ.\n\nEsc close   ↑↓ scroll";
            frame.render_widget(
                Paragraph::new(help)
                    .wrap(Wrap { trim: false })
                    .scroll((app.help_scroll, 0)),
                inner,
            );
            button(
                frame,
                hits,
                inner,
                inner.bottom().saturating_sub(1),
                inner.right().saturating_sub(10),
                "[Esc Close]",
                Click::Cancel,
                false,
                theme,
            );
        }
        Mode::Prompt(kind, input) => {
            let title = match kind {
                Prompt::Init => "New project",
                Prompt::Add => "Add env file",
                Prompt::Filter => "Filter paths",
                Prompt::NewKey => "New key",
                Prompt::Value(_) => "Set value",
            };
            let bounds = modal_rect(ui.content, 64, 8);
            let inner = modal(frame, hits, bounds, title, theme);
            let shown = if matches!(kind, Prompt::Value(_)) {
                "•".repeat(input.chars().count())
            } else {
                input.clone()
            };
            line(
                frame,
                inner,
                inner.y,
                title,
                Style::default().fg(theme.muted),
            );
            line(
                frame,
                inner,
                inner.y + 2,
                label(&format!("> {shown}_"), inner.width as usize),
                Style::default(),
            );
            button(
                frame,
                hits,
                inner,
                inner.bottom().saturating_sub(1),
                inner.x,
                "[Enter Save]",
                Click::Confirm,
                false,
                theme,
            );
            button(
                frame,
                hits,
                inner,
                inner.bottom().saturating_sub(1),
                inner.x + 16,
                "[Esc Cancel]",
                Click::Cancel,
                false,
                theme,
            );
        }
        Mode::Confirm(confirm) => {
            let bounds = modal_rect(ui.content, 72, 9);
            let inner = modal(frame, hits, bounds, "Confirm", theme);
            let message = match confirm {
                Confirm::Remove(file) => format!("Remove shared {}?", file.display()),
                Confirm::Unset(file, key) => format!("Unset {key} in {}?", file.display()),
                Confirm::Relink(file, _) => format!("Restore shared link for {}?", file.display()),
            };
            frame.render_widget(
                Paragraph::new(message).wrap(Wrap { trim: false }),
                rect(inner.x, inner.y, inner.width, 2),
            );
            let consequence = match confirm {
                Confirm::Remove(_) => "Independent local copies remain.",
                Confirm::Unset(_, _) => "Only this key is removed.",
                Confirm::Relink(_, true) => {
                    "Your different local copy will be backed up privately."
                }
                Confirm::Relink(_, false) => {
                    "Copies match; linked worktrees will use the shared copy."
                }
            };
            line(
                frame,
                inner,
                inner.y + 3,
                consequence,
                Style::default().fg(theme.warning),
            );
            button(
                frame,
                hits,
                inner,
                inner.bottom().saturating_sub(1),
                inner.x,
                "[y Confirm]",
                Click::Confirm,
                false,
                theme,
            );
            button(
                frame,
                hits,
                inner,
                inner.bottom().saturating_sub(1),
                inner.x + 16,
                "[Esc Cancel]",
                Click::Cancel,
                false,
                theme,
            );
        }
        Mode::Notice(message) => {
            let bounds = modal_rect(ui.content, 86, 10);
            let inner = modal(frame, hits, bounds, "Relinked", theme);
            frame.render_widget(
                Paragraph::new(message.as_str()).wrap(Wrap { trim: false }),
                rect(
                    inner.x,
                    inner.y,
                    inner.width,
                    inner.height.saturating_sub(2),
                ),
            );
            button(
                frame,
                hits,
                inner,
                inner.bottom().saturating_sub(1),
                inner.x,
                "[Esc Close]",
                Click::Cancel,
                false,
                theme,
            );
        }
        Mode::RemovalBlocked(file, paths, index) => {
            let bounds = modal_rect(ui.content, 86, 18);
            let inner = modal(frame, hits, bounds, "Remove blocked", theme);
            line(
                frame,
                inner,
                inner.y,
                format!("Unlink {} in these worktrees first:", file.display()),
                Style::default().fg(theme.warning),
            );
            let list_area = rect(
                inner.x,
                inner.y + 2,
                inner.width,
                inner.height.saturating_sub(5),
            );
            let mut state = ListState::default().with_selected(Some(*index));
            frame.render_stateful_widget(
                List::new(paths.iter().map(|p| ListItem::new(p.display().to_string())))
                    .highlight_symbol("› "),
                list_area,
                &mut state,
            );
            hits.modal_list = list_area;
            hits.modal_offset = state.offset();
            line(
                frame,
                inner,
                inner.bottom().saturating_sub(2),
                "Local copies remain after unlink. Then retry removal.",
                Style::default().fg(theme.muted),
            );
            button(
                frame,
                hits,
                inner,
                inner.bottom().saturating_sub(1),
                inner.x,
                "[Esc Close]",
                Click::Cancel,
                false,
                theme,
            );
        }
        Mode::Import {
            paths,
            marked,
            index,
        } => {
            let bounds = modal_rect(ui.content, 86, 22);
            let inner = modal(frame, hits, bounds, "Import ignored env files", theme);
            let list_area = rect(
                inner.x,
                inner.y,
                inner.width,
                inner.height.saturating_sub(2),
            );
            let mut state = ListState::default().with_selected(Some(*index));
            let items: Vec<_> = paths
                .iter()
                .enumerate()
                .map(|(at, path)| {
                    ListItem::new(format!(
                        "{} {}",
                        if marked.contains(&at) { "[x]" } else { "[ ]" },
                        path.display()
                    ))
                })
                .collect();
            frame.render_stateful_widget(
                List::new(items).highlight_symbol("› "),
                list_area,
                &mut state,
            );
            hits.modal_list = list_area;
            hits.modal_offset = state.offset();
            button(
                frame,
                hits,
                inner,
                inner.bottom().saturating_sub(1),
                inner.x,
                "[Space Select]",
                Click::Key(' '),
                false,
                theme,
            );
            button(
                frame,
                hits,
                inner,
                inner.bottom().saturating_sub(1),
                inner.x + 18,
                "[Enter Import]",
                Click::Confirm,
                false,
                theme,
            );
            button(
                frame,
                hits,
                inner,
                inner.bottom().saturating_sub(1),
                inner.x + 38,
                "[Esc Cancel]",
                Click::Cancel,
                false,
                theme,
            );
        }
    }
}

fn draw_dividers(frame: &mut Frame, ui: &UiLayout, theme: &Theme) {
    let positions: &[u16] = match ui.mode {
        WidthMode::Wide => &[ui.files.right() + 1, ui.keys.right() + 1],
        WidthMode::Medium => &[ui.files.right() + 1],
        WidthMode::Compact => &[],
    };
    for &x in positions {
        for y in ui.body.y..ui.body.bottom() {
            line(
                frame,
                rect(x, y, 1, 1),
                y,
                "│",
                Style::default().fg(theme.muted),
            );
        }
    }
}

pub(super) fn draw(frame: &mut Frame, app: &App) -> HitAreas {
    let mut hits = HitAreas::default();
    let theme = Theme::new(app.no_color);
    let Some(ui) = layout(frame.area(), app.pane(), app.keys.len()) else {
        frame.render_widget(
            Paragraph::new("Terminal too small; need 50×16. q quit.")
                .style(Style::default().fg(theme.warning)),
            frame.area(),
        );
        return hits;
    };
    draw_header(frame, app, &ui, &mut hits, &theme);
    if app.project.is_none() {
        draw_projects(frame, app, ui.body, &mut hits, &theme);
    } else {
        draw_files(frame, app, ui.files, &mut hits, &theme);
        draw_keys(
            frame,
            app,
            ui.keys,
            &mut hits,
            &theme,
            ui.mode == WidthMode::Compact,
        );
        draw_details(frame, app, ui.details, &mut hits, &theme);
        draw_dividers(frame, &ui, &theme);
    }
    draw_footer(frame, app, &ui, &mut hits, &theme);
    draw_modal(frame, app, &ui, &mut hits, &theme);
    hits
}
