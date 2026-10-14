// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kostiantyn Andriiuk <konstantin@andriyuk.com>
use crate::protocol::Property;
use serde_json::{json, Value};

#[derive(Clone, Copy)]
pub enum Kind {
    Text,
    Config,
    Number,
    Boolean,
}
#[derive(Clone, Copy)]
pub struct Entry {
    pub name: &'static str,
    pub opcode: u16,
    pub kind: Kind,
}
pub const GETS: &[Entry] = &[
    Entry {
        name: "software.version",
        opcode: 0x271d,
        kind: Kind::Text,
    },
    Entry {
        name: "dsp.version",
        opcode: 0x272d,
        kind: Kind::Text,
    },
    Entry {
        name: "factory.config",
        opcode: 0x271f,
        kind: Kind::Config,
    },
    Entry {
        name: "proximity.enabled",
        opcode: 0x2718,
        kind: Kind::Boolean,
    },
    Entry {
        name: "eis.enabled",
        opcode: 0x28a6,
        kind: Kind::Boolean,
    },
    Entry {
        name: "display.edid",
        opcode: 0x275e,
        kind: Kind::Number,
    },
];
pub fn find(name: &str) -> Option<Entry> {
    GETS.iter().find(|e| e.name == name).copied()
}
pub fn capabilities() -> Value {
    let mut entries: Vec<Value> = GETS.iter().map(|e| json!({"name":e.name,"opcode":format!("0x{:04x}",e.opcode),"readable":true,"writable":false,"evidence":"known GET on inspected XREAL1S; live result establishes current support"})).collect();
    for name in [
        "display.brightness",
        "display.dimmer",
        "display.scene",
        "display.input_mode",
    ] {
        entries.push(json!({"name":name,"readable":false,"writable":false,"evidence":"known One/Pro operation; unqualified on this1S"}));
    }
    json!({"api_version":1,"parameters":entries,"imu":{"gyro":"rad/s (protocol interpretation)","accel":"m/s2 (protocol interpretation)","mag":"stream units; physical units unverified","temperature":"degC","pose":false},"buttons":{"evdev":true,"tcp_0x272e":"Quick Button code4 physically verified on1S; other positions unqualified","quick":{"source":"control","code":4,"physical_mapping_verified":true,"gestures":["single","double","long"],"double_window_ms":350,"long_threshold_ms":600,"long_emitted_on":"release","timing_source":"receive_monotonic"},"exclusive_grab":false},"set_enabled":false,"camera":"not implemented; no Eye present in inspected setup"})
}

pub fn value(entry: Entry, property: Property) -> Result<Value, String> {
    match (entry.kind, property) {
        (Kind::Text, Property::Bytes(bytes)) => String::from_utf8(bytes)
            .map(Value::String)
            .map_err(|_| "invalid device UTF-8".into()),
        (Kind::Config, Property::Bytes(bytes)) => {
            if bytes.len() > 256 * 1024 {
                return Err("factory JSON exceeds256KiB POC limit".into());
            }
            let mut result: Value =
                serde_json::from_slice(&bytes).map_err(|_| "invalid factory JSON")?;
            if !result.is_object() {
                return Err("factory config is not an object".into());
            }
            fn nodes(value: &Value) -> usize {
                match value {
                    Value::Object(map) => 1 + map.values().map(nodes).sum::<usize>(),
                    Value::Array(items) => 1 + items.iter().map(nodes).sum::<usize>(),
                    _ => 1,
                }
            }
            if nodes(&result) > 32768 {
                return Err("factory JSON exceeds node budget".into());
            }
            redact(&mut result);
            Ok(result)
        }
        (Kind::Number, Property::Number(n)) => Ok(json!(n)),
        (Kind::Boolean, Property::Number(n @ 0..=1)) => Ok(json!(n == 1)),
        _ => Err("unexpected property type/value".into()),
    }
}

fn redact(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                if ["fsn", "serial", "serial_number"].contains(&key.to_lowercase().as_str()) {
                    *value = json!("<redacted>");
                } else {
                    redact(value);
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                redact(value);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn keep_unknown_factory_fields_but_redact_identity() {
        let e = find("factory.config").unwrap();
        let v = value(
            e,
            Property::Bytes(
                br#"{"FSN":"private","future":{"scale":3,"serial":"private"}}"#.to_vec(),
            ),
        )
        .unwrap();
        assert_eq!(v["FSN"], "<redacted>");
        assert_eq!(v["future"]["scale"], 3);
        assert_eq!(v["future"]["serial"], "<redacted>");
        assert!(find("write_firmware").is_none());
    }
}
