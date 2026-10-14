// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kostiantyn Andriiuk <konstantin@andriyuk.com>
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Reading<T> {
    pub value: T,
    pub device_timestamp_ns: u64,
    pub received_monotonic_ns: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Snapshot {
    pub epoch: u64,
    pub sequence: u64,
    pub gyro: Option<Reading<[f32; 3]>>,
    pub accel: Option<Reading<[f32; 3]>>,
    pub mag: Option<Reading<[f32; 3]>>,
    pub temperature: Option<Reading<f32>>,
}

#[derive(Debug)]
pub struct Sample {
    pub timestamp: u64,
    pub mask: u32,
    pub gyro: Option<[f32; 3]>,
    pub accel: Option<[f32; 3]>,
    pub mag: Option<[f32; 3]>,
    pub temperature: Option<f32>,
}

pub fn decode(opcode: u16, body: &[u8]) -> Result<Sample, String> {
    if ![0x2836, 0x2736].contains(&opcode) || body.len() != 128 {
        return Err("unknown IMU frame layout".into());
    }
    let timestamp = u64::from_le_bytes(body[8..16].try_into().unwrap());
    let mask = u32::from_le_bytes(body[24..28].try_into().unwrap());
    let vector = |offset: usize| -> Result<[f32; 3], String> {
        let values = std::array::from_fn(|i| {
            f32::from_le_bytes(body[offset + i * 4..offset + i * 4 + 4].try_into().unwrap())
        });
        if values.iter().all(|x| x.is_finite()) {
            Ok(values)
        } else {
            Err("non-finite IMU vector".into())
        }
    };
    let temperature = if mask & 8 != 0 {
        let value = f32::from_le_bytes(body[64..68].try_into().unwrap());
        if !value.is_finite() {
            return Err("non-finite IMU temperature".into());
        }
        Some(value)
    } else {
        None
    };
    Ok(Sample {
        timestamp,
        mask,
        gyro: if mask & 1 != 0 {
            Some(vector(28)?)
        } else {
            None
        },
        accel: if mask & 2 != 0 {
            Some(vector(40)?)
        } else {
            None
        },
        mag: if mask & 4 != 0 {
            Some(vector(52)?)
        } else {
            None
        },
        temperature,
    })
}

impl Snapshot {
    pub fn update(&mut self, sample: Sample, now: u64) {
        self.sequence += 1;
        let reading = |value| Reading {
            value,
            device_timestamp_ns: sample.timestamp,
            received_monotonic_ns: now,
        };
        if let Some(value) = sample.gyro {
            self.gyro = Some(reading(value));
        }
        if let Some(value) = sample.accel {
            self.accel = Some(reading(value));
        }
        if let Some(value) = sample.mag {
            self.mag = Some(reading(value));
        }
        if let Some(value) = sample.temperature {
            self.temperature = Some(Reading {
                value,
                device_timestamp_ns: sample.timestamp,
                received_monotonic_ns: now,
            });
        }
    }

    pub fn fresh(&self, now: u64) -> bool {
        self.gyro
            .as_ref()
            .is_some_and(|x| now.saturating_sub(x.received_monotonic_ns) < 500_000_000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mask_does_not_overwrite_other_sensor() {
        let mut body = [0u8; 128];
        body[24..28].copy_from_slice(&3u32.to_le_bytes());
        body[28..32].copy_from_slice(&2f32.to_le_bytes());
        let mut state = Snapshot::default();
        state.update(decode(0x2836, &body).unwrap(), 10);
        body[24..28].copy_from_slice(&4u32.to_le_bytes());
        body[28..32].copy_from_slice(&f32::NAN.to_le_bytes());
        state.update(decode(0x2736, &body).unwrap(), 20);
        assert_eq!(state.gyro.unwrap().received_monotonic_ns, 10);
        assert_eq!(state.mag.unwrap().received_monotonic_ns, 20);
    }
    #[test]
    fn reject_valid_nan_and_bad_length() {
        let mut body = [0u8; 128];
        body[24..28].copy_from_slice(&1u32.to_le_bytes());
        body[28..32].copy_from_slice(&f32::NAN.to_le_bytes());
        assert!(decode(0x2836, &body).is_err());
        assert!(decode(0x2836, &body[..127]).is_err());
    }
}
