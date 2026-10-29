// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kostiantyn Andriiuk <konstantin@andriyuk.com>
//! Exact One1S event bodies observed in controlled take-off/put-on captures.
//! No GET or inference from proximity.enabled; unknown must remain fail-closed.
use serde_json::{json, Value};

#[derive(Default)]
pub struct Wearing {
    state: Option<bool>,
    received: Option<u64>,
}
impl Wearing {
    pub fn accept(&mut self, body: &[u8], now: u64) -> bool {
        self.state = match body {
            [0x18, 1] => Some(true),
            [0x18, 2] => Some(false),
            _ => None,
        };
        self.received = Some(now);
        self.state.is_some()
    }
    pub fn value(&self) -> Value {
        json!({"state":match self.state {Some(true)=>"worn",Some(false)=>"not_worn",None=>"unknown"},
               "received_monotonic_ns":self.received})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_observed_bodies_only_and_reset() {
        let mut wearing = Wearing::default();
        assert_eq!(wearing.value()["state"], "unknown");
        assert!(wearing.value()["received_monotonic_ns"].is_null());
        for body in [
            vec![],
            vec![0x18],
            vec![0x18, 0],
            vec![0x18, 3],
            vec![0x18, 1, 0x18, 2],
            vec![0x18, 0x81, 0],
            vec![0x10, 1],
        ] {
            assert!(wearing.accept(&[0x18, 1], 1));
            assert!(!wearing.accept(&body, 2));
            assert_eq!(wearing.value()["state"], "unknown");
        }
        assert!(wearing.accept(&[0x18, 2], 3));
        assert_eq!(wearing.value()["state"], "not_worn");
        assert!(wearing.accept(&[0x18, 1], 4));
        assert_eq!(wearing.value()["state"], "worn");
        assert_eq!(wearing.value()["received_monotonic_ns"], 4);
        wearing = Wearing::default();
        assert_eq!(wearing.value()["state"], "unknown");
    }
}
