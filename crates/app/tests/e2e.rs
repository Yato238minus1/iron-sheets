#![allow(clippy::collapsible_if)]

mod common;

#[path = "e2e/driver.rs"]
mod driver;
#[path = "e2e/tier1_core.rs"]
mod tier1_core;
#[path = "e2e/tier1_excalidraw_m3.rs"]
mod tier1_excalidraw_m3;
#[path = "e2e/tier1_git_m4.rs"]
mod tier1_git_m4;
#[path = "e2e/tier1_ux_m2.rs"]
mod tier1_ux_m2;
