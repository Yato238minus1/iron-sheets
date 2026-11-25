//! Seam between the framework-free app state and the UI toolkit.
use std::sync::Arc;

/// Background threads call `wake` to ask the UI to repaint.
pub trait Waker: Send + Sync {
    fn wake(&self);
}

/// Window and clipboard side effects used by `command::execute`.
pub trait Platform {
    fn waker(&self) -> Arc<dyn Waker>;
    fn clipboard_set(&mut self, text: &str);
    fn close_window(&mut self);
    fn set_title(&mut self, title: &str);
}
