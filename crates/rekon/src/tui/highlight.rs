//! Syntax highlighting with syntect: a whole file at once, into ratatui lines that
//! blocks then slice by range. Recently used files are cached.

use std::collections::VecDeque;
use std::sync::{Arc, OnceLock};

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Theme, ThemeSet};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

const THEME: &str = "base16-ocean.dark";
/// Files above this many lines are shown without highlighting.
pub const MAX_LINES: usize = 20_000;
const CACHE_SIZE: usize = 20;

pub type Lines = Arc<Vec<Line<'static>>>;

struct Assets {
    syntaxes: SyntaxSet,
    theme: Theme,
}

fn assets() -> &'static Assets {
    static ASSETS: OnceLock<Assets> = OnceLock::new();
    ASSETS.get_or_init(|| {
        let syntaxes = SyntaxSet::load_defaults_newlines();
        let mut themes = ThemeSet::load_defaults();
        let theme = themes.themes.remove(THEME).unwrap_or_default();
        Assets { syntaxes, theme }
    })
}

/// Highlights `text` as the language guessed from `path` (or the first line).
/// `None` for files too long to highlight.
pub fn highlight(path: &str, text: &str) -> Option<Vec<Line<'static>>> {
    if text.lines().count() > MAX_LINES {
        return None;
    }
    let a = assets();
    let ext = path.rsplit('.').next().unwrap_or("");
    let name = path.rsplit('/').next().unwrap_or(path);
    let syntax = a
        .syntaxes
        .find_syntax_by_extension(ext)
        .or_else(|| a.syntaxes.find_syntax_by_extension(name))
        .or_else(|| a.syntaxes.find_syntax_by_first_line(text.lines().next().unwrap_or("")))
        .unwrap_or_else(|| a.syntaxes.find_syntax_plain_text());
    let mut h = HighlightLines::new(syntax, &a.theme);
    let mut out = Vec::new();
    for line in LinesWithEndings::from(text) {
        let spans = match h.highlight_line(line, &a.syntaxes) {
            Ok(regions) => regions
                .into_iter()
                .map(|(style, s)| Span::styled(clean(s), convert(style)))
                .filter(|s| !s.content.is_empty())
                .collect(),
            Err(_) => vec![Span::raw(clean(line))],
        };
        out.push(Line::from(spans));
    }
    Some(out)
}

fn clean(s: &str) -> String {
    s.trim_end_matches(['\n', '\r']).replace('\t', "    ")
}

fn convert(style: syntect::highlighting::Style) -> Style {
    let fg = style.foreground;
    let mut out = Style::new().fg(Color::Rgb(fg.r, fg.g, fg.b));
    if style.font_style.contains(FontStyle::BOLD) {
        out = out.add_modifier(Modifier::BOLD);
    }
    if style.font_style.contains(FontStyle::ITALIC) {
        out = out.add_modifier(Modifier::ITALIC);
    }
    if style.font_style.contains(FontStyle::UNDERLINE) {
        out = out.add_modifier(Modifier::UNDERLINED);
    }
    out
}

/// Highlighted files keyed by (path, content hash), most recent last.
#[derive(Default)]
pub struct Cache {
    items: VecDeque<((String, String), Lines)>,
}

impl Cache {
    pub fn get(&mut self, path: &str, hash: &str) -> Option<Lines> {
        let pos = self.items.iter().position(|((p, h), _)| p == path && h == hash)?;
        let item = self.items.remove(pos)?;
        let lines = item.1.clone();
        self.items.push_back(item);
        Some(lines)
    }

    pub fn put(&mut self, path: String, hash: String, lines: Lines) {
        self.items.retain(|((p, _), _)| *p != path);
        self.items.push_back(((path, hash), lines));
        while self.items.len() > CACHE_SIZE {
            self.items.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlights_rust_by_extension() {
        let lines = highlight("a.rs", "fn main() {\n\tlet x = 1;\n}\n").unwrap();
        assert_eq!(lines.len(), 3);
        let text: String = lines[1].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "    let x = 1;");
        assert!(lines[0].spans.len() > 1, "keywords get their own spans");
    }

    #[test]
    fn cache_keeps_twenty_most_recent() {
        let mut c = Cache::default();
        for i in 0..25 {
            c.put(format!("f{i}"), "h".into(), Arc::new(Vec::new()));
        }
        assert!(c.get("f0", "h").is_none());
        assert!(c.get("f24", "h").is_some());
        assert!(c.get("f24", "other").is_none());
    }
}
