//! Meeting recording. `capture` records the microphone and the system audio
//! as two aligned tracks on disk; storage and lifecycle live in
//! `managers::meeting`, which builds on it.

pub mod capture;
pub mod power;
