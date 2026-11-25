#![allow(clippy::collapsible_if)]
mod common;

#[path = "perf/bignote_perf.rs"]
mod bignote_perf;
#[path = "perf/editor_perf.rs"]
mod editor_perf;
#[path = "perf/general.rs"]
mod general;
#[path = "perf/preview_perf.rs"]
mod preview_perf;
#[path = "perf/uxpolish_perf.rs"]
mod uxpolish_perf;
