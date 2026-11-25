//! Breaks the Wayland IME echo loop.
//!
//! While a text field has focus, egui-winit calls `set_ime_cursor_area` after every frame that had any input event
//! (`egui-winit-0.37.0/src/lib.rs`, `handle_platform_output`). On Wayland (text-input-v3) the input method answers
//! that request with an empty `Preedit`, which is an input event again: the app repaints as fast as it can while a
//! note is open (measured 2026-11-14 on KDE Wayland: ~1100 frames/s, 10-30 % CPU at idle).
//!
//! An empty preedit only means something while a composition is in progress (it clears the composition). Outside a
//! composition it is dropped before egui sees it, so the frame has no event, egui-winit does not touch the cursor
//! area again and the loop ends after one frame.

/// Tracks whether an IME composition is in progress across frames.
#[derive(Debug, Default, Clone, Copy)]
pub struct ImeFilter {
    composing: bool,
}

impl ImeFilter {
    /// Removes empty `Preedit` events that arrive outside a composition; keeps everything else in order.
    pub fn filter(&mut self, events: &mut Vec<egui::Event>) {
        if !events.iter().any(|e| matches!(e, egui::Event::Ime(_))) {
            return;
        }
        events.retain(|e| match e {
            egui::Event::Ime(egui::ImeEvent::Preedit { text, .. }) if text.is_empty() => {
                let keep = self.composing;
                self.composing = false;
                keep
            }
            egui::Event::Ime(egui::ImeEvent::Preedit { .. }) => {
                self.composing = true;
                true
            }
            egui::Event::Ime(egui::ImeEvent::Commit(_)) => {
                self.composing = false;
                true
            }
            _ => true,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, ImeEvent};

    fn pre(t: &str) -> Event {
        Event::Ime(ImeEvent::Preedit { text: t.into(), active_range_chars: None })
    }

    #[test]
    fn idle_empty_preedit_is_dropped() {
        let mut f = ImeFilter::default();
        let mut ev = vec![pre("")];
        f.filter(&mut ev);
        assert!(ev.is_empty());
        let mut ev = vec![pre(""), Event::Text("a".into())];
        f.filter(&mut ev);
        assert_eq!(ev, vec![Event::Text("a".into())]);
    }

    #[test]
    fn composition_keeps_its_events_including_the_clearing_preedit() {
        let mut f = ImeFilter::default();
        let mut ev = vec![pre("ka")];
        f.filter(&mut ev);
        assert_eq!(ev.len(), 1);
        // Composition cancelled: the empty preedit must reach the text edit.
        let mut ev = vec![pre("")];
        f.filter(&mut ev);
        assert_eq!(ev, vec![pre("")]);
        // After that, idle echoes are dropped again.
        let mut ev = vec![pre("")];
        f.filter(&mut ev);
        assert!(ev.is_empty());
    }

    #[test]
    fn commit_ends_the_composition() {
        let mut f = ImeFilter::default();
        let mut ev = vec![pre("ka"), Event::Ime(ImeEvent::Commit("か".into())), pre("")];
        f.filter(&mut ev);
        assert_eq!(ev, vec![pre("ka"), Event::Ime(ImeEvent::Commit("か".into()))]);
    }
}
