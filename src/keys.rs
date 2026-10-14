// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kostiantyn Andriiuk <konstantin@andriyuk.com>
use serde::Serialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize)]
pub struct Button {
    pub source: &'static str,
    pub code: u32,
    pub label: String,
    pub state: &'static str,
    pub received_monotonic_ns: u64,
    pub device_timestamp_raw: Option<u32>,
    pub kernel_timestamp_ns: Option<u64>,
    pub duration_ms: Option<u64>,
    pub duration_source: Option<&'static str>,
    pub gesture: Option<&'static str>,
    pub physical_mapping_verified: bool,
}
pub fn tcp(body: &[u8], now: u64) -> Result<Button, String> {
    if body.len() != 64 {
        return Err("unknown TCP key layout".into());
    }
    let number = |offset| u32::from_le_bytes(body[offset..offset + 4].try_into().unwrap());
    let code = number(0);
    let state = match number(4) {
        1 => "down",
        2 => "up",
        _ => return Err("unknown TCP key state".into()),
    };
    Ok(Button {
        source: "control",
        code,
        label: if code == 4 {
            "XREAL_QUICK".into()
        } else {
            format!("device_key_{code}")
        },
        state,
        received_monotonic_ns: now,
        device_timestamp_raw: Some(number(8)),
        kernel_timestamp_ns: None,
        duration_ms: None,
        duration_source: None,
        gesture: None,
        physical_mapping_verified: code == 4,
    })
}

pub enum Input {
    Button(Button),
    SynDropped,
    SynReport,
    Other,
}

#[derive(Default)]
pub struct EvdevDecoder {
    bytes: Vec<u8>,
    dropping: bool,
}
impl EvdevDecoder {
    pub fn feed(&mut self, bytes: &[u8], now: u64) -> Result<Vec<Input>, String> {
        if self.bytes.len().saturating_add(bytes.len()) > 24 * 64 + 23 {
            return Err("input receive buffer limit".into());
        }
        self.bytes.extend_from_slice(bytes);
        let complete = self.bytes.len() / 24 * 24;
        let mut output = Vec::new();
        for bytes in self.bytes[..complete].as_chunks::<24>().0 {
            match evdev(bytes, now)? {
                Input::SynDropped => {
                    self.dropping = true;
                    output.push(Input::SynDropped);
                }
                Input::SynReport if self.dropping => {
                    self.dropping = false;
                    output.push(Input::SynReport);
                }
                Input::Button(button) if !self.dropping => output.push(Input::Button(button)),
                _ => {}
            }
        }
        self.bytes.drain(..complete);
        Ok(output)
    }
}
/// Linux64 input_event ABI. Timestamp bytes are retained by the kernel, not reinterpreted as device time.
pub fn evdev(bytes: &[u8], now: u64) -> Result<Input, String> {
    if bytes.len() != 24 {
        return Err("unsupported input_event ABI".into());
    }
    let kind = u16::from_ne_bytes(bytes[16..18].try_into().unwrap());
    let code = u16::from_ne_bytes(bytes[18..20].try_into().unwrap());
    if kind == 0 && code == 3 {
        return Ok(Input::SynDropped);
    }
    if kind == 0 && code == 0 {
        return Ok(Input::SynReport);
    }
    if kind != 1 {
        return Ok(Input::Other);
    }
    let value = i32::from_ne_bytes(bytes[20..24].try_into().unwrap());
    let seconds = i64::from_ne_bytes(bytes[..8].try_into().unwrap());
    let micros = i64::from_ne_bytes(bytes[8..16].try_into().unwrap());
    let kernel_timestamp_ns = if seconds >= 0 && (0..1_000_000).contains(&micros) {
        (seconds as u64)
            .checked_mul(1_000_000_000)
            .and_then(|n| n.checked_add(micros as u64 * 1000))
    } else {
        None
    };
    let state = match value {
        0 => "up",
        1 => "down",
        2 => "repeat",
        _ => return Err("invalid EV_KEY value".into()),
    };
    let label = match code {
        113 => "KEY_MUTE".into(),
        114 => "KEY_VOLUMEDOWN".into(),
        115 => "KEY_VOLUMEUP".into(),
        116 => "KEY_POWER".into(),
        _ => format!("KEY_{code}"),
    };
    Ok(Input::Button(Button {
        source: "evdev",
        code: u32::from(code),
        label,
        state,
        received_monotonic_ns: now,
        device_timestamp_raw: None,
        kernel_timestamp_ns,
        duration_ms: None,
        duration_source: None,
        gesture: None,
        physical_mapping_verified: [114, 115].contains(&code),
    }))
}

#[derive(Default)]
pub struct Holds {
    down: HashMap<(&'static str, u32), (u64, Option<u64>)>,
}
impl Holds {
    pub fn apply(&mut self, button: &mut Button) {
        let key = (button.source, button.code);
        match button.state {
            "down" => {
                if self.down.len() < 64 || self.down.contains_key(&key) {
                    self.down
                        .entry(key)
                        .or_insert((button.received_monotonic_ns, button.kernel_timestamp_ns));
                } else {
                    button.gesture = Some("tracking_limit");
                }
            }
            "up" => {
                if let Some((start, kernel_start)) = self.down.remove(&key) {
                    let kernel_duration = kernel_start
                        .zip(button.kernel_timestamp_ns)
                        .and_then(|(from, to)| to.checked_sub(from));
                    let duration = kernel_duration
                        .unwrap_or_else(|| button.received_monotonic_ns.saturating_sub(start));
                    button.duration_source = Some(if kernel_duration.is_some() {
                        "kernel_event_clock"
                    } else {
                        "receive_monotonic"
                    });
                    let ms = duration / 1_000_000;
                    button.duration_ms = Some(ms);
                    button.gesture = Some(if ms >= 600 {
                        "long_press"
                    } else {
                        "short_press"
                    });
                }
            }
            _ => {}
        }
    }
    pub fn reset_source(&mut self, source: &str) {
        self.down.retain(|(s, _), _| *s != source);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independent_sources_and_hold_duration() {
        let mut body = [0; 64];
        body[0..4].copy_from_slice(&1u32.to_le_bytes());
        body[4..8].copy_from_slice(&1u32.to_le_bytes());
        let mut holds = Holds::default();
        let mut down = tcp(&body, 100).unwrap();
        holds.apply(&mut down);
        body[4..8].copy_from_slice(&2u32.to_le_bytes());
        let mut up = tcp(&body, 700_000_100).unwrap();
        holds.apply(&mut up);
        assert_eq!(up.duration_ms, Some(700));
        assert_eq!(up.gesture, Some("long_press"));
        assert!(!up.physical_mapping_verified);
        assert!(tcp(&body[..63], 0).is_err());
    }
    #[test]
    fn evdev_only_key_and_drop_markers() {
        let mut bytes = [0; 24];
        bytes[16..18].copy_from_slice(&1u16.to_ne_bytes());
        bytes[18..20].copy_from_slice(&115u16.to_ne_bytes());
        bytes[20..24].copy_from_slice(&1i32.to_ne_bytes());
        let Input::Button(b) = evdev(&bytes, 0).unwrap() else {
            panic!()
        };
        assert_eq!(b.label, "KEY_VOLUMEUP");
        assert!(b.physical_mapping_verified);
        bytes[16..18].copy_from_slice(&0u16.to_ne_bytes());
        bytes[18..20].copy_from_slice(&3u16.to_ne_bytes());
        assert!(matches!(evdev(&bytes, 0).unwrap(), Input::SynDropped));
    }

    #[test]
    fn batched_evdev_hold_uses_event_clock() {
        let mut bytes = [0; 24];
        bytes[..8].copy_from_slice(&10i64.to_ne_bytes());
        bytes[16..18].copy_from_slice(&1u16.to_ne_bytes());
        bytes[18..20].copy_from_slice(&115u16.to_ne_bytes());
        bytes[20..24].copy_from_slice(&1i32.to_ne_bytes());
        let Input::Button(mut down) = evdev(&bytes, 5_000).unwrap() else {
            panic!()
        };
        let mut holds = Holds::default();
        holds.apply(&mut down);
        bytes[8..16].copy_from_slice(&750_000i64.to_ne_bytes());
        bytes[20..24].copy_from_slice(&0i32.to_ne_bytes());
        let Input::Button(mut up) = evdev(&bytes, 5_000).unwrap() else {
            panic!()
        };
        holds.apply(&mut up);
        assert_eq!(up.duration_ms, Some(750));
        assert_eq!(up.duration_source, Some("kernel_event_clock"));
        assert_eq!(up.gesture, Some("long_press"));
    }

    #[test]
    fn dropped_input_preserves_order_and_skips_until_sync() {
        fn event(kind: u16, code: u16, value: i32) -> [u8; 24] {
            let mut b = [0; 24];
            b[16..18].copy_from_slice(&kind.to_ne_bytes());
            b[18..20].copy_from_slice(&code.to_ne_bytes());
            b[20..24].copy_from_slice(&value.to_ne_bytes());
            b
        }
        let bytes = [
            event(1, 115, 1),
            event(0, 3, 0),
            event(1, 115, 0),
            event(0, 0, 0),
            event(1, 114, 1),
        ]
        .concat();
        let mut decoder = EvdevDecoder::default();
        assert!(decoder.feed(&bytes[..7], 0).unwrap().is_empty());
        let actions = decoder.feed(&bytes[7..], 1).unwrap();
        assert_eq!(actions.len(), 4);
        assert!(matches!(actions[0], Input::Button(_)));
        assert!(matches!(actions[1], Input::SynDropped));
        assert!(matches!(actions[2], Input::SynReport));
        let Input::Button(last) = &actions[3] else {
            panic!()
        };
        assert_eq!(last.code, 114);
    }
}
