// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kostiantyn Andriiuk <konstantin@andriyuk.com>
//! Device framing and the small, known GET protobuf schema. No opcode scanning.
#[derive(Debug, Clone)]
pub struct Frame {
    pub opcode: u16,
    pub body: Vec<u8>,
}

pub const MAX_CONTROL_FRAME: usize = 1024 * 1024;
pub const READ_CHUNK: usize = 16 * 1024;

pub struct Framer {
    buffer: Vec<u8>,
    limit: usize,
}

impl Framer {
    pub fn new(limit: usize) -> Self {
        Self {
            buffer: Vec::new(),
            limit,
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<Frame>, String> {
        if self.buffer.len().saturating_add(bytes.len()) > self.limit + READ_CHUNK + 6 {
            return Err("device receive buffer limit".into());
        }
        self.buffer.extend_from_slice(bytes);
        let mut cursor = 0;
        let mut frames = Vec::new();
        while self.buffer.len() - cursor >= 6 {
            let header = &self.buffer[cursor..cursor + 6];
            let length = u32::from_be_bytes(header[2..6].try_into().unwrap()) as usize;
            if length > self.limit {
                return Err("oversized device frame".into());
            }
            if self.buffer.len() - cursor < length + 6 {
                break;
            }
            frames.push(Frame {
                opcode: u16::from_be_bytes(header[..2].try_into().unwrap()),
                body: self.buffer[cursor + 6..cursor + 6 + length].to_vec(),
            });
            cursor += length + 6;
        }
        self.buffer.drain(..cursor);
        Ok(frames)
    }
}

pub fn get_request(opcode: u16, transaction: u32) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(12);
    bytes.extend_from_slice(&opcode.to_be_bytes());
    bytes.extend_from_slice(&6u32.to_be_bytes());
    bytes.extend_from_slice(&(transaction | 0x8000_0000).to_be_bytes());
    bytes.extend_from_slice(&[0x18, 0x00]);
    bytes
}

#[derive(Debug, PartialEq)]
pub enum Property {
    Number(u64),
    Bytes(Vec<u8>),
}

enum Field<'a> {
    Number(u64),
    Bytes(&'a [u8]),
    Other,
}

fn varint(bytes: &[u8], cursor: &mut usize) -> Result<u64, String> {
    let mut result = 0u64;
    for index in 0..10 {
        let byte = *bytes.get(*cursor).ok_or("truncated protobuf varint")?;
        *cursor += 1;
        if index == 9 && byte > 1 {
            return Err("protobuf varint overflow".into());
        }
        result |= u64::from(byte & 127) << (index * 7);
        if byte & 128 == 0 {
            return Ok(result);
        }
    }
    Err("protobuf varint overflow".into())
}

fn fields(bytes: &[u8]) -> Result<Vec<(u32, Field<'_>)>, String> {
    let mut cursor = 0;
    let mut result = Vec::new();
    while cursor < bytes.len() {
        // Bound field-count independently of the TCP size limit.
        if result.len() >= 1024 {
            return Err("too many protobuf fields".into());
        }
        let tag = varint(bytes, &mut cursor)?;
        let number = tag >> 3;
        if number == 0 || number > 0x1fff_ffff {
            return Err("invalid protobuf field".into());
        }
        let field = match tag & 7 {
            0 => Field::Number(varint(bytes, &mut cursor)?),
            2 => {
                let length =
                    usize::try_from(varint(bytes, &mut cursor)?).map_err(|_| "protobuf length")?;
                let end = cursor
                    .checked_add(length)
                    .ok_or("protobuf length overflow")?;
                let data = bytes.get(cursor..end).ok_or("truncated protobuf bytes")?;
                cursor = end;
                Field::Bytes(data)
            }
            1 | 5 => {
                let size = if tag & 7 == 1 { 8 } else { 4 };
                let end = cursor + size;
                bytes
                    .get(cursor..end)
                    .ok_or("truncated fixed protobuf field")?;
                cursor = end;
                Field::Other
            }
            _ => return Err("unsupported protobuf wire type".into()),
        };
        result.push((number as u32, field));
    }
    Ok(result)
}

/// Some verified GETs omit numeric field2 when its default value is zero.
pub fn property_value(bytes: &[u8]) -> Result<Property, String> {
    let outer = fields(bytes)?;
    let nested: Vec<_> = outer.iter().filter(|(n, _)| *n == 4).collect();
    if nested.len() != 1 {
        return Err("missing or duplicate property envelope".into());
    }
    let Field::Bytes(inner) = &nested[0].1 else {
        return Err("invalid property envelope".into());
    };
    let mut value = None;
    let mut status = None;
    for (number, field) in fields(inner)? {
        match (number, field) {
            (1, Field::Number(code)) => {
                if status.replace(code).is_some() {
                    return Err("duplicate device status".into());
                }
            }
            (2, Field::Number(n)) => {
                if value.replace(Property::Number(n)).is_some() {
                    return Err("duplicate property value".into());
                }
            }
            (2, Field::Bytes(data)) => {
                if value.replace(Property::Bytes(data.to_vec())).is_some() {
                    return Err("duplicate property value".into());
                }
            }
            (1 | 2, _) => return Err("invalid property field type".into()),
            _ => {}
        }
    }
    if status.unwrap_or(0) != 0 {
        return Err(format!("device returned status {}", status.unwrap()));
    }
    Ok(value.unwrap_or(Property::Number(0)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn split_and_coalesced_frames() {
        let mut f = Framer::new(128);
        let packet = [0x28, 0x36, 0, 0, 0, 2, 9, 8];
        assert!(f.feed(&packet[..3]).unwrap().is_empty());
        let mut rest = packet[3..].to_vec();
        rest.extend_from_slice(&packet);
        let got = f.feed(&rest).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[1].body, [9, 8]);
    }
    #[test]
    fn bounds_and_protobuf_errors() {
        assert!(Framer::new(128)
            .feed(&[0x28, 0x36, 0xff, 0xff, 0xff, 0xff])
            .is_err());
        assert!(property_value(&[0x22, 9, 0x10]).is_err());
        assert!(property_value(&[0x22, 2, 8, 3]).is_err());
        assert!(property_value(&[0x22, 4, 0x10, 1, 0x10, 2]).is_err());
        assert_eq!(property_value(&[0x22, 0]).unwrap(), Property::Number(0));
        assert_eq!(
            property_value(&[0x22, 2, 0x10, 29]).unwrap(),
            Property::Number(29)
        );
        let mut cursor = 0;
        assert!(varint(&[0xff; 11], &mut cursor).is_err());
    }
    #[test]
    fn corrupt_input_never_panics() {
        let mut seed = 17u64;
        for size in 0..512 {
            let mut bytes = vec![0; size];
            for byte in &mut bytes {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                *byte = (seed >> 32) as u8;
            }
            let _ = property_value(&bytes);
            let _ = Framer::new(1024).feed(&bytes);
        }
    }
}
