//! Editor view-models (no toolkit).
pub mod autoclose;
pub mod buffer;
pub mod find_model;
pub mod history;
#[cfg(test)]
mod model_tests;
pub mod selection;
pub mod transaction;
pub mod wiki_model;

/// Largest char boundary at or below `b` (clamped to the text length).
pub fn floor_boundary(text: &str, b: usize) -> usize {
    let mut b = b.min(text.len());
    while !text.is_char_boundary(b) {
        b -= 1;
    }
    b
}
