//! Page preview: hovering an internal link (preview or editor) shows the target note in a popup, like Obsidian.
//! The popup is a second `MarkdownPreview` fed with the note's text; the text of a note that is not open is read by
//! the prefetch worker, never on the UI thread.
use super::markdown::{MarkdownPreview, percent_decode};
use crate::command::Command;
use crate::scrollsync::PaneScroll;
use crate::state::AppState;
use egui::{Context, Key, Rect, vec2};
use iron_sheets_core::FileKind;
use iron_sheets_md::model::LinkTarget;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const SIZE: egui::Vec2 = vec2(520.0, 360.0);
/// The pointer may leave link and popup for this long (the gap between them) before the popup closes.
const GRACE: Duration = Duration::from_millis(250);
/// With "Always" the popup waits this long, so moving across a page does not flash popups.
const DELAY: Duration = Duration::from_millis(350);
/// How often the prefetch worker's result is looked at.
const POLL: Duration = Duration::from_millis(60);
/// A note that has not arrived after this long is too big to prefetch.
const GIVE_UP: Duration = Duration::from_secs(3);

/// An internal link the pointer is on, in screen space.
#[derive(Clone, Debug, PartialEq)]
pub struct LinkHover {
    pub note: String,
    pub heading: Option<String>,
    pub from: PathBuf,
    pub rect: Rect,
}

impl LinkHover {
    fn same(&self, o: &LinkHover) -> bool {
        (&self.note, &self.heading, &self.from) == (&o.note, &o.heading, &o.from)
    }
}

/// (note, heading) a link points at, `None` for web links, same-note anchors and footnotes.
pub fn internal_target(t: &LinkTarget) -> Option<(String, Option<String>)> {
    match t {
        LinkTarget::Wiki { note, heading, .. } if !note.is_empty() => Some((note.clone(), heading.clone())),
        LinkTarget::Url(u) => {
            let l = u.to_ascii_lowercase();
            if u.starts_with('#') || l.starts_with("http://") || l.starts_with("https://") || l.starts_with("mailto:") {
                return None;
            }
            let (p, frag) = u.split_once('#').map_or((u.as_str(), None), |(p, f)| (p, Some(percent_decode(f))));
            let p = percent_decode(p);
            (!p.is_empty()).then(|| (p.strip_suffix(".md").unwrap_or(&p).to_string(), frag))
        }
        _ => None,
    }
}

struct Cur {
    hover: LinkHover,
    since: Instant,
    delay: Duration,
    /// The pointer left link and popup at this moment.
    left_at: Option<Instant>,
    /// `Some(None)`: no such note.
    path: Option<Option<PathBuf>>,
    text: Option<(String, u64)>,
    goto: Option<String>,
    polled: Option<Instant>,
    rect: Option<Rect>,
}

#[derive(Default)]
pub struct PagePopup {
    offered: Option<LinkHover>,
    cur: Option<Cur>,
    /// Esc closed the popup of this link; it stays closed until the pointer leaves the link.
    dismissed: Option<LinkHover>,
    /// Esc was pressed while the popup was open (taken at the start of the frame, before anything else sees it).
    esc: bool,
    preview: MarkdownPreview,
    scroll: PaneScroll,
    /// Popup for a link inside this popup, stacked on top.
    child: Option<Box<PagePopup>>,
    depth: u64,
}

impl PagePopup {
    /// A link is under the pointer this frame (called by the preview and the editor).
    pub fn offer(&mut self, h: LinkHover) {
        self.offered = Some(h);
    }

    /// The popup stacked on this one (tests).
    pub fn child(&self) -> Option<&PagePopup> {
        self.child.as_deref()
    }

    /// The pointer is on this popup or a nested one.
    fn over(&self, p: egui::Pos2) -> bool {
        self.cur.as_ref().and_then(|c| c.rect).is_some_and(|r| r.contains(p)) || self.child.as_ref().is_some_and(|c| c.over(p))
    }

    /// Esc goes to the topmost open popup.
    fn set_esc(&mut self) {
        match self.child.as_mut() {
            Some(c) if c.is_open() => c.set_esc(),
            _ => self.esc = true,
        }
    }

    /// Scroll offset of the popup's note (tests).
    pub fn scroll_offset(&self) -> f32 {
        self.scroll.offset
    }

    /// Start of the frame: Esc belongs to the open popup alone (unless a modal dialog is open).
    pub fn take_escape(ctx: &Context, state: &mut AppState) {
        if state.dialogs.current.is_some() {
            return;
        }
        let p = &mut state.ui.page_popup;
        if p.is_open()
            && ctx.input_mut(|i| {
                let n = i.events.len();
                i.events.retain(|e| !matches!(e, egui::Event::Key { key: Key::Escape, pressed: true, .. }));
                n != i.events.len()
            })
        {
            p.set_esc();
        }
    }

    /// Forgets the popup's note and frees what the second preview built (docs, textures, layouts, worker).
    fn close(&mut self) {
        self.cur = None;
        self.child = None;
        self.preview = MarkdownPreview::default();
        self.scroll = PaneScroll { force: Some(0.0), ..Default::default() };
    }

    /// The popup's preview and its screen rect (tests).
    pub fn preview_rect(&self) -> (&MarkdownPreview, Option<Rect>) {
        (&self.preview, self.cur.as_ref().and_then(|c| c.rect))
    }

    pub fn is_open(&self) -> bool {
        self.cur.as_ref().is_some_and(|c| c.rect.is_some())
    }

    /// Once per frame, after the panels so the popup is on top.
    pub fn show(ctx: &Context, state: &mut AppState) {
        let mut me = std::mem::take(&mut state.ui.page_popup);
        me.run(ctx, state);
        state.ui.page_popup = me;
    }

    /// Returns whether a click in this popup or one stacked on it followed a link.
    fn run(&mut self, ctx: &Context, state: &mut AppState) -> bool {
        let offered = self.offered.take();
        if offered.is_none() && self.cur.is_none() {
            self.dismissed = None;
            return false;
        }
        let mode = state.config.page_preview.as_str();
        let ctrl = ctx.input(|i| i.modifiers.command);
        let allowed = mode == "always" || (mode.is_empty() && ctrl);
        if offered.is_none() {
            self.dismissed = None;
        }
        let offer = offered.filter(|h| allowed && !self.dismissed.as_ref().is_some_and(|d| d.same(h)));
        let now = Instant::now();
        let over_popup = ctx.pointer_hover_pos().is_some_and(|p| self.over(p));
        match offer {
            Some(h) => match &mut self.cur {
                Some(c) if c.hover.same(&h) => c.left_at = None,
                _ => {
                    self.scroll = PaneScroll { force: Some(0.0), ..Default::default() };
                    self.child = None;
                    let delay = if mode == "always" { DELAY } else { Duration::ZERO };
                    self.cur =
                        Some(Cur { goto: h.heading.clone(), hover: h, since: now, delay, left_at: None, path: None, text: None, polled: None, rect: None });
                }
            },
            None => {
                if let Some(c) = &mut self.cur {
                    c.left_at = if over_popup || state.dialogs.current.is_some() { None } else { Some(c.left_at.unwrap_or(now)) };
                }
            }
        }
        if let Some(c) = &self.cur {
            if c.left_at.is_some_and(|t| t.elapsed() >= GRACE) {
                self.close();
                return false;
            }
            if c.left_at.is_some() {
                ctx.request_repaint_after(GRACE);
            }
        }
        if std::mem::take(&mut self.esc) && self.cur.is_some() {
            self.dismissed = self.cur.as_ref().map(|c| c.hover.clone());
            self.close();
            return false;
        }
        let Some(mut c) = self.cur.take() else { return false };
        if c.since.elapsed() < c.delay {
            ctx.request_repaint_after(c.delay - c.since.elapsed());
            self.cur = Some(c);
            return false;
        }
        self.load(&mut c, ctx, state);
        let (rect, followed) = self.paint(&mut c, ctx, state);
        c.rect = Some(rect);
        self.cur = Some(c);
        // A click followed a link: the page behind changes, so the whole stack goes away.
        let followed = followed || self.child.as_mut().is_some_and(|ch| ch.run(ctx, state));
        if followed {
            self.dismissed = None;
            self.close();
        }
        followed
    }

    /// Finds the note and gets its text; leaves `text` empty while the background read runs.
    fn load(&mut self, c: &mut Cur, ctx: &Context, state: &mut AppState) {
        if c.path.is_none() {
            let Some(idx) =
                state.indexer.index().try_read().map(|g| g.resolve(&c.hover.note, Some(&c.hover.from)).and_then(|id| g.get(id).map(|m| m.path.clone())))
            else {
                ctx.request_repaint_after(Duration::from_millis(30));
                return;
            };
            c.path = Some(idx);
        }
        let Some(Some(path)) = &c.path else { return };
        if FileKind::from_path(path) != FileKind::Markdown {
            return;
        }
        if let Some(d) = state.docs.get(path) {
            // Open note: its live text, copied again only when it changed.
            if c.text.as_ref().is_none_or(|t| t.1 != d.version) {
                c.text = Some((d.text.clone(), d.version));
            }
        } else if c.text.is_none() {
            // `take` stats the file: ask once, then look every POLL, not every frame.
            if c.polled.is_none() {
                state.docs.prefetch.request(path);
            }
            if c.polled.is_none_or(|t| t.elapsed() >= POLL) {
                c.polled = Some(Instant::now());
                if let Some(l) = state.docs.prefetch.take(path) {
                    let v = iron_sheets_app_core::prefetch::text_hash(&l.text);
                    c.text = Some((l.text, v));
                    return;
                }
            }
            ctx.request_repaint_after(POLL);
        }
    }

    /// Returns the popup's rect and whether a click in it followed a link.
    fn paint(&mut self, c: &mut Cur, ctx: &Context, state: &mut AppState) -> (Rect, bool) {
        let screen = ctx.content_rect();
        let a = c.hover.rect;
        let mut pos = egui::pos2(a.min.x, a.max.y + 2.0);
        if pos.y + SIZE.y + 12.0 > screen.max.y && a.min.y - SIZE.y - 14.0 > screen.min.y {
            pos.y = a.min.y - SIZE.y - 14.0;
        }
        pos.x = pos.x.min(screen.max.x - SIZE.x - 12.0).max(screen.min.x);
        let order = if state.dialogs.current.is_some() { egui::Order::Middle } else { egui::Order::Tooltip };
        let area = egui::Area::new(egui::Id::NULL.with(("page_preview", self.depth))).order(order).fixed_pos(pos);
        let (mut hover, mut followed) = (None, false);
        let out = area.show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_min_size(SIZE);
                ui.set_max_size(SIZE);
                match (&c.path, &c.text) {
                    (None, _) => {
                        ui.label("Loading...");
                    }
                    (Some(None), _) => {
                        ui.label("Note not found");
                    }
                    (Some(Some(p)), _) if FileKind::from_path(p) != FileKind::Markdown => {
                        ui.label("No preview for this file type");
                    }
                    (Some(Some(p)), Some((text, v))) => {
                        let n = state.commands.len();
                        hover = self.preview.show_doc(ui, state, p, 0x9A6E + self.depth, Some((text, *v)), &mut self.scroll, false, Some(&mut c.goto));
                        followed = state.commands[n..].iter().any(|c| matches!(c, Command::OpenLink { .. } | Command::OpenPath { .. }));
                    }
                    (Some(Some(_)), None) => {
                        ui.label(if c.since.elapsed() > GIVE_UP { "Note too large to preview" } else { "Loading..." });
                    }
                }
            });
        });
        if let Some(h) = hover {
            let depth = self.depth + 1;
            self.child.get_or_insert_with(|| Box::new(PagePopup { depth, ..Default::default() })).offer(h);
        }
        (out.response.rect, followed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use LinkTarget::{Tag, Url, Wiki};

    #[test]
    fn internal_targets() {
        let t = |u: &str| internal_target(&Url(u.into()));
        assert_eq!(t("https://x.org"), None);
        assert_eq!(t("#h"), None);
        assert_eq!(t("Notes/My%20Note.md#Part"), Some(("Notes/My Note".into(), Some("Part".into()))));
        assert_eq!(internal_target(&Wiki { note: "A".into(), heading: None, block: None }), Some(("A".into(), None)));
        assert_eq!(internal_target(&Wiki { note: String::new(), heading: Some("H".into()), block: None }), None);
        assert_eq!(internal_target(&Tag("t".into())), None);
    }
}
