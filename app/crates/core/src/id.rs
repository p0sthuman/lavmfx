//! Object/parameter identifiers.
//!
//! The REST API addresses every object and parameter by an `int64` id that is
//! unique within a running composition. We hand them out from one counter
//! stored in the composition so ids survive save/load.

use serde::{Deserialize, Serialize};

pub type Id = i64;

/// Monotonic id source. Never hands out 0 (reserved for "none").
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdGen {
    next: Id,
}

impl IdGen {
    pub fn new() -> Self {
        Self { next: 1 }
    }

    pub fn alloc(&mut self) -> Id {
        if self.next < 1 {
            self.next = 1;
        }
        let id = self.next;
        self.next += 1;
        id
    }

    /// Ensure future ids are above `seen` (used after loading foreign files).
    pub fn observe(&mut self, seen: Id) {
        if seen >= self.next {
            self.next = seen + 1;
        }
    }
}
