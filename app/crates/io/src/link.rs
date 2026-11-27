//! Ableton Link (tempo + beat phase shared with other apps on the network),
//! through Ableton's own library (rusty_link, GPL-2.0-or-later).

use rusty_link::{AblLink, SessionState};

/// Bars of 4 beats: phase is aligned per bar.
pub const QUANTUM: f64 = 4.0;

pub struct Link {
    abl: AblLink,
    state: SessionState,
    /// Tempo last read from the session (to tell local changes apart).
    last_tempo: f64,
}

impl Link {
    pub fn new(bpm: f64) -> Self {
        Self {
            abl: AblLink::new(bpm),
            state: SessionState::new(),
            last_tempo: bpm,
        }
    }

    pub fn set_enabled(&mut self, on: bool) {
        if self.abl.is_enabled() != on {
            self.abl.enable(on);
        }
    }

    pub fn enabled(&self) -> bool {
        self.abl.is_enabled()
    }

    pub fn peers(&self) -> u64 {
        self.abl.num_peers()
    }

    /// Once per frame. `local_bpm` = the app's tempo: if it changed since the
    /// last call (typed, tapped), it goes to the session (NaN = only read). Returns the session
    /// (tempo, beat).
    pub fn sync(&mut self, local_bpm: f64) -> (f64, f64) {
        let now = self.abl.clock_micros();
        self.abl.capture_app_session_state(&mut self.state);
        if local_bpm.is_finite() && (local_bpm - self.last_tempo).abs() > 1e-6 {
            self.state.set_tempo(local_bpm, now);
            self.abl.commit_app_session_state(&self.state);
        }
        let tempo = self.state.tempo();
        self.last_tempo = tempo;
        (tempo, self.state.beat_at_time(now, QUANTUM))
    }

    /// Resync: bar 1 now, for everyone (Link moves the others' phase too).
    pub fn restart_bar(&mut self) {
        let now = self.abl.clock_micros();
        self.abl.capture_app_session_state(&mut self.state);
        self.state.force_beat_at_time(0.0, now, QUANTUM);
        self.abl.commit_app_session_state(&self.state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Two sessions in one process find each other (needs local networking).
    #[test]
    #[ignore]
    fn two_peers_share_tempo() {
        let mut a = Link::new(120.0);
        let mut b = Link::new(90.0);
        a.set_enabled(true);
        b.set_enabled(true);
        for _ in 0..50 {
            if a.peers() > 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert_eq!(a.peers(), 1, "peers see each other");
        a.sync(133.0);
        std::thread::sleep(Duration::from_millis(500));
        let (tempo, _) = b.sync(b.last_tempo);
        assert!((tempo - 133.0).abs() < 0.01, "b follows a: {tempo}");
    }
}
