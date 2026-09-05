//! Catppuccin Mocha colors and semantic styles for the terminal UI.

use ratatui::style::{Modifier, Style};

pub(crate) mod mocha {
    use ratatui::style::Color;

    pub const BASE: Color = Color::Rgb(30, 30, 46);
    pub const MANTLE: Color = Color::Rgb(24, 24, 37);
    pub const SURFACE_0: Color = Color::Rgb(49, 50, 68);
    pub const SURFACE_1: Color = Color::Rgb(69, 71, 90);
    pub const SURFACE_2: Color = Color::Rgb(88, 91, 112);
    pub const OVERLAY_0: Color = Color::Rgb(108, 112, 134);
    pub const SUBTEXT_0: Color = Color::Rgb(166, 173, 200);
    pub const TEXT: Color = Color::Rgb(205, 214, 244);
    pub const LAVENDER: Color = Color::Rgb(180, 190, 254);
    pub const MAUVE: Color = Color::Rgb(203, 166, 247);
    pub const RED: Color = Color::Rgb(243, 139, 168);
    pub const PEACH: Color = Color::Rgb(250, 179, 135);
    pub const YELLOW: Color = Color::Rgb(249, 226, 175);
    pub const GREEN: Color = Color::Rgb(166, 227, 161);
}

pub(crate) fn app() -> Style {
    Style::default().fg(mocha::TEXT).bg(mocha::BASE)
}

pub(crate) fn panel() -> Style {
    app()
}

pub(crate) fn header() -> Style {
    Style::default().fg(mocha::TEXT).bg(mocha::MANTLE)
}

pub(crate) fn border() -> Style {
    Style::default().fg(mocha::SURFACE_2)
}

pub(crate) fn search_prompt() -> Style {
    Style::default().fg(mocha::OVERLAY_0).bg(mocha::MANTLE)
}

pub(crate) fn search_text() -> Style {
    Style::default().fg(mocha::TEXT).bg(mocha::MANTLE)
}

pub(crate) fn tab() -> Style {
    Style::default().fg(mocha::SUBTEXT_0).bg(mocha::MANTLE)
}

pub(crate) fn selected_tab() -> Style {
    Style::default()
        .fg(mocha::MAUVE)
        .bg(mocha::SURFACE_1)
        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
}

pub(crate) fn selected_row() -> Style {
    Style::default()
        .fg(mocha::TEXT)
        .bg(mocha::SURFACE_1)
        .add_modifier(Modifier::BOLD)
}

pub(crate) fn checked() -> Style {
    Style::default().fg(mocha::GREEN)
}

pub(crate) fn unchecked() -> Style {
    Style::default().fg(mocha::OVERLAY_0)
}

pub(crate) fn alias() -> Style {
    Style::default().fg(mocha::GREEN)
}

pub(crate) fn status() -> Style {
    Style::default().fg(mocha::PEACH)
}

pub(crate) fn warning() -> Style {
    Style::default().fg(mocha::YELLOW)
}

pub(crate) fn error() -> Style {
    Style::default().fg(mocha::RED)
}

pub(crate) fn footer() -> Style {
    Style::default().fg(mocha::SUBTEXT_0).bg(mocha::SURFACE_0)
}

pub(crate) fn dialog() -> Style {
    Style::default().fg(mocha::TEXT).bg(mocha::MANTLE)
}

pub(crate) fn dialog_border() -> Style {
    Style::default().fg(mocha::MAUVE).bg(mocha::MANTLE)
}

pub(crate) fn dialog_title() -> Style {
    Style::default()
        .fg(mocha::LAVENDER)
        .bg(mocha::MANTLE)
        .add_modifier(Modifier::BOLD)
}

pub(crate) fn empty() -> Style {
    Style::default().fg(mocha::SUBTEXT_0).bg(mocha::BASE)
}
