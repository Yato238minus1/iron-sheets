//! egui implementations of the `platform` seam.
use crate::platform::{Platform, Waker};
use std::sync::Arc;

/// Repaint requests through an `egui::Context`.
pub struct EguiWaker(pub egui::Context);

impl Waker for EguiWaker {
    fn wake(&self) {
        self.0.request_repaint();
    }
}

/// Per-frame wrapper (lives on the stack).
pub struct EguiPlatform<'a> {
    pub ctx: &'a egui::Context,
    waker: Arc<dyn Waker>,
}

impl<'a> EguiPlatform<'a> {
    /// `waker` is the state's shared waker (cloning an `Arc` is cheap, no allocation).
    pub fn new(ctx: &'a egui::Context, waker: Arc<dyn Waker>) -> Self {
        Self { ctx, waker }
    }
}

impl Platform for EguiPlatform<'_> {
    fn waker(&self) -> Arc<dyn Waker> {
        self.waker.clone()
    }
    fn clipboard_set(&mut self, text: &str) {
        self.ctx.copy_text(text.to_owned());
    }
    fn close_window(&mut self) {
        self.ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
    fn set_title(&mut self, title: &str) {
        self.ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.to_owned()));
    }
}
