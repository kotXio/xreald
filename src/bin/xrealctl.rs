// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kostiantyn Andriiuk <konstantin@andriyuk.com>
use serde_json::{json, Value};
use std::io::{self, IsTerminal, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use xreald::{api, daemon};

fn main() {
    if let Err(error) = run() {
        eprintln!("xrealctl: {error}");
        std::process::exit(1);
    }
}
fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let mut socket = daemon::default_socket();
    let mut positional = Vec::new();
    let mut rate = 30u64;
    let mut native = false;
    let mut seconds = None;
    let mut cached = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                println!("xrealctl [--socket PATH] status|capabilities|get NAME [--cached]\n         [--socket PATH] watch imu|buttons|gestures|state [--rate HZ] [--seconds N] [--native]\n         [--socket PATH] set NAME JSON_VALUE (explicitly disabled in this POC)");
                return Ok(());
            }
            "--socket" => socket = PathBuf::from(args.next().ok_or("missing socket path")?),
            "--rate" => {
                rate = args
                    .next()
                    .ok_or("missing rate")?
                    .parse()
                    .map_err(|_| "invalid rate")?
            }
            "--seconds" => {
                seconds = Some(
                    args.next()
                        .ok_or("missing duration")?
                        .parse::<u64>()
                        .map_err(|_| "invalid duration")?,
                )
            }
            "--native" => native = true,
            "--cached" => cached = true,
            value if value.starts_with('-') => return Err(format!("unknown option {value}")),
            _ => positional.push(arg),
        }
    }
    let command = positional
        .first()
        .ok_or("use --help for commands")?
        .as_str();
    let (method, params) = match command {
        "status" | "capabilities" if positional.len() == 1 => (command, json!({})),
        "get" if positional.len() == 2 => ("get", json!({"name":positional[1],"cached":cached})),
        "set" if positional.len() == 3 => (
            "set",
            json!({"name":positional[1],"value":serde_json::from_str::<Value>(&positional[2]).map_err(|_|"invalid JSON value")?}),
        ),
        "watch" if positional.len() == 2 => (
            "subscribe",
            json!({"topics":positional[1].split(',').collect::<Vec<_>>(),"format":if native{"native"}else{"json"},"rate_hz":rate}),
        ),
        _ => return Err("invalid command or arguments; use --help".into()),
    };
    if command != "watch" && (native || seconds.is_some() || rate != 30) {
        return Err("watch options require watch".into());
    }
    if command != "get" && cached {
        return Err("--cached requires get".into());
    }
    if native && io::stdout().is_terminal() {
        return Err("redirect native output to a file or pipe".into());
    }
    let mut stream =
        UnixStream::connect(&socket).map_err(|e| format!("{}: {e}", socket.display()))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| e.to_string())?;
    api::send(
        &mut stream,
        &json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}),
    )
    .map_err(|e| e.to_string())?;
    let (kind, body) = api::receive(&mut stream).map_err(|e| e.to_string())?;
    if kind != api::JSON_KIND {
        return Err("expected JSON response".into());
    }
    let response: Value = serde_json::from_slice(&body).map_err(|e| e.to_string())?;
    if let Some(error) = response.get("error") {
        return Err(error.to_string());
    }
    if response.get("id") != Some(&json!(1)) {
        return Err("unexpected response ID".into());
    }
    if command != "watch" {
        println!(
            "{}",
            serde_json::to_string_pretty(&response["result"]).unwrap()
        );
        return Ok(());
    }
    eprintln!("{}", serde_json::to_string(&response["result"]).unwrap());
    stream
        .set_read_timeout(Some(Duration::from_millis(200)))
        .map_err(|e| e.to_string())?;
    let start = Instant::now();
    let mut decoder = api::Decoder::default();
    let mut bytes = [0; 16 * 1024];
    let stdout = io::stdout();
    let mut output = stdout.lock();
    while seconds.is_none_or(|seconds| start.elapsed() < Duration::from_secs(seconds)) {
        match stream.read(&mut bytes) {
            Ok(0) => return Err("daemon closed subscription".into()),
            Ok(size) => {
                for (kind, body) in decoder.feed(&bytes[..size])? {
                    if kind == api::NATIVE_KIND {
                        if !native {
                            return Err("unexpected native packet".into());
                        }
                        output
                            .write_all(&api::packet(kind, &body)?)
                            .map_err(|e| e.to_string())?;
                    } else if kind == api::JSON_KIND {
                        let message: Value =
                            serde_json::from_slice(&body).map_err(|e| e.to_string())?;
                        if native {
                            eprintln!("{message}");
                        } else {
                            writeln!(output, "{message}").map_err(|e| e.to_string())?;
                        }
                    } else {
                        return Err("unknown API packet kind".into());
                    }
                }
            }
            Err(e)
                if e.kind() == io::ErrorKind::WouldBlock
                    || e.kind() == io::ErrorKind::TimedOut
                    || e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.to_string()),
        }
        output.flush().map_err(|e| e.to_string())?;
    }
    Ok(())
}
