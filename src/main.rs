#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#[cfg(windows)]
mod app;
#[cfg(windows)]
mod caret;
mod convert;
#[cfg(windows)]
mod detector;
#[cfg(windows)]
mod exclusions;
#[cfg(windows)]
mod input;
#[cfg(windows)]
mod overlay;
#[cfg(windows)]
mod preferences;
#[cfg(windows)]
mod settings_window;
#[cfg(windows)]
mod startup;
#[cfg(windows)]
mod storage;
fn main() {
    #[cfg(windows)]
    app::run();
}
