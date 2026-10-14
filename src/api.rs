// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kostiantyn Andriiuk <konstantin@andriyuk.com>
use serde_json::{json, Value};
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;

pub const MAX_MESSAGE: usize = 256 * 1024;
pub const JSON_KIND: u8 = 1;
pub const NATIVE_KIND: u8 = 2;

pub fn packet(kind: u8, body: &[u8]) -> Result<Vec<u8>, String> {
    if body.len() + 1 > MAX_MESSAGE {
        return Err("API message size limit".into());
    }
    let mut result = Vec::with_capacity(body.len() + 5);
    result.extend_from_slice(&((body.len() + 1) as u32).to_be_bytes());
    result.push(kind);
    result.extend_from_slice(body);
    Ok(result)
}
pub fn json_packet(value: &Value) -> Result<Vec<u8>, String> {
    packet(
        JSON_KIND,
        &serde_json::to_vec(value).map_err(|e| e.to_string())?,
    )
}
pub fn response(id: Value, result: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"result":result})
}
pub fn error(id: Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}
pub fn event(name: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0","method":name,"params":params})
}

#[derive(Default)]
pub struct Decoder {
    buffer: Vec<u8>,
}
impl Decoder {
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<(u8, Vec<u8>)>, String> {
        if self.buffer.len().saturating_add(bytes.len()) > MAX_MESSAGE + 16 * 1024 + 4 {
            return Err("API receive buffer limit".into());
        }
        self.buffer.extend_from_slice(bytes);
        let mut cursor = 0;
        let mut output = Vec::new();
        while self.buffer.len() - cursor >= 4 {
            let length =
                u32::from_be_bytes(self.buffer[cursor..cursor + 4].try_into().unwrap()) as usize;
            if length == 0 || length > MAX_MESSAGE {
                return Err("invalid API frame length".into());
            }
            if self.buffer.len() - cursor < length + 4 {
                break;
            }
            output.push((
                self.buffer[cursor + 4],
                self.buffer[cursor + 5..cursor + 4 + length].to_vec(),
            ));
            cursor += length + 4;
        }
        self.buffer.drain(..cursor);
        Ok(output)
    }
}
pub fn send(stream: &mut UnixStream, value: &Value) -> io::Result<()> {
    let bytes = json_packet(value).map_err(io::Error::other)?;
    stream.write_all(&bytes)
}
pub fn receive(stream: &mut UnixStream) -> io::Result<(u8, Vec<u8>)> {
    let mut header = [0; 4];
    stream.read_exact(&mut header)?;
    let length = u32::from_be_bytes(header) as usize;
    if length == 0 || length > MAX_MESSAGE {
        return Err(io::Error::other("invalid API frame length"));
    }
    let mut body = vec![0; length];
    stream.read_exact(&mut body)?;
    Ok((body[0], body[1..].to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn framed_json_and_binary_are_unambiguous() {
        let json = json_packet(&response(json!(7), json!({"ok":true}))).unwrap();
        let native = packet(NATIVE_KIND, b"XRI1\0\x01").unwrap();
        let mut decoder = Decoder::default();
        assert!(decoder.feed(&json[..2]).unwrap().is_empty());
        let mut bytes = json[2..].to_vec();
        bytes.extend(native);
        let got = decoder.feed(&bytes).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[1].0, NATIVE_KIND);
        assert_eq!(serde_json::from_slice::<Value>(&got[0].1).unwrap()["id"], 7);
        assert!(decoder.feed(&[0xff; 4]).is_err());
    }
}
