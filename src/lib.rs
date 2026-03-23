// SPDX-License-Identifier: MIT
// Author: Jakob Kastelic
// Copyright (c) 2026 Stanford Research Systems, Inc.

//! # param_serv
//!
//! Lightweight parameter server with two interfaces:
//!
//! ## UDS binary protocol (`/tmp/param_serv`)
//!
//! On-device firmware clients use this instead of HTTP because it is
//! lower overhead (no HTTP framing, no text parsing) and stays local
//! to the device via a Unix domain socket -- no network stack involved.
//!
//! Use the helpers in this crate rather than the wire format directly:
//!
//! ```no_run
//! use param_serv::{connect, param_list, param_set, param_get};
//!
//! let sock = connect(); // blocks until server is up
//! let mut w = sock.try_clone().unwrap();
//! let mut r = std::io::BufReader::new(sock);
//!
//! let names = param_list(&mut w, &mut r).unwrap();
//!
//! param_set(&mut w, &mut r, &[("gain", 1.5), ("offset", 0.0)])
//!     .unwrap();
//!
//! let mut cursor = 0u64;
//! param_get(&mut w, &mut r, &mut cursor, |name, val| {
//!     println!("{name} = {val}");
//! }).unwrap();
//! ```
//!
//! All integers are native-endian. Wire format for reference:
//!
//! ```text
//! OP_LIST (3)  req: [u8=3]
//!              res: [u16 count][(u8 nlen)(name)...]
//! OP_SET  (1)  req: [u8=1][u16 count][(u8 nlen)(name)(f64)...]
//!              res: [u8=0]  (ack)
//! OP_GET  (2)  req: [u8=2][u64 cursor]
//!              res: [u64 cursor][u16 count][(u8 nlen)(name)(f64)...]
//! ```
//!
//! `OP_GET` returns only params changed since `cursor`;
//! pass `0` to get all.
//!
//! ## HTTP interface (TCP port 7777)
//!
//! For browser clients. `GET /events` is SSE (Server-Sent Events) --
//! a long-lived HTTP connection where the server pushes updates;
//! not REST.
//!
//! ```text
//! GET /params  ->  newline-separated parameter names
//! PUT /params  body: [u16 count][(u8 nlen)(name)(f64)...]  ->  200 OK
//! GET /events  ->  SSE stream (see below)
//! ```
//!
//! SSE event format (pushed at up to 60 Hz on any change):
//! ```text
//! data: {"c":<clock>,"p":{"name":value,...}}
//! ```
//! `c` is a monotonic counter incremented on every `SET`.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

pub const UDS_PATH: &str = "/tmp/param_serv";

// ---- Wire constants and helpers ---------------------------------

pub const OP_SET: u8 = 1;
pub const OP_GET: u8 = 2;
pub const OP_LIST: u8 = 3;

pub fn read_u8(r: &mut impl Read) -> io::Result<u8> {
    let mut b = [0u8; 1];
    r.read_exact(&mut b)?;
    Ok(b[0])
}
pub fn read_u16(r: &mut impl Read) -> io::Result<u16> {
    let mut b = [0u8; 2];
    r.read_exact(&mut b)?;
    Ok(u16::from_ne_bytes(b))
}
pub fn read_u64(r: &mut impl Read) -> io::Result<u64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b)?;
    Ok(u64::from_ne_bytes(b))
}
pub fn read_f64(r: &mut impl Read) -> io::Result<f64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b)?;
    Ok(f64::from_ne_bytes(b))
}

// ---- Public API -------------------------------------------------

/// Connect to the param server, retrying until it is available.
pub fn connect() -> UnixStream {
    loop {
        match UnixStream::connect(UDS_PATH) {
            Ok(s) => return s,
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

/// OP_LIST -- return all parameter names known to the server.
pub fn param_list(
    w: &mut impl Write,
    r: &mut impl Read,
) -> io::Result<Vec<String>> {
    w.write_all(&[OP_LIST])?;
    let count = read_u16(r)? as usize;
    let mut names = Vec::with_capacity(count);
    let mut nbuf = [0u8; 255];
    for _ in 0..count {
        let nlen = read_u8(r)? as usize;
        r.read_exact(&mut nbuf[..nlen])?;
        names.push(String::from_utf8_lossy(&nbuf[..nlen]).into_owned());
    }
    Ok(names)
}

/// OP_SET -- write one or more `(name, value)` pairs; waits for ack.
pub fn param_set(
    w: &mut impl Write,
    r: &mut impl Read,
    updates: &[(&str, f64)],
) -> io::Result<()> {
    let mut buf = Vec::with_capacity(
        3 + updates.iter().map(|(n, _)| 1 + n.len() + 8).sum::<usize>(),
    );
    buf.push(OP_SET);
    buf.extend_from_slice(&(updates.len() as u16).to_ne_bytes());
    for (name, val) in updates {
        buf.push(name.len() as u8);
        buf.extend_from_slice(name.as_bytes());
        buf.extend_from_slice(&val.to_ne_bytes());
    }
    w.write_all(&buf)?;
    let mut ack = [0u8; 1];
    r.read_exact(&mut ack)?;
    Ok(())
}

/// OP_GET -- fetch params changed since `cursor`.
/// Advances `cursor` to the server's current clock.
pub fn param_get(
    w: &mut impl Write,
    r: &mut impl Read,
    cursor: &mut u64,
) -> io::Result<Vec<(String, f64)>> {
    let mut req = [0u8; 9];
    req[0] = OP_GET;
    req[1..].copy_from_slice(&cursor.to_ne_bytes());
    w.write_all(&req)?;

    *cursor = read_u64(r)?;
    let count = read_u16(r)? as usize;
    let mut results = Vec::with_capacity(count);
    let mut nbuf = [0u8; 255];
    for _ in 0..count {
        let nlen = read_u8(r)? as usize;
        r.read_exact(&mut nbuf[..nlen])?;
        let val = read_f64(r)?;
        let name = String::from_utf8_lossy(&nbuf[..nlen]).into_owned();
        results.push((name, val));
    }
    Ok(results)
}
