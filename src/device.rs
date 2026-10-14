// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kostiantyn Andriiuk <konstantin@andriyuk.com>
use serde::Serialize;
#[cfg(target_os = "linux")]
use std::fs;
use std::fs::{File, OpenOptions};
use std::io;
use std::net::{Ipv4Addr, SocketAddrV4, TcpStream};
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

#[derive(Default, Clone, Serialize)]
pub struct Discovery {
    pub usb_device: Option<String>,
    pub ambiguous: bool,
    pub source: Option<Ipv4Addr>,
    pub interface: Option<String>,
    pub input: Vec<PathBuf>,
    pub display_status: Option<String>,
}
#[cfg(target_os = "linux")]
fn text(path: impl AsRef<Path>) -> String {
    fs::read_to_string(path)
        .unwrap_or_default()
        .trim()
        .to_owned()
}
#[cfg(target_os = "linux")]
fn xreal_parent(path: &Path) -> bool {
    path.ancestors()
        .any(|p| text(p.join("idVendor")) == "3318" && text(p.join("idProduct")) == "043e")
}

pub fn discover() -> Discovery {
    let mut result = Discovery::default();
    #[cfg(target_os = "linux")]
    {
        let units: Vec<PathBuf> = fs::read_dir("/sys/bus/usb/devices")
            .ok()
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| {
                text(e.path().join("idVendor")) == "3318"
                    && text(e.path().join("idProduct")) == "043e"
            })
            .filter_map(|e| e.path().canonicalize().ok())
            .collect();
        if units.len() > 1 {
            result.ambiguous = true;
            return result;
        }
        let selected = units.first();
        result.usb_device = selected.map(|p| p.to_string_lossy().into_owned());
        if let Ok(entries) = fs::read_dir("/sys/class/input") {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if !name.starts_with("event") {
                    continue;
                }
                let device = entry.path().join("device");
                if text(device.join("id/bustype")) == "0003"
                    && text(device.join("id/vendor")) == "3318"
                    && text(device.join("id/product")) == "043e"
                    && device.canonicalize().is_ok_and(|p| {
                        xreal_parent(&p) && selected.is_some_and(|s| p.starts_with(s))
                    })
                {
                    result
                        .input
                        .push(PathBuf::from("/dev/input").join(name.as_ref()));
                }
            }
        }
        if let Ok(entries) = fs::read_dir("/sys/class/net") {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                let device = entry.path().join("device");
                let ncm = device
                    .join("driver")
                    .canonicalize()
                    .is_ok_and(|p| p.file_name().is_some_and(|n| n == "cdc_ncm"));
                if ncm
                    && device.canonicalize().is_ok_and(|p| {
                        xreal_parent(&p) && selected.is_some_and(|s| p.starts_with(s))
                    })
                {
                    if let Some(source) = interface_address(&name) {
                        result.source = Some(source);
                        result.interface = Some(name);
                        break;
                    }
                }
            }
        }
        result.display_status = fs::read_dir("/sys/class/drm")
            .ok()
            .and_then(|entries| {
                entries
                    .flatten()
                    .find(|e| e.file_name().to_string_lossy().ends_with("-DP-1"))
            })
            .map(|e| text(e.path().join("status")));
    }
    result.input.sort();
    result
}

#[cfg(target_os = "linux")]
fn interface_address(name: &str) -> Option<Ipv4Addr> {
    // getifaddrs owns this linked list until freeifaddrs; addresses are checked before dereference.
    unsafe {
        let mut base = std::ptr::null_mut();
        if libc::getifaddrs(&mut base) != 0 {
            return None;
        }
        let mut cursor = base;
        let mut found = None;
        while !cursor.is_null() {
            let item = &*cursor;
            if !item.ifa_name.is_null()
                && !item.ifa_addr.is_null()
                && (*item.ifa_addr).sa_family as i32 == libc::AF_INET
                && std::ffi::CStr::from_ptr(item.ifa_name).to_bytes() == name.as_bytes()
            {
                let addr = &*(item.ifa_addr as *const libc::sockaddr_in);
                found = Some(Ipv4Addr::from(addr.sin_addr.s_addr.to_ne_bytes()));
                break;
            }
            cursor = item.ifa_next;
        }
        libc::freeifaddrs(base);
        found
    }
}

pub fn connect(address: SocketAddrV4, source: Option<Ipv4Addr>) -> io::Result<(TcpStream, bool)> {
    // The only raw descriptor is immediately owned by TcpStream, including all error paths.
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let stream = unsafe { TcpStream::from_raw_fd(fd) };
    stream.set_nonblocking(true)?;
    stream.set_nodelay(true)?;
    unsafe {
        if libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    fn sock(ip: Ipv4Addr, port: u16) -> libc::sockaddr_in {
        let mut out: libc::sockaddr_in = unsafe { std::mem::zeroed() };
        out.sin_family = libc::AF_INET as _;
        out.sin_port = port.to_be();
        out.sin_addr.s_addr = u32::from_ne_bytes(ip.octets());
        #[cfg(target_os = "macos")]
        {
            out.sin_len = std::mem::size_of::<libc::sockaddr_in>() as _;
        }
        out
    }
    if let Some(source) = source {
        let addr = sock(source, 0);
        if unsafe {
            libc::bind(
                fd,
                &addr as *const _ as *const libc::sockaddr,
                std::mem::size_of_val(&addr) as _,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    let addr = sock(*address.ip(), address.port());
    let rc = unsafe {
        libc::connect(
            fd,
            &addr as *const _ as *const libc::sockaddr,
            std::mem::size_of_val(&addr) as _,
        )
    };
    if rc == 0 {
        Ok((stream, true))
    } else {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EINPROGRESS) {
            Ok((stream, false))
        } else {
            Err(error)
        }
    }
}
pub fn connected(stream: &TcpStream) -> io::Result<bool> {
    if let Some(error) = stream.take_error()? {
        Err(error)
    } else {
        Ok(stream.peer_addr().is_ok())
    }
}
pub fn input_open(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)?;
    #[cfg(target_os = "linux")]
    {
        #[repr(C)]
        #[derive(Default)]
        struct Identity {
            bus: u16,
            vendor: u16,
            product: u16,
            version: u16,
        }
        let mut identity = Identity::default();
        let request = (2u64 << 30) | (8u64 << 16) | ((b'E' as u64) << 8) | 2;
        // Recheck the descriptor, not just sysfs: eventN can be reused during unplug/open.
        if unsafe { libc::ioctl(file.as_raw_fd(), request as _, &mut identity) } < 0 {
            return Err(io::Error::last_os_error());
        }
        if (identity.bus, identity.vendor, identity.product) != (3, 0x3318, 0x043e) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "opened input is not XREAL USB3318:043e",
            ));
        }
        if std::mem::size_of::<libc::input_event>() != 24 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "input_event ABI must be Linux64",
            ));
        }
    }
    Ok(file)
}

pub fn watcher() -> Option<File> {
    #[cfg(target_os = "linux")]
    unsafe {
        let fd = libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC);
        if fd < 0 {
            return None;
        }
        let file = File::from_raw_fd(fd);
        for dir in [c"/dev/input", c"/sys/class/net"] {
            libc::inotify_add_watch(
                file.as_raw_fd(),
                dir.as_ptr(),
                libc::IN_CREATE
                    | libc::IN_DELETE
                    | libc::IN_ATTRIB
                    | libc::IN_MOVED_TO
                    | libc::IN_MOVED_FROM,
            );
        }
        Some(file)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Address assignment can occur after interface creation; listen rather than waiting for the fallback scan.
pub fn network_watcher() -> Option<File> {
    #[cfg(target_os = "linux")]
    unsafe {
        let fd = libc::socket(
            libc::AF_NETLINK,
            libc::SOCK_RAW | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            libc::NETLINK_ROUTE,
        );
        if fd < 0 {
            return None;
        }
        let file = File::from_raw_fd(fd);
        let mut address: libc::sockaddr_nl = std::mem::zeroed();
        address.nl_family = libc::AF_NETLINK as _;
        address.nl_groups = (libc::RTMGRP_LINK | libc::RTMGRP_IPV4_IFADDR) as _;
        if libc::bind(
            fd,
            &address as *const _ as *const libc::sockaddr,
            std::mem::size_of_val(&address) as _,
        ) < 0
        {
            return None;
        }
        Some(file)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}
