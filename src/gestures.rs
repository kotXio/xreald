// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kostiantyn Andriiuk <konstantin@andriyuk.com>
//! One bounded recognizer for the physically verified XREAL1S Quick Button.
//! Raw down/up remains immediate; derived single/double/long are mutually exclusive.
use crate::keys::Button;
use serde::Serialize;

pub const DOUBLE_WINDOW_NS: u64 = 350_000_000;
pub const LONG_PRESS_NS: u64 = 600_000_000;

#[derive(Clone, Copy)]
struct Press {
    down: u64,
    up: u64,
}
impl Press {
    fn duration_ms(self) -> u64 {
        self.up.saturating_sub(self.down) / 1_000_000
    }
}

#[derive(Debug, Serialize)]
pub struct Gesture {
    pub button: &'static str,
    pub source: &'static str,
    pub code: u32,
    pub kind: &'static str,
    pub first_down_monotonic_ns: u64,
    pub last_up_monotonic_ns: u64,
    pub emitted_monotonic_ns: u64,
    pub press_durations_ms: Vec<u64>,
    pub timing_source: &'static str,
    pub physical_mapping_verified: bool,
}
impl Gesture {
    fn new(kind: &'static str, first: Press, second: Option<Press>, now: u64) -> Self {
        let mut durations = vec![first.duration_ms()];
        if let Some(second) = second {
            durations.push(second.duration_ms());
        }
        Self {
            button: "quick",
            source: "control",
            code: 4,
            kind,
            first_down_monotonic_ns: first.down,
            last_up_monotonic_ns: second.unwrap_or(first).up,
            emitted_monotonic_ns: now,
            press_durations_ms: durations,
            timing_source: "receive_monotonic",
            physical_mapping_verified: true,
        }
    }
}

#[derive(Default)]
pub struct QuickGestures {
    down: Option<u64>,
    pending: Option<Press>,
}
impl QuickGestures {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn deadline(&self) -> Option<u64> {
        self.pending.map(|press| match self.down {
            // A second press began in the double window. Wait for its release;
            // once it becomes long, the previous short click is independent.
            Some(down) => down.saturating_add(LONG_PRESS_NS),
            None => press.up.saturating_add(DOUBLE_WINDOW_NS),
        })
    }

    pub fn flush(&mut self, now: u64) -> Vec<Gesture> {
        if self.deadline().is_some_and(|deadline| now >= deadline) {
            let press = self.pending.take().unwrap();
            vec![Gesture::new("single", press, None, now)]
        } else {
            Vec::new()
        }
    }

    pub fn accept(&mut self, button: &Button) -> Vec<Gesture> {
        if button.source != "control" || button.code != 4 {
            return Vec::new();
        }
        let now = button.received_monotonic_ns;
        let mut output = self.flush(now);
        match button.state {
            "down" => {
                self.down.get_or_insert(now);
            }
            "up" => {
                if let Some(down) = self.down.take() {
                    let press = Press { down, up: now };
                    if now.saturating_sub(down) >= LONG_PRESS_NS {
                        // flush() has already emitted any earlier short click.
                        output.push(Gesture::new("long", press, None, now));
                    } else if let Some(first) = self.pending.take() {
                        output.push(Gesture::new("double", first, Some(press), now));
                    } else {
                        self.pending = Some(press);
                    }
                }
            }
            _ => {}
        }
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event(state: &str, now: u64) -> Button {
        let mut body = [0; 64];
        body[..4].copy_from_slice(&4u32.to_le_bytes());
        body[4..8].copy_from_slice(&(if state == "down" { 1u32 } else { 2u32 }).to_le_bytes());
        crate::keys::tcp(&body, now).unwrap()
    }
    #[test]
    fn replay_owner_quick_button_trace() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/quick-button-20261008.json"))
                .unwrap();
        let mut recognizer = QuickGestures::default();
        let mut observed = Vec::new();
        for row in fixture["events"].as_array().unwrap() {
            let now = row["received_ns"].as_u64().unwrap();
            observed.extend(recognizer.accept(&event(row["state"].as_str().unwrap(), now)));
        }
        if let Some(deadline) = recognizer.deadline() {
            observed.extend(recognizer.flush(deadline));
        }
        let names: Vec<_> = observed.iter().map(|e| e.kind).collect();
        assert_eq!(serde_json::json!(names), fixture["expected"]);
        let long = observed.iter().find(|g| g.kind == "long").unwrap();
        assert_eq!(long.press_durations_ms, vec![2169]);
    }
    #[test]
    fn double_suppresses_single_and_disconnect_discards_pending() {
        let mut r = QuickGestures::default();
        assert!(r.accept(&event("down", 0)).is_empty());
        assert!(r.accept(&event("up", 100_000_000)).is_empty());
        assert!(r.flush(199_999_999).is_empty());
        assert!(r.accept(&event("down", 200_000_000)).is_empty());
        let gestures = r.accept(&event("up", 300_000_000));
        assert_eq!(gestures.len(), 1);
        assert_eq!(gestures[0].kind, "double");
        assert!(r.flush(1_000_000_000).is_empty());
        r.accept(&event("down", 2_000_000_000));
        r.accept(&event("up", 2_100_000_000));
        r.reset();
        assert!(r.flush(3_000_000_000).is_empty());
        assert!(r.accept(&event("up", 3_100_000_000)).is_empty());
    }
    #[test]
    fn first_short_then_second_long_and_threshold_boundaries() {
        let mut r = QuickGestures::default();
        r.accept(&event("down", 0));
        r.accept(&event("up", 100_000_000));
        r.accept(&event("down", 200_000_000));
        assert!(r.flush(799_999_999).is_empty());
        assert_eq!(r.flush(800_000_000)[0].kind, "single");
        assert!(r.deadline().is_none());
        assert_eq!(r.accept(&event("up", 800_000_000))[0].kind, "long");
        r.accept(&event("down", 1_000_000_000));
        r.accept(&event("up", 1_100_000_000));
        assert_eq!(r.flush(1_450_000_000)[0].kind, "single");
        assert!(r.flush(9_000_000_000).is_empty());
    }
}
