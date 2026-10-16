//! Meeting recording. `capture` records the microphone and the system audio
//! as two aligned tracks on disk; storage and lifecycle live in
//! `managers::meeting`, which builds on it. `control` and `indicator` are the
//! manual controls (tray, shortcut, CLI) and the floating recording pill.

pub mod capture;
pub mod control;
pub mod indicator;
pub mod power;
