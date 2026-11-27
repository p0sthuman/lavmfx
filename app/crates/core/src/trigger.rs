//! Event params (buttons without a value, e.g. Slice Chase's Next / Reset):
//! a press is queued by the param's id until the next frame reads it.
//!
//! Presses come from the effect panels and from control addresses (OSC,
//! MIDI, REST, keyboard) alike. They change nothing in the composition, so
//! they never make an undo step.

use std::sync::Mutex;

use crate::Id;

static FIRED: Mutex<Vec<Id>> = Mutex::new(Vec::new());

/// Presses kept when nothing reads them (no frame running).
const LIMIT: usize = 256;

/// Press the event param `id`.
pub fn fire(id: Id) {
    let mut q = FIRED.lock().unwrap_or_else(|e| e.into_inner());
    if q.len() < LIMIT {
        q.push(id);
    }
}

/// Presses since the last call (once per frame).
pub fn take() -> Vec<Id> {
    std::mem::take(&mut *FIRED.lock().unwrap_or_else(|e| e.into_inner()))
}
