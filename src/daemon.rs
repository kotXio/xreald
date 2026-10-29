// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kostiantyn Andriiuk <konstantin@andriyuk.com>
use crate::{api, catalog, device, gestures, imu, keys, protocol, wearing};
use serde_json::{json, Value};
use std::collections::{BTreeMap, VecDeque};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpStream};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

pub const MAX_CLIENTS: usize = 8;
const MAX_QUEUE: usize = 512 * 1024;
const MAX_PENDING: usize = 16;
const SECOND: u64 = 1_000_000_000;
static STOP: AtomicBool = AtomicBool::new(false);
extern "C" fn stop(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}

pub fn default_socket() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("/tmp/xreald-{}", unsafe { libc::geteuid() })))
        .join("xreald/control.sock")
}

#[derive(Clone)]
pub struct Config {
    pub socket: PathBuf,
    pub peer: Ipv4Addr,
    pub source: Option<Ipv4Addr>,
    pub discovery: bool,
    pub imu_port: u16,
    pub control_port: u16,
    pub imu_batch_ms: u64,
    pub run_for: Option<u64>,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            socket: default_socket(),
            peer: Ipv4Addr::new(169, 254, 2, 1),
            source: None,
            discovery: true,
            imu_port: 52998,
            control_port: 52999,
            imu_batch_ms: 8,
            run_for: None,
        }
    }
}
impl Config {
    pub fn arguments(args: impl Iterator<Item = String>) -> Result<Option<Self>, String> {
        let mut config = Self::default();
        let mut args = args;
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--help" | "-h" => {
                    println!("xreald [--socket PATH] [--peer IPv4] [--source IPv4] [--no-discovery]\n       [--imu-port PORT] [--control-port PORT] [--imu-batch-ms 0..20] [--run-for SECONDS]\nDefault: discover only USB3318:043e NCM/input; known GETs; batch IMU up to8ms (0 for immediate).");
                    return Ok(None);
                }
                "--version" => {
                    println!("xreald {}", env!("CARGO_PKG_VERSION"));
                    return Ok(None);
                }
                "--socket" => config.socket = args.next().ok_or("missing socket path")?.into(),
                "--peer" => {
                    config.peer = args
                        .next()
                        .ok_or("missing peer")?
                        .parse()
                        .map_err(|_| "invalid peer IPv4")?
                }
                "--source" => {
                    config.source = Some(
                        args.next()
                            .ok_or("missing source")?
                            .parse()
                            .map_err(|_| "invalid source IPv4")?,
                    )
                }
                "--imu-port" => {
                    config.imu_port = args
                        .next()
                        .ok_or("missing port")?
                        .parse()
                        .map_err(|_| "invalid port")?
                }
                "--control-port" => {
                    config.control_port = args
                        .next()
                        .ok_or("missing port")?
                        .parse()
                        .map_err(|_| "invalid port")?
                }
                "--run-for" => {
                    config.run_for = Some(
                        args.next()
                            .ok_or("missing duration")?
                            .parse()
                            .map_err(|_| "invalid duration")?,
                    )
                }
                "--imu-batch-ms" => {
                    config.imu_batch_ms = args
                        .next()
                        .ok_or("missing batch interval")?
                        .parse()
                        .map_err(|_| "invalid batch interval")?;
                    if config.imu_batch_ms > 20 {
                        return Err("imu-batch-ms must be0..20".into());
                    }
                }
                "--no-discovery" => config.discovery = false,
                _ => return Err(format!("unknown option {arg}")),
            }
        }
        if config.imu_port == 0 || config.control_port == 0 {
            return Err("port must be nonzero".into());
        }
        Ok(Some(config))
    }
}

struct Output {
    bytes: Vec<u8>,
    offset: usize,
    telemetry: bool,
}
#[derive(Default)]
struct Outbox {
    items: VecDeque<Output>,
    bytes: usize,
}
impl Outbox {
    fn push(&mut self, bytes: Vec<u8>, telemetry: bool, replace: bool) -> Result<u64, ()> {
        let mut dropped = 0;
        if replace {
            self.items.retain(|item| {
                if item.telemetry && item.offset == 0 {
                    self.bytes -= item.bytes.len();
                    dropped += 1;
                    false
                } else {
                    true
                }
            });
        }
        if self.bytes.saturating_add(bytes.len()) > MAX_QUEUE {
            return Err(());
        }
        self.bytes += bytes.len();
        self.items.push_back(Output {
            bytes,
            offset: 0,
            telemetry,
        });
        Ok(dropped)
    }
    fn flush(&mut self, stream: &mut impl Write) -> io::Result<()> {
        let mut budget = 64 * 1024;
        while budget > 0 {
            if self.items.is_empty() {
                break;
            }
            let mut remaining = budget;
            let slices: Vec<_> = self
                .items
                .iter()
                .take(64)
                .filter_map(|item| {
                    let size = (item.bytes.len() - item.offset).min(remaining);
                    remaining -= size;
                    (size > 0)
                        .then(|| io::IoSlice::new(&item.bytes[item.offset..item.offset + size]))
                })
                .collect();
            match stream.write_vectored(&slices) {
                Ok(0) => return Err(io::Error::new(io::ErrorKind::WriteZero, "socket closed")),
                Ok(size) => {
                    self.bytes -= size;
                    budget -= size;
                    let mut consumed = size;
                    while consumed > 0 {
                        let item = self.items.front_mut().unwrap();
                        let count = consumed.min(item.bytes.len() - item.offset);
                        item.offset += count;
                        consumed -= count;
                        if item.offset == item.bytes.len() {
                            self.items.pop_front();
                        }
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}

struct Subscription {
    imu: bool,
    buttons: bool,
    gestures: bool,
    wearing: bool,
    state: bool,
    native: bool,
    interval: u64,
}
struct Client {
    socket: UnixStream,
    decoder: api::Decoder,
    out: Outbox,
    subscription: Option<Subscription>,
    next_imu: u64,
    last_sequence: u64,
    next_state: u64,
    dead: bool,
}

struct Link {
    socket: Option<TcpStream>,
    online: bool,
    parser: protocol::Framer,
    out: Outbox,
    retry: u64,
    backoff: u64,
    connect_deadline: u64,
    last_error: Option<String>,
}
impl Link {
    fn new(limit: usize) -> Self {
        Self {
            socket: None,
            online: false,
            parser: protocol::Framer::new(limit),
            out: Outbox::default(),
            retry: 0,
            backoff: 250_000_000,
            connect_deadline: 0,
            last_error: None,
        }
    }
    fn fd(&self) -> Option<i32> {
        self.socket.as_ref().map(AsRawFd::as_raw_fd)
    }
    fn disconnect(&mut self, now: u64, reason: &str) {
        self.socket = None;
        self.online = false;
        self.out = Outbox::default();
        self.last_error = Some(reason.chars().take(128).collect());
        self.retry = now + self.backoff;
        self.backoff = (self.backoff * 2).min(4 * SECOND);
    }
    fn attempt(&mut self, address: SocketAddrV4, source: Option<Ipv4Addr>, now: u64, limit: usize) {
        if self.socket.is_some() || now < self.retry {
            return;
        }
        match device::connect(address, source) {
            Ok((socket, online)) => {
                self.socket = Some(socket);
                self.online = online;
                self.connect_deadline = now + 2 * SECOND;
                self.parser = protocol::Framer::new(limit);
                self.out = Outbox::default();
                if online {
                    self.backoff = 250_000_000;
                    self.last_error = None;
                }
            }
            Err(e) => self.disconnect(now, &e.to_string()),
        }
    }
    fn read(&mut self) -> Result<Vec<protocol::Frame>, String> {
        let mut bytes = [0; protocol::READ_CHUNK];
        let stream = self.socket.as_mut().ok_or("disconnected")?;
        match stream.read(&mut bytes) {
            Ok(0) => Err("TCP EOF".into()),
            Ok(size) => self.parser.feed(&bytes[..size]),
            Err(e)
                if e.kind() == io::ErrorKind::WouldBlock
                    || e.kind() == io::ErrorKind::Interrupted =>
            {
                Ok(Vec::new())
            }
            Err(e) => Err(e.to_string()),
        }
    }
}

struct Pending {
    client: u64,
    id: Value,
    entry: catalog::Entry,
    deadline: u64,
}
struct InputReader {
    file: File,
    decoder: keys::EvdevDecoder,
}
#[derive(Default)]
struct Metrics {
    imu_frames: u64,
    imu_invalid: u64,
    control_unknown: u64,
    control_errors: u64,
    api_errors: u64,
    clients_evicted: u64,
    telemetry_replaced: u64,
    input_gaps: u64,
    buttons: u64,
    gestures: u64,
    reconnects: u64,
    samples: [u64; 4],
    rate_base: [u64; 4],
    rates: [f64; 4],
    rate_time: u64,
}

pub struct Server {
    config: Config,
    listener: UnixListener,
    socket_inode: u64,
    start: Instant,
    clients: BTreeMap<u64, Client>,
    next_client: u64,
    control: Link,
    imu: Link,
    discovery: device::Discovery,
    discovery_time: u64,
    next_imu_read: u64,
    watcher: Option<File>,
    network_watcher: Option<File>,
    inputs: BTreeMap<PathBuf, InputReader>,
    input_errors: BTreeMap<PathBuf, String>,
    holds: keys::Holds,
    quick_gestures: gestures::QuickGestures,
    snapshot: imu::Snapshot,
    wearing: wearing::Wearing,
    pending: BTreeMap<u32, Pending>,
    next_transaction: u32,
    cache: BTreeMap<String, Value>,
    parameter_errors: BTreeMap<String, String>,
    metrics: Metrics,
}
fn private_directory(path: &Path) -> io::Result<()> {
    if !path.exists() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)?;
    }
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "socket directory must be owned by this user and mode0700",
        ));
    }
    Ok(())
}
impl Server {
    pub fn new(config: Config) -> io::Result<Self> {
        let parent = config
            .socket
            .parent()
            .ok_or_else(|| io::Error::other("socket needs a directory"))?;
        private_directory(parent)?;
        match fs::symlink_metadata(&config.socket) {
            Ok(metadata) => {
                if !metadata.file_type().is_socket() || metadata.uid() != unsafe { libc::geteuid() }
                {
                    return Err(io::Error::other("refuse to replace a non-owned socket"));
                }
                if UnixStream::connect(&config.socket).is_ok() {
                    return Err(io::Error::new(
                        io::ErrorKind::AddrInUse,
                        "daemon already running",
                    ));
                }
                fs::remove_file(&config.socket)?;
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let listener = UnixListener::bind(&config.socket)?;
        listener.set_nonblocking(true)?;
        fs::set_permissions(&config.socket, fs::Permissions::from_mode(0o600))?;
        let socket_inode = fs::symlink_metadata(&config.socket)?.ino();
        Ok(Self {
            config,
            listener,
            socket_inode,
            start: Instant::now(),
            clients: BTreeMap::new(),
            next_client: 0,
            control: Link::new(protocol::MAX_CONTROL_FRAME),
            imu: Link::new(128),
            discovery: device::Discovery::default(),
            discovery_time: 0,
            next_imu_read: 0,
            watcher: device::watcher(),
            network_watcher: device::network_watcher(),
            inputs: BTreeMap::new(),
            input_errors: BTreeMap::new(),
            holds: keys::Holds::default(),
            quick_gestures: gestures::QuickGestures::default(),
            snapshot: imu::Snapshot::default(),
            wearing: wearing::Wearing::default(),
            pending: BTreeMap::new(),
            next_transaction: 1,
            cache: BTreeMap::new(),
            parameter_errors: BTreeMap::new(),
            metrics: Metrics::default(),
        })
    }
    fn now(&self) -> u64 {
        self.start.elapsed().as_nanos().min(u64::MAX as u128) as u64
    }
    fn wants_imu(&self) -> bool {
        self.clients
            .values()
            .any(|c| !c.dead && c.subscription.as_ref().is_some_and(|s| s.imu))
    }
    fn source(&self) -> Option<Ipv4Addr> {
        self.config.source.or(self.discovery.source)
    }
    fn device_available(&self) -> bool {
        !self.config.discovery || self.source().is_some()
    }
    fn queue(&mut self, client: u64, value: Value, telemetry: bool, replace: bool) {
        let packet = api::json_packet(&value).unwrap_or_else(|_| {
            api::json_packet(&api::error(
                value.get("id").cloned().unwrap_or(Value::Null),
                -32002,
                "response exceeds API limit",
            ))
            .unwrap()
        });
        self.queue_packet(client, packet, telemetry, replace);
    }
    fn queue_packet(&mut self, id: u64, bytes: Vec<u8>, telemetry: bool, replace: bool) {
        if let Some(client) = self.clients.get_mut(&id) {
            if client.dead {
                return;
            }
            match client.out.push(bytes, telemetry, replace) {
                Ok(dropped) => self.metrics.telemetry_replaced += dropped,
                Err(_) => {
                    client.dead = true;
                    self.metrics.clients_evicted += 1;
                }
            }
        }
    }
    fn notify(&mut self, name: &str, data: Value) {
        // A buttons-only consumer must see loss of input, even without health telemetry.
        let button_lifecycle = name.starts_with("input_")
            || (name == "connection"
                && data.get("channel").and_then(Value::as_str) == Some("control"));
        let ids: Vec<_> = self
            .clients
            .iter()
            .filter(|(_, c)| {
                c.subscription
                    .as_ref()
                    .is_some_and(|s| s.state || ((s.buttons || s.gestures) && button_lifecycle))
            })
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.queue(id, api::event(name, data.clone()), false, false);
        }
    }
    fn button(&mut self, mut button: keys::Button) {
        self.holds.apply(&mut button);
        let gestures = self.quick_gestures.accept(&button);
        self.metrics.buttons += 1;
        let mut data = serde_json::to_value(button).unwrap();
        data["sequence"] = json!(self.metrics.buttons);
        let ids: Vec<_> = self
            .clients
            .iter()
            .filter(|(_, c)| c.subscription.as_ref().is_some_and(|s| s.buttons))
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.queue(id, api::event("button", data.clone()), false, false);
        }
        for gesture in gestures {
            self.gesture(gesture);
        }
    }
    fn gesture(&mut self, gesture: gestures::Gesture) {
        self.metrics.gestures += 1;
        let mut data = serde_json::to_value(gesture).unwrap();
        data["sequence"] = json!(self.metrics.gestures);
        let ids: Vec<_> = self
            .clients
            .iter()
            .filter(|(_, c)| c.subscription.as_ref().is_some_and(|s| s.gestures))
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.queue(id, api::event("button_gesture", data.clone()), false, false);
        }
    }
    fn disconnect(&mut self, control: bool, now: u64, reason: &str) {
        if control {
            self.control.disconnect(now, reason);
            self.cache.clear();
            self.wearing = wearing::Wearing::default();
            self.publish_wearing();
            self.holds.reset_source("control");
            self.quick_gestures.reset();
            let pending = std::mem::take(&mut self.pending);
            for (_, request) in pending {
                self.queue(
                    request.client,
                    api::error(request.id, -32010, "control connection lost"),
                    false,
                    false,
                );
            }
        } else {
            self.imu.disconnect(now, reason);
        }
        self.notify("connection",json!({"channel":if control{"control"}else{"imu"},"online":false,"reason":reason.chars().take(128).collect::<String>()}));
    }
    fn connected(&mut self, control: bool, now: u64) {
        if control {
            self.wearing = wearing::Wearing::default();
            self.publish_wearing();
        }
        if !control {
            let epoch = self.snapshot.epoch + 1;
            self.snapshot = imu::Snapshot {
                epoch,
                ..Default::default()
            };
            self.metrics.samples = [0; 4];
            self.metrics.rate_base = [0; 4];
            self.metrics.rates = [0.; 4];
            self.metrics.rate_time = now;
        }
        self.metrics.reconnects += 1;
        self.notify("connection",json!({"channel":if control{"control"}else{"imu"},"online":true,"imu_epoch":self.snapshot.epoch}));
    }
    fn refresh(&mut self, now: u64) {
        if now < self.discovery_time {
            return;
        }
        self.discovery_time = now + 5 * SECOND;
        let found = device::discover();
        if self.config.discovery
            && (found.source != self.discovery.source
                || found.usb_device != self.discovery.usb_device)
        {
            if self.control.socket.is_some() {
                self.disconnect(true, now, "USB network changed");
            }
            if self.imu.socket.is_some() {
                self.disconnect(false, now, "USB network changed");
            }
            self.control.retry = now;
            self.imu.retry = now;
        }
        let old = self.inputs.len();
        self.inputs.retain(|path, _| found.input.contains(path));
        if self.inputs.len() != old {
            self.holds.reset_source("evdev");
            self.notify("input_reset", json!({"reason":"device removed"}));
        }
        for path in &found.input {
            if !self.inputs.contains_key(path) {
                match device::input_open(path) {
                    Ok(file) => {
                        self.input_errors.remove(path);
                        self.inputs.insert(
                            path.clone(),
                            InputReader {
                                file,
                                decoder: keys::EvdevDecoder::default(),
                            },
                        );
                    }
                    Err(e) => {
                        self.input_errors.insert(path.clone(), e.to_string());
                    }
                }
            }
        }
        self.input_errors
            .retain(|path, _| found.input.contains(path));
        self.discovery = found;
    }
    fn status(&self, now: u64) -> Value {
        let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
        let usage_ok = unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) } == 0;
        let cpu_ns = if usage_ok {
            Some(
                ((usage.ru_utime.tv_sec as u64 + usage.ru_stime.tv_sec as u64) * SECOND)
                    + (usage.ru_utime.tv_usec as u64 + usage.ru_stime.tv_usec as u64) * 1000,
            )
        } else {
            None
        };
        #[cfg(target_os = "linux")]
        let resident_bytes = fs::read_to_string("/proc/self/statm")
            .ok()
            .and_then(|s| {
                s.split_whitespace()
                    .nth(1)
                    .and_then(|x| x.parse::<u64>().ok())
            })
            .map(|pages| pages * unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64);
        #[cfg(not(target_os = "linux"))]
        let resident_bytes: Option<u64> = None;
        let parameter_results:serde_json::Map<String,Value>=self.cache.iter().map(|(name,cached)|{
            let result=if name=="factory.config" {json!({"available":true,"received_monotonic_ns":cached["received_monotonic_ns"],"full_value":"use get factory.config"})} else {cached.clone()};
            (name.clone(),result)
        }).collect();
        json!({"api_version":1,"daemon_version":env!("CARGO_PKG_VERSION"),"uptime_ns":now,"transport":{"mode":if self.config.discovery{"usb_discovery"}else{"manual"},"peer":self.config.peer,"source":self.source(),"imu_port":self.config.imu_port,"control_port":self.config.control_port,"imu_batch_ms":self.config.imu_batch_ms},
            "discovery":self.discovery,"control":{"online":self.control.online,"error":self.control.last_error},
            "imu":{"online":self.imu.online,"fresh":self.snapshot.fresh(now)&&self.imu.online,"epoch":self.snapshot.epoch,"sequence":self.snapshot.sequence,"error":self.imu.last_error,"subscribers":self.clients.values().filter(|c|c.subscription.as_ref().is_some_and(|s|s.imu)).count()},
            "input":{"readable_nodes":self.inputs.keys().collect::<Vec<_>>(),"errors":self.input_errors,"exclusive_grab":false,"button_events":self.metrics.buttons,"gesture_events":self.metrics.gestures},
            "metrics":{"cpu_total_ns":cpu_ns,"resident_bytes":resident_bytes,"imu_frames":self.metrics.imu_frames,"imu_invalid":self.metrics.imu_invalid,"rates_hz":{"gyro":self.metrics.rates[0],"accel":self.metrics.rates[1],"mag":self.metrics.rates[2],"temperature":self.metrics.rates[3]},"control_unknown":self.metrics.control_unknown,"control_errors":self.metrics.control_errors,"api_errors":self.metrics.api_errors,"clients_evicted":self.metrics.clients_evicted,"telemetry_replaced":self.metrics.telemetry_replaced,"input_gaps":self.metrics.input_gaps,"connections":self.metrics.reconnects,"pending_requests":self.pending.len()},
            "wearing":self.wearing.value(),"parameter_results":parameter_results,"parameter_errors":self.parameter_errors,"set_enabled":false})
    }
    fn request(&mut self, client: u64, request: Value, now: u64) {
        if self.clients.get(&client).is_none_or(|c| c.dead) {
            return;
        }
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let method = request.get("method").and_then(Value::as_str);
        if request.get("id").is_none()
            || request.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
            || method.is_none()
            || !(id.is_null() || id.is_number() || id.is_string())
        {
            self.queue(
                client,
                api::error(Value::Null, -32600, "invalid JSON-RPC request"),
                false,
                false,
            );
            return;
        }
        let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
        if !params.is_object() {
            self.queue(
                client,
                api::error(id, -32602, "params must be an object"),
                false,
                false,
            );
            return;
        }
        if method == Some("get") && params.get("cached").is_some_and(|v| !v.is_boolean()) {
            self.queue(
                client,
                api::error(id, -32602, "cached must be boolean"),
                false,
                false,
            );
            return;
        }
        if method == Some("subscribe")
            && (params.get("rate_hz").is_some_and(|v| v.as_u64().is_none())
                || params.get("format").is_some_and(|v| !v.is_string()))
        {
            self.queue(
                client,
                api::error(id, -32602, "invalid rate_hz or format type"),
                false,
                false,
            );
            return;
        }

        match method.unwrap() {
            "status" => self.queue(client, api::response(id, self.status(now)), false, false),
            "capabilities" => {
                let mut caps = catalog::capabilities();
                caps["live_results"] = json!({"verified":self.cache.keys().collect::<Vec<_>>(),"errors":self.parameter_errors});
                self.queue(client, api::response(id, caps), false, false);
            }
            "get" => {
                let entry = params
                    .get("name")
                    .and_then(Value::as_str)
                    .and_then(catalog::find);
                let Some(entry) = entry else {
                    self.queue(
                        client,
                        api::error(id, -32602, "parameter not in known GET allowlist"),
                        false,
                        false,
                    );
                    return;
                };
                if params.get("cached").and_then(Value::as_bool) == Some(true) {
                    if let Some(value) = self.cache.get(entry.name) {
                        self.queue(
                            client,
                            api::response(
                                id,
                                json!({"name":entry.name,"value":value["value"],"cached":true,"received_monotonic_ns":value["received_monotonic_ns"]}),
                            ),
                            false,
                            false,
                        );
                        return;
                    }
                }
                if !self.control.online {
                    self.queue(
                        client,
                        api::error(id, -32010, "XREAL control unavailable"),
                        false,
                        false,
                    );
                    return;
                }
                if self.pending.len() >= MAX_PENDING {
                    self.queue(
                        client,
                        api::error(id, -32011, "control request queue full"),
                        false,
                        false,
                    );
                    return;
                }
                let transaction = self.next_transaction;
                self.next_transaction = if transaction == 0x7fff_ffff {
                    1
                } else {
                    transaction + 1
                };
                if self.pending.contains_key(&transaction) {
                    self.queue(
                        client,
                        api::error(id, -32011, "transaction busy"),
                        false,
                        false,
                    );
                    return;
                }
                let bytes = protocol::get_request(entry.opcode, transaction);
                if self.control.out.push(bytes, false, false).is_err() {
                    self.queue(
                        client,
                        api::error(id, -32011, "control output queue full"),
                        false,
                        false,
                    );
                    return;
                }
                self.pending.insert(
                    transaction,
                    Pending {
                        client,
                        id,
                        entry,
                        deadline: now + 4 * SECOND,
                    },
                );
            }
            "set" => self.queue(
                client,
                api::error(
                    id,
                    -32030,
                    "SET operations are unqualified on this1S and disabled in POC",
                ),
                false,
                false,
            ),
            "subscribe" => {
                let topics = params.get("topics").and_then(Value::as_array);
                let Some(topics) = topics else {
                    self.queue(
                        client,
                        api::error(id, -32602, "topics array required"),
                        false,
                        false,
                    );
                    return;
                };
                if topics.is_empty()
                    || topics.iter().any(|v| {
                        !matches!(
                            v.as_str(),
                            Some("imu" | "buttons" | "gestures" | "wearing" | "state")
                        )
                    })
                {
                    self.queue(
                        client,
                        api::error(id, -32602, "unknown subscription topic"),
                        false,
                        false,
                    );
                    return;
                }
                let native = match params.get("format").and_then(Value::as_str) {
                    None | Some("json") => false,
                    Some("native") => true,
                    _ => {
                        self.queue(
                            client,
                            api::error(id, -32602, "format must be json or native"),
                            false,
                            false,
                        );
                        return;
                    }
                };
                let rate = params.get("rate_hz").and_then(Value::as_u64).unwrap_or(30);
                if rate == 0 || rate > 240 {
                    self.queue(
                        client,
                        api::error(id, -32602, "rate_hz must be1..240"),
                        false,
                        false,
                    );
                    return;
                }
                let imu = topics.iter().any(|v| v == "imu");
                if native && !imu {
                    self.queue(
                        client,
                        api::error(id, -32602, "native format requires imu topic"),
                        false,
                        false,
                    );
                    return;
                }
                if let Some(c) = self.clients.get_mut(&client) {
                    c.subscription = Some(Subscription {
                        imu,
                        buttons: topics.iter().any(|v| v == "buttons"),
                        gestures: topics.iter().any(|v| v == "gestures"),
                        wearing: topics.iter().any(|v| v == "wearing"),
                        state: topics.iter().any(|v| v == "state"),
                        native,
                        interval: SECOND / rate,
                    });
                    c.next_imu = now;
                    c.next_state = now;
                    c.last_sequence = 0;
                }
                self.queue(client,api::response(id,json!({"subscribed":topics,"format":if native{"native"}else{"json"},"rate_hz":if native{Value::Null}else{json!(rate)}})),false,false);
                if topics.iter().any(|v| v == "wearing") {
                    self.queue(
                        client,
                        api::event("wearing", self.wearing.value()),
                        false,
                        false,
                    );
                }
            }
            "unsubscribe" => {
                if let Some(c) = self.clients.get_mut(&client) {
                    c.subscription = None;
                }
                self.queue(
                    client,
                    api::response(id, json!({"unsubscribed":true})),
                    false,
                    false,
                );
            }
            _ => self.queue(
                client,
                api::error(id, -32601, "unknown method"),
                false,
                false,
            ),
        }
    }
    fn publish_wearing(&mut self) {
        let ids: Vec<_> = self
            .clients
            .iter()
            .filter(|(_, c)| c.subscription.as_ref().is_some_and(|s| s.wearing))
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.queue(
                id,
                api::event("wearing", self.wearing.value()),
                false,
                false,
            );
        }
    }
    fn control_frame(&mut self, frame: protocol::Frame, now: u64) {
        if frame.opcode == 0x273d {
            if !self.wearing.accept(&frame.body, now) {
                self.metrics.control_errors += 1;
            }
            self.publish_wearing();
            return;
        }
        if frame.opcode == 0x272e {
            match keys::tcp(&frame.body, now) {
                Ok(button) => self.button(button),
                Err(_) => self.metrics.control_errors += 1,
            }
            return;
        }
        if frame.body.len() < 4 {
            self.metrics.control_unknown += 1;
            return;
        }
        let transaction = u32::from_be_bytes(frame.body[..4].try_into().unwrap());
        let Some(p) = self.pending.get(&transaction) else {
            self.metrics.control_unknown += 1;
            return;
        };
        if p.entry.opcode != frame.opcode {
            self.metrics.control_unknown += 1;
            return;
        }
        let pending = self.pending.remove(&transaction).unwrap();
        match protocol::property_value(&frame.body[4..])
            .and_then(|v| catalog::value(pending.entry, v))
        {
            Ok(value) => {
                self.parameter_errors.remove(pending.entry.name);
                self.cache.insert(
                    pending.entry.name.into(),
                    json!({"value":value.clone(),"received_monotonic_ns":now}),
                );
                self.queue(pending.client,api::response(pending.id,json!({"name":pending.entry.name,"value":value,"cached":false,"received_monotonic_ns":now})),false,false);
            }
            Err(e) => {
                self.metrics.control_errors += 1;
                self.parameter_errors
                    .insert(pending.entry.name.into(), e.clone());
                self.queue(
                    pending.client,
                    api::error(pending.id, -32020, &e),
                    false,
                    false,
                );
            }
        }
    }
    fn imu_frame(&mut self, frame: protocol::Frame, now: u64) {
        let sample = match imu::decode(frame.opcode, &frame.body) {
            Ok(sample) => sample,
            Err(_) => {
                self.metrics.imu_invalid += 1;
                return;
            }
        };
        self.metrics.imu_frames += 1;
        for (i, bit) in [1, 2, 4, 8].iter().enumerate() {
            if sample.mask & bit != 0 {
                self.metrics.samples[i] += 1;
            }
        }
        self.snapshot.update(sample, now);
        let ids: Vec<_> = self
            .clients
            .iter()
            .filter(|(_, c)| c.subscription.as_ref().is_some_and(|s| s.imu && s.native))
            .map(|(id, _)| *id)
            .collect();
        if !ids.is_empty() {
            let mut body = Vec::with_capacity(162);
            body.extend_from_slice(b"XRI1");
            for value in [self.snapshot.epoch, self.snapshot.sequence, now] {
                body.extend_from_slice(&value.to_be_bytes());
            }
            body.extend_from_slice(&frame.opcode.to_be_bytes());
            body.extend_from_slice(&(frame.body.len() as u32).to_be_bytes());
            body.extend(frame.body);
            let packet = api::packet(api::NATIVE_KIND, &body).unwrap();
            for id in ids {
                self.queue_packet(id, packet.clone(), false, false);
            }
        }
    }
    fn timers(&mut self, now: u64) {
        self.refresh(now);
        for gesture in self.quick_gestures.flush(now) {
            self.gesture(gesture);
        }
        let wants = self.wants_imu();
        if !wants && self.imu.socket.is_some() {
            self.disconnect(false, now, "no telemetry subscribers");
            self.imu.retry = now;
            self.metrics.rates = [0.; 4];
        }
        if self.device_available() {
            let source = self.source();
            let had_control = self.control.online;
            self.control.attempt(
                SocketAddrV4::new(self.config.peer, self.config.control_port),
                source,
                now,
                protocol::MAX_CONTROL_FRAME,
            );
            if !had_control && self.control.online {
                self.connected(true, now);
            }
            if wants {
                let had_imu = self.imu.online;
                self.imu.attempt(
                    SocketAddrV4::new(self.config.peer, self.config.imu_port),
                    source,
                    now,
                    128,
                );
                if !had_imu && self.imu.online {
                    self.connected(false, now);
                }
            }
        }
        for control in [true, false] {
            let link = if control { &self.control } else { &self.imu };
            if link.socket.is_some() && !link.online && now >= link.connect_deadline {
                self.disconnect(control, now, "connect timeout");
            }
        }
        let expired: Vec<_> = self
            .pending
            .iter()
            .filter(|(_, p)| p.deadline <= now)
            .map(|(tx, _)| *tx)
            .collect();
        for tx in expired {
            let p = self.pending.remove(&tx).unwrap();
            self.parameter_errors
                .insert(p.entry.name.into(), "timeout".into());
            self.queue(
                p.client,
                api::error(p.id, -32012, "control GET timeout"),
                false,
                false,
            );
        }
        if self.imu.online && now.saturating_sub(self.metrics.rate_time) >= SECOND {
            let seconds = (now - self.metrics.rate_time) as f64 / SECOND as f64;
            for i in 0..4 {
                self.metrics.rates[i] =
                    (self.metrics.samples[i] - self.metrics.rate_base[i]) as f64 / seconds;
            }
            self.metrics.rate_base = self.metrics.samples;
            self.metrics.rate_time = now;
        }
        let mut snapshots = Vec::new();
        let mut statuses = Vec::new();
        for (id, c) in &mut self.clients {
            if let Some(s) = &c.subscription {
                if s.imu && !s.native && c.next_imu <= now {
                    c.next_imu = now + s.interval;
                    if self.snapshot.sequence != c.last_sequence {
                        c.last_sequence = self.snapshot.sequence;
                        snapshots.push(*id);
                    }
                }
                if s.state && c.next_state <= now {
                    c.next_state = now + SECOND;
                    statuses.push(*id);
                }
            }
        }
        if !snapshots.is_empty() {
            let data = json!({"snapshot":self.snapshot,"fresh":self.snapshot.fresh(now)&&self.imu.online,"sent_monotonic_ns":now});
            for id in snapshots {
                self.queue(id, api::event("imu", data.clone()), true, true);
            }
        }
        if !statuses.is_empty() {
            let status = self.status(now);
            for id in statuses {
                self.queue(id, api::event("status", status.clone()), false, false);
            }
        }
        for client in self.clients.values_mut() {
            if !client.dead && client.out.bytes > 0 && client.out.flush(&mut client.socket).is_err()
            {
                client.dead = true;
            }
        }
        let dead: Vec<_> = self
            .clients
            .iter()
            .filter(|(_, c)| c.dead)
            .map(|(id, _)| *id)
            .collect();
        for id in dead {
            self.clients.remove(&id);
            self.pending.retain(|_, p| p.client != id);
        }
    }
    fn timeout(&self, now: u64) -> i32 {
        let mut deadline = now + SECOND;
        deadline = deadline.min(self.discovery_time);
        if self.device_available() {
            for (link, wanted) in [(&self.control, true), (&self.imu, self.wants_imu())] {
                if wanted {
                    deadline = deadline.min(if link.socket.is_none() {
                        link.retry
                    } else if !link.online {
                        link.connect_deadline
                    } else {
                        now + SECOND
                    });
                }
            }
        }
        for p in self.pending.values() {
            deadline = deadline.min(p.deadline);
        }
        if let Some(gesture_deadline) = self.quick_gestures.deadline() {
            deadline = deadline.min(gesture_deadline);
        }
        if self.imu.online && self.next_imu_read > now {
            deadline = deadline.min(self.next_imu_read);
        }
        for c in self.clients.values() {
            if let Some(s) = &c.subscription {
                if s.imu && !s.native {
                    deadline = deadline.min(c.next_imu);
                }
                if s.state {
                    deadline = deadline.min(c.next_state);
                }
            }
        }
        ((deadline.saturating_sub(now).saturating_add(999_999) / 1_000_000).min(1000)) as i32
    }
    pub fn run(&mut self) -> io::Result<()> {
        STOP.store(false, Ordering::Relaxed);
        unsafe {
            libc::signal(libc::SIGTERM, stop as *const () as libc::sighandler_t);
            libc::signal(libc::SIGINT, stop as *const () as libc::sighandler_t);
            libc::signal(libc::SIGPIPE, libc::SIG_IGN);
        }
        #[derive(Clone)]
        enum Tag {
            Listener,
            Watcher,
            NetworkWatcher,
            Input(PathBuf),
            Control,
            Imu,
            Client(u64),
        }
        while !STOP.load(Ordering::Relaxed)
            && self
                .config
                .run_for
                .is_none_or(|seconds| self.now() < seconds.saturating_mul(SECOND))
        {
            let now = self.now();
            self.timers(now);
            let mut fds = Vec::new();
            let mut tags = Vec::new();
            let mut add = |fd, events, tag| {
                fds.push(libc::pollfd {
                    fd,
                    events,
                    revents: 0,
                });
                tags.push(tag);
            };
            add(self.listener.as_raw_fd(), libc::POLLIN, Tag::Listener);
            if let Some(watcher) = &self.watcher {
                add(watcher.as_raw_fd(), libc::POLLIN, Tag::Watcher);
            }
            if let Some(watcher) = &self.network_watcher {
                add(watcher.as_raw_fd(), libc::POLLIN, Tag::NetworkWatcher);
            }
            for (path, input) in &self.inputs {
                add(
                    input.file.as_raw_fd(),
                    libc::POLLIN,
                    Tag::Input(path.clone()),
                );
            }
            for (control, link) in [(true, &self.control), (false, &self.imu)] {
                if !control && link.online && now < self.next_imu_read {
                    continue;
                }
                if let Some(fd) = link.fd() {
                    add(
                        fd,
                        libc::POLLIN
                            | if !link.online || link.out.bytes > 0 {
                                libc::POLLOUT
                            } else {
                                0
                            },
                        if control { Tag::Control } else { Tag::Imu },
                    );
                }
            }
            for (id, c) in &self.clients {
                add(
                    c.socket.as_raw_fd(),
                    libc::POLLIN | if c.out.bytes > 0 { libc::POLLOUT } else { 0 },
                    Tag::Client(*id),
                );
            }
            let rc = unsafe {
                libc::poll(
                    fds.as_mut_ptr(),
                    fds.len() as libc::nfds_t,
                    self.timeout(now),
                )
            };
            if rc < 0 {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e);
            }
            for (fd, tag) in fds.into_iter().zip(tags) {
                let events = fd.revents;
                if events == 0 {
                    continue;
                }
                let now = self.now();
                match tag {
                    Tag::Listener => {
                        for _ in 0..MAX_CLIENTS + 1 {
                            match self.listener.accept() {
                                Ok((socket, _)) => {
                                    if self.clients.len() >= MAX_CLIENTS {
                                        drop(socket);
                                        self.metrics.clients_evicted += 1;
                                        continue;
                                    }
                                    socket.set_nonblocking(true)?;
                                    self.next_client += 1;
                                    self.clients.insert(
                                        self.next_client,
                                        Client {
                                            socket,
                                            decoder: api::Decoder::default(),
                                            out: Outbox::default(),
                                            subscription: None,
                                            next_imu: 0,
                                            last_sequence: 0,
                                            next_state: 0,
                                            dead: false,
                                        },
                                    );
                                }
                                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                                Err(e) => return Err(e),
                            }
                        }
                    }
                    Tag::Watcher => {
                        if let Some(watcher) = &mut self.watcher {
                            let mut b = [0; 8192];
                            let _ = watcher.read(&mut b);
                        }
                        self.discovery_time = 0;
                    }
                    Tag::NetworkWatcher => {
                        if let Some(watcher) = &mut self.network_watcher {
                            let mut bytes = [0; 8192];
                            let _ = watcher.read(&mut bytes);
                        }
                        self.discovery_time = 0;
                    }
                    Tag::Input(path) => self.input_events(&path, events, now),
                    Tag::Control | Tag::Imu => {
                        let control = matches!(tag, Tag::Control);
                        let link = if control {
                            &mut self.control
                        } else {
                            &mut self.imu
                        };
                        if !link.online && events & libc::POLLOUT != 0 {
                            match device::connected(link.socket.as_ref().unwrap()) {
                                Ok(true) => {
                                    link.online = true;
                                    link.backoff = 250_000_000;
                                    link.last_error = None;
                                    self.connected(control, now);
                                }
                                Ok(false) => {}
                                Err(e) => {
                                    self.disconnect(control, now, &e.to_string());
                                    continue;
                                }
                            }
                        }
                        let link = if control {
                            &mut self.control
                        } else {
                            &mut self.imu
                        };
                        if link.online && events & libc::POLLOUT != 0 {
                            if let Err(e) = link.out.flush(link.socket.as_mut().unwrap()) {
                                self.disconnect(control, now, &e.to_string());
                                continue;
                            }
                        }
                        let link = if control {
                            &mut self.control
                        } else {
                            &mut self.imu
                        };
                        if link.online && events & (libc::POLLIN | libc::POLLHUP) != 0 {
                            if !control {
                                self.next_imu_read = now + self.config.imu_batch_ms * 1_000_000;
                            }
                            match link.read() {
                                Ok(frames) => {
                                    for frame in frames {
                                        if control {
                                            self.control_frame(frame, now);
                                        } else {
                                            self.imu_frame(frame, now);
                                        }
                                    }
                                }
                                Err(e) => {
                                    self.disconnect(control, now, &e);
                                    continue;
                                }
                            }
                        }
                        if events & (libc::POLLERR | libc::POLLNVAL) != 0 {
                            self.disconnect(control, now, "TCP hangup/error");
                        }
                    }
                    Tag::Client(id) => {
                        let mut requests = Vec::new();
                        if let Some(client) = self.clients.get_mut(&id) {
                            if events & libc::POLLIN != 0 {
                                let mut bytes = [0; protocol::READ_CHUNK];
                                match client.socket.read(&mut bytes) {
                                    Ok(0) => client.dead = true,
                                    Ok(size) => match client.decoder.feed(&bytes[..size]) {
                                        Ok(frames) => {
                                            if frames.len() > 64 {
                                                client.dead = true;
                                                self.metrics.api_errors += 1;
                                            } else {
                                                for (kind, body) in frames {
                                                    if kind != api::JSON_KIND {
                                                        client.dead = true;
                                                        self.metrics.api_errors += 1;
                                                        break;
                                                    }
                                                    match serde_json::from_slice::<Value>(&body) {
                                                        Ok(value) => requests.push(value),
                                                        Err(_) => {
                                                            client.dead = true;
                                                            self.metrics.api_errors += 1;
                                                            break;
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                        Err(_) => {
                                            client.dead = true;
                                            self.metrics.api_errors += 1;
                                        }
                                    },
                                    Err(e)
                                        if e.kind() == io::ErrorKind::WouldBlock
                                            || e.kind() == io::ErrorKind::Interrupted => {}
                                    Err(_) => client.dead = true,
                                }
                            }
                            if events & libc::POLLOUT != 0
                                && client.out.flush(&mut client.socket).is_err()
                            {
                                client.dead = true;
                            }
                            if events & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                                client.dead = true;
                            }
                        }
                        for request in requests {
                            self.request(id, request, now);
                        }
                    }
                }
            }
        }
        Ok(())
    }
    fn input_events(&mut self, path: &Path, events: i16, now: u64) {
        let mut actions = Vec::new();
        let mut remove = false;
        if let Some(input) = self.inputs.get_mut(path) {
            if events & libc::POLLIN != 0 {
                let mut bytes = [0; 24 * 64];
                match input.file.read(&mut bytes) {
                    Ok(0) => remove = true,
                    Ok(size) => match input.decoder.feed(&bytes[..size], now) {
                        Ok(parsed) => actions = parsed,
                        Err(_) => {
                            remove = true;
                            self.metrics.input_gaps += 1;
                        }
                    },
                    Err(e)
                        if e.kind() == io::ErrorKind::WouldBlock
                            || e.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => remove = true,
                }
            }
            if events & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                remove = true;
            }
        }
        // Handle actions in kernel event order: pre-gap buttons must not reseed post-gap holds.
        for action in actions {
            match action {
                keys::Input::Button(button) => self.button(button),
                keys::Input::SynDropped => {
                    self.metrics.input_gaps += 1;
                    self.holds.reset_source("evdev");
                    self.notify(
                        "input_gap",
                        json!({"source":"evdev","reason":"SYN_DROPPED"}),
                    );
                }
                keys::Input::SynReport => {
                    let pressed = self
                        .inputs
                        .get(path)
                        .and_then(|reader| pressed_keys(&reader.file));
                    self.notify("input_resync",json!({"source":"evdev","query_succeeded":pressed.is_some(),"pressed_codes":pressed,"duration_unknown":true}));
                }
                keys::Input::Other => {}
            }
        }
        if remove {
            self.inputs.remove(path);
            self.holds.reset_source("evdev");
            self.discovery_time = now + 250_000_000;
            self.notify("input_reset", json!({"reason":"input disconnected"}));
        }
    }
}
fn pressed_keys(file: &File) -> Option<Vec<u16>> {
    #[cfg(target_os = "linux")]
    {
        let mut bits = [0u8; 96];
        let request = ((2u64 << 30) | (96u64 << 16) | ((b'E' as u64) << 8) | 0x18) as libc::c_ulong;
        if unsafe { libc::ioctl(file.as_raw_fd(), request as _, bits.as_mut_ptr()) } < 0 {
            return None;
        }
        Some(
            (0..768)
                .filter(|code| bits[*code / 8] & (1 << (*code % 8)) != 0)
                .map(|code| code as u16)
                .collect(),
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = file;
        None
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        if fs::symlink_metadata(&self.config.socket)
            .is_ok_and(|m| m.file_type().is_socket() && m.ino() == self.socket_inode)
        {
            let _ = fs::remove_file(&self.config.socket);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn queued_native_frames_share_one_socket_write() {
        struct Counter {
            calls: usize,
            bytes: Vec<u8>,
        }
        impl Write for Counter {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.calls += 1;
                self.bytes.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn write_vectored(&mut self, slices: &[io::IoSlice<'_>]) -> io::Result<usize> {
                self.calls += 1;
                let mut count = 0;
                for slice in slices {
                    self.bytes.extend_from_slice(slice);
                    count += slice.len();
                }
                Ok(count)
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut out = Outbox::default();
        for n in 0..12u8 {
            out.push(vec![n; 166], false, false).unwrap();
        }
        let mut sink = Counter {
            calls: 0,
            bytes: Vec::new(),
        };
        out.flush(&mut sink).unwrap();
        assert_eq!(sink.calls, 1, "batched native packets must share writev");
        assert_eq!(sink.bytes.len(), 12 * 166);
        for (n, packet) in sink.bytes.as_chunks::<166>().0.iter().enumerate() {
            assert!(packet.iter().all(|b| *b == n as u8));
        }
        assert_eq!(out.bytes, 0);
    }

    #[test]
    fn replace_only_unsent_telemetry_and_bound_native_queue() {
        let mut box_ = Outbox::default();
        box_.push(vec![1; 8], true, true).unwrap();
        box_.push(vec![2; 4], false, false).unwrap();
        assert_eq!(box_.push(vec![3; 8], true, true).unwrap(), 1);
        assert_eq!(box_.bytes, 12);
        let mut output = Vec::new();
        box_.flush(&mut output).unwrap();
        assert_eq!(output, [vec![2; 4], vec![3; 8]].concat());
        assert!(box_.push(vec![0; MAX_QUEUE + 1], true, false).is_err());
    }

    #[test]
    fn partial_vectored_write_preserves_packet_boundaries() {
        struct Partial(Vec<u8>);
        impl Write for Partial {
            fn write(&mut self, b: &[u8]) -> io::Result<usize> {
                let n = b.len().min(71);
                self.0.extend_from_slice(&b[..n]);
                Ok(n)
            }
            fn write_vectored(&mut self, slices: &[io::IoSlice<'_>]) -> io::Result<usize> {
                let mut remaining = 71;
                for slice in slices {
                    let n = slice.len().min(remaining);
                    self.0.extend_from_slice(&slice[..n]);
                    remaining -= n;
                }
                Ok(71 - remaining)
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut out = Outbox::default();
        out.push(vec![1; 166], false, false).unwrap();
        out.push(vec![2; 166], false, false).unwrap();
        let mut sink = Partial(Vec::new());
        out.flush(&mut sink).unwrap();
        assert_eq!(sink.0, [vec![1; 166], vec![2; 166]].concat());
        assert_eq!(out.bytes, 0);
    }
}
