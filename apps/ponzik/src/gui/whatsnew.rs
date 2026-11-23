//! "What's new in Ponzik": the latest section of the changelog (compiled into
//! the program), shown once after an update; the whole changelog on demand.

use ponzik_i18n::{tr, tr_fmt};
use ponzik_project::update::{ChangelogSection, changelog_sections};
use ponzik_ui::{Canvas, Color, Rect};

use super::Input;
use super::app::{App, Popup};
use super::theme::*;
use super::widgets::{Icon, draw_icon, wid};
use crate::build_info;

/// `docs/CHANGELOG.md`, compiled in.
const CHANGELOG: &str = include_str!("../../../../docs/CHANGELOG.md");

/// The changelog's sections, newest first.
pub fn sections() -> Vec<ChangelogSection> {
    changelog_sections(CHANGELOG)
}

/// One laid-out line of the changelog.
enum Line {
    /// `## ` heading of a section (whole changelog only).
    Section(String),
    /// `### ` group: "New instruments", "Mixer"…
    Group(String),
    /// Text line of a list item: (text, first line of the item).
    Bullet(String, bool),
    Text(String),
    Gap(f32),
}

impl Line {
    fn height(&self) -> f32 {
        match self {
            Line::Section(_) => 40.0,
            Line::Group(_) => 32.0,
            Line::Bullet(..) | Line::Text(_) => 20.0,
            Line::Gap(h) => *h,
        }
    }
}

/// A block of a changelog section: lines of a paragraph or of a list item
/// that go on over several source lines are joined.
#[derive(Debug, PartialEq)]
enum Block {
    Group(String),
    Item(String),
    Text(String),
}

fn blocks(body: &str) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::new();
    let mut open = false;
    for raw in body.lines() {
        let l = raw.trim();
        if l.is_empty() {
            open = false;
        } else if let Some(h) = l.strip_prefix("### ") {
            out.push(Block::Group(h.trim().to_string()));
            open = false;
        } else if let Some(item) = l.strip_prefix("- ").or_else(|| l.strip_prefix("* ")) {
            out.push(Block::Item(item.trim().to_string()));
            open = true;
        } else if let (true, Some(Block::Item(t) | Block::Text(t))) = (open, out.last_mut()) {
            t.push(' ');
            t.push_str(l);
        } else {
            out.push(Block::Text(l.to_string()));
            open = true;
        }
    }
    out
}

/// Markdown made plain: no `**`, no backticks, links as their text.
fn plain(s: &str) -> String {
    let s = s.replace("**", "").replace('`', "");
    // [text](url) → text
    let mut out = String::with_capacity(s.len());
    let mut rest = s.as_str();
    while let Some(open) = rest.find('[') {
        let Some(mid) = rest[open..].find("](") else { break };
        let Some(close) = rest[open + mid..].find(')') else { break };
        out.push_str(&rest[..open]);
        out.push_str(&rest[open + 1..open + mid]);
        rest = &rest[open + mid + close + 1..];
    }
    out.push_str(rest);
    out
}

impl App {
    /// After an update: the popup, once per changelog section. A brand new
    /// user (the tour is starting) has nothing to compare with.
    pub fn show_whats_new_if_updated(&mut self) {
        let Some(latest) = sections().into_iter().next() else { return };
        if self.updates.news_seen == latest.title {
            return;
        }
        if self.tour.step.is_some() {
            self.updates.news_seen = latest.title;
            self.save_settings();
            return;
        }
        // "Don't show again" starts checked: the section counts as seen now.
        self.updates.news_prev = std::mem::replace(&mut self.updates.news_seen, latest.title);
        self.save_settings();
        self.open_whats_new();
    }

    pub fn open_whats_new(&mut self) {
        self.updates.news_full = false;
        self.updates.news_scroll = 0.0;
        self.open_popup(Popup::WhatsNew, Rect::default());
    }

    fn news_lines(&self, full: bool, width: f32) -> Vec<Line> {
        let mut lines = Vec::new();
        let all = sections();
        let shown = if full { &all[..] } else { &all[..all.len().min(1)] };
        for (k, s) in shown.iter().enumerate() {
            if full {
                if k > 0 {
                    lines.push(Line::Gap(10.0));
                }
                lines.push(Line::Section(s.title.trim_matches(['[', ']']).replace("] ", " ")));
            }
            for (k, block) in blocks(&s.body).into_iter().enumerate() {
                match block {
                    Block::Group(h) => {
                        if k > 0 {
                            lines.push(Line::Gap(10.0));
                        }
                        lines.push(Line::Group(plain(&h)));
                    }
                    Block::Item(item) => {
                        for (i, w) in self.wrap(self.f.sans, 13.0, &plain(&item), width - 22.0).into_iter().enumerate() {
                            lines.push(Line::Bullet(w, i == 0));
                        }
                        lines.push(Line::Gap(5.0));
                    }
                    Block::Text(text) => {
                        for w in self.wrap(self.f.sans, 13.0, &plain(&text), width) {
                            lines.push(Line::Text(w));
                        }
                        lines.push(Line::Gap(6.0));
                    }
                }
            }
        }
        lines
    }

    /// The popup; false when it closes.
    pub(super) fn whats_new_popup(&mut self, c: &mut Canvas, input: &Input, screen: Rect) -> bool {
        let t = self.theme;
        let open = self.anim.to(wid("popup", 0, 0), 1.0, T_HOVER);
        let all = sections();
        let Some(latest) = all.first() else { return false };
        let full = self.updates.news_full;

        c.fill_rect(screen, Color::rgba(0, 0, 0, (120.0 * open) as u8));
        let w = 640.0f32.min(screen.w - 40.0);
        let h = 600.0f32.min(screen.h - 60.0);
        let r = Rect::new(((screen.w - w) * 0.5).round(), ((screen.h - h) * 0.5 + (1.0 - open) * 12.0).round(), w, h);
        c.fill_rrect(r.translate(0.0, 12.0).inset(-4.0), 24.0, Color::rgba(0, 0, 0, (100.0 * open) as u8));
        c.fill_rrect(r, 20.0, t.panel);
        // A green dawn behind the header.
        c.fill_rrect_vgrad(Rect::new(r.x, r.y, r.w, 120.0), 20.0, t.panel.mix(t.accent, 0.13), t.panel);
        c.stroke_rrect(r, 20.0, 1.0, t.hairline.alpha(1.8));

        // Header: the mark, the title, the date and build.
        let mark = Rect::new(r.x + 26.0, r.y + 24.0, 56.0, 56.0);
        ponzik_ui::brand::draw_mark(c, mark);
        let tx = mark.right() + 18.0;
        let title = if full { tr("Changelog").to_string() } else { tr_fmt!("What's new in Ponzik {}", latest.version.as_str()) };
        self.text_fit(c, self.f.sans_med, 20.0, tx, r.y + 44.0, &title, r.right() - tx - 64.0, t.text);
        let sub = if full {
            ponzik_i18n::tr_fmt_n!("{} version · you have {}", "{} versions · you have {}", all.len(), all.len(), build_info::version().semver())
        } else {
            let v = build_info::version();
            let build = if v.build > 0 { tr_fmt!("build {}", v.build) } else { v.semver() };
            [latest.label.clone(), build].into_iter().filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" · ")
        };
        self.text_fit(c, self.f.sans, 12.0, tx, r.y + 68.0, &sub, r.right() - tx - 64.0, t.text2);
        let close_r = Rect::new(r.right() - 18.0 - 34.0, r.y + 18.0, 34.0, 34.0);
        let mut keep = !self.icon_button(c, input, wid("news-close", 0, 0), close_r, Icon::Close, false, (tr("Close"), tr("Esc")));

        // Body: the section, scrollable.
        let body = Rect::new(r.x + 28.0, r.y + 104.0, r.w - 56.0, r.h - 104.0 - 72.0);
        let lines = self.news_lines(full, body.w - 14.0);
        let total: f32 = lines.iter().map(Line::height).sum::<f32>() + 8.0;
        let max_scroll = (total - body.h).max(0.0);
        if input.inside && body.contains(input.x, input.y) && input.wheel != 0.0 {
            self.updates.news_scroll -= input.wheel * 48.0;
        }
        self.updates.news_scroll = self.updates.news_scroll.clamp(0.0, max_scroll);
        let scroll = self.anim.to(wid("news-scroll", 0, 0), self.updates.news_scroll, T_LAYOUT);
        let prev = c.push_clip(body);
        let mut y = body.y - scroll;
        for line in &lines {
            let lh = line.height();
            if y + lh >= body.y - 40.0 && y <= body.bottom() + 40.0 {
                match line {
                    Line::Section(s) => {
                        self.text(c, self.f.sans_med, 16.0, body.x, y + 22.0, s, t.text);
                        c.fill_rect(Rect::new(body.x, y + 36.0, body.w - 14.0, 1.0), t.hairline);
                    }
                    Line::Group(s) => {
                        c.fill_rrect(Rect::new(body.x, y + 12.0, 4.0, 14.0), 2.0, t.accent);
                        self.text(c, self.f.sans_med, 14.0, body.x + 14.0, y + 19.0, s, t.accent.mix(t.text, 0.35));
                    }
                    Line::Bullet(s, first) => {
                        if *first {
                            c.fill_circle(body.x + 6.0, y + 10.0, 2.5, t.accent.alpha(0.8));
                        }
                        self.text(c, self.f.sans, 13.0, body.x + 22.0, y + 10.0, s, t.text2.mix(t.text, 0.4));
                    }
                    Line::Text(s) => {
                        self.text(c, self.f.sans, 13.0, body.x, y + 10.0, s, t.text2);
                    }
                    Line::Gap(_) => {}
                }
            }
            y += lh;
        }
        c.set_clip(Some(prev));
        if max_scroll > 0.0 {
            // More below: the text fades out, and a thin bar tells where we are.
            if scroll < max_scroll - 1.0 {
                c.fill_rrect_vgrad(Rect::new(body.x, body.bottom() - 36.0, body.w, 36.0), 0.0, t.panel.alpha(0.0), t.panel);
            }
            let track = Rect::new(body.right() - 4.0, body.y, 4.0, body.h);
            c.fill_rrect(track, 2.0, t.well);
            let th = (body.h * body.h / total).max(28.0);
            let ty = track.y + (track.h - th) * (scroll / max_scroll);
            c.fill_rrect(Rect::new(track.x, ty, 4.0, th), 2.0, t.muted.alpha(0.8));
        }

        // Footer: don't show again, the whole changelog, done.
        let foot_y = r.bottom() - 72.0;
        c.fill_rect(Rect::new(r.x + 1.0, foot_y, r.w - 2.0, 1.0), t.hairline);
        let cy = foot_y + 36.0;
        let checked = self.updates.news_seen == latest.title;
        let label = tr("Don't show again for this version");
        let lw = self.fonts.measure(self.f.sans, 13.0, label);
        let row = Rect::new(r.x + 24.0, cy - 16.0, 30.0 + lw, 32.0);
        let id = wid("news-dont", 0, 0);
        let hov = self.hovered(input, id, row);
        let hv = self.hover_anim(id, hov);
        let on = self.anim.to(id ^ 0x4f4e, checked as u8 as f32, T_HOVER);
        let bx = Rect::new(row.x, cy - 9.0, 18.0, 18.0);
        c.fill_rrect(bx, 5.0, t.well.mix(t.accent, on));
        c.stroke_rrect(bx, 5.0, 1.0, t.hairline.mix(t.muted, hv).mix(t.accent, on));
        if on > 0.05 {
            draw_icon(c, Icon::Check, bx.x + 9.0, bx.y + 9.0, 12.0, t.on_accent.alpha(on));
        }
        self.text(c, self.f.sans, 13.0, bx.right() + 10.0, cy, label, t.text2.mix(t.text, hv));
        if hov {
            self.set_cursor(ponzik_window::Cursor::Hand);
            if input.pressed {
                self.updates.news_seen = if checked {
                    let prev = std::mem::take(&mut self.updates.news_prev);
                    if prev == latest.title { String::new() } else { prev }
                } else {
                    self.updates.news_prev = std::mem::take(&mut self.updates.news_seen);
                    latest.title.clone()
                };
                self.save_settings();
            }
        }
        let done = tr("Got it");
        let dw = self.fonts.measure(self.f.sans_med, 12.0, done) + 48.0;
        let done_r = Rect::new(r.right() - 24.0 - dw, cy - 16.0, dw, 32.0);
        if self.pill_button(c, input, wid("news-done", 0, 0), done_r, Icon::Check, done, true) {
            keep = false;
        }
        let other = if full { tr("Latest only") } else { tr("Full changelog") };
        let ow = self.fonts.measure(self.f.sans_med, 12.0, other) + 48.0;
        let other_r = Rect::new(done_r.x - 8.0 - ow, cy - 16.0, ow, 32.0);
        if self.pill_button(c, input, wid("news-full", 0, 0), other_r, Icon::History, other, false) {
            self.updates.news_full = !full;
            self.updates.news_scroll = 0.0;
            self.anim.set(wid("news-scroll", 0, 0), 0.0);
        }
        if input.pressed && !r.contains(input.x, input.y) {
            keep = false;
        }
        keep
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_changelog_has_a_latest_section_with_groups() {
        let s = sections();
        assert!(!s.is_empty(), "docs/CHANGELOG.md has no ## section");
        let latest = &s[0];
        assert!(!latest.version.is_empty());
        assert!(latest.body.lines().any(|l| l.starts_with("### ")), "no ### group in {}", latest.title);
        assert!(latest.body.lines().any(|l| l.starts_with("- ")), "no item in {}", latest.title);
    }

    #[test]
    fn paragraphs_and_items_go_on_over_lines() {
        let b = blocks("Intro on\ntwo lines.\n\n### Mixer\n- One\n  more\n- Two\n\nAfter.");
        assert_eq!(
            b,
            vec![
                Block::Text("Intro on two lines.".into()),
                Block::Group("Mixer".into()),
                Block::Item("One more".into()),
                Block::Item("Two".into()),
                Block::Text("After.".into()),
            ]
        );
    }

    #[test]
    fn markdown_is_made_plain() {
        assert_eq!(plain("**Mixer**: `999` tracks, see [the docs](http://x) now"), "Mixer: 999 tracks, see the docs now");
        assert_eq!(plain("a [b] c"), "a [b] c");
    }
}
