//! Browser frontend, written in Rust (wasm-bindgen + web-sys).
//!
//! The page layout lives in static/index.html. The only JavaScript involved
//! is the module loader there.
//!
//! - `logic`: platform-independent parts (file detection, key map, ...),
//!   unit-tested natively
//! - `dom`: small browser helpers
//! - `app`: emulator state, audio/video, persistence, actions
//! - `ui`: wires the page to the app
//! - `idb`: IndexedDB wrapper (save states, last game)

pub mod logic;

#[cfg(target_arch = "wasm32")]
mod app;
#[cfg(target_arch = "wasm32")]
mod dom;
#[cfg(target_arch = "wasm32")]
mod idb;
#[cfg(target_arch = "wasm32")]
mod ui;
