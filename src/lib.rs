// SPDX-License-Identifier: MIT
// Author: Jakob Kastelic
// Copyright (c) 2026 Stanford Research Systems, Inc.

pub mod config;

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpStream;

pub const TCP_ADDR: &str = "127.0.0.1:7777";

/// Param metadata returned by `Connection::list()`.
pub struct ParamInfo {
    pub name: String,
    pub opts: Vec<String>,
    pub prec: Option<usize>,
    pub unit: Option<String>,
    pub unit_conv: Vec<(String, i32)>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub step: Option<f64>,
}

/// Parse a single `/params` response line into a `ParamInfo`.
/// Format: `name[\topts:A,B,C][\tprec:N][\tunit:U][\tunit_conv:Hz=0,kHz=3][\tmin:V][\tmax:V][\tstep:V]`
pub fn parse_param_line(line: &str) -> Option<ParamInfo> {
    let mut fields = line.split('\t');
    let name = fields.next()?.to_owned();
    if name.is_empty() { return None; }
    let mut info = ParamInfo {
        name, opts: Vec::new(), prec: None, unit: None,
        unit_conv: Vec::new(), min: None, max: None, step: None,
    };
    for field in fields {
        if let Some(v) = field.strip_prefix("opts:") {
            info.opts = v.split(',').map(str::to_owned).collect();
        } else if let Some(v) = field.strip_prefix("prec:") {
            info.prec = v.parse().ok();
        } else if let Some(v) = field.strip_prefix("unit:") {
            info.unit = Some(v.to_owned());
        } else if let Some(v) = field.strip_prefix("unit_conv:") {
            for pair in v.split(',') {
                if let Some((name, exp)) = pair.split_once('=') {
                    if let Ok(e) = exp.parse::<i32>() {
                        info.unit_conv.push((name.to_owned(), e));
                    }
                }
            }
        } else if let Some(v) = field.strip_prefix("min:") {
            info.min = v.parse().ok();
        } else if let Some(v) = field.strip_prefix("max:") {
            info.max = v.parse().ok();
        } else if let Some(v) = field.strip_prefix("step:") {
            info.step = v.parse().ok();
        }
    }
    Some(info)
}

pub struct Connection {
    r: BufReader<TcpStream>,
    w: TcpStream,
    cursor: u64,
}

impl Connection {
    pub fn new() -> io::Result<Self> {
        let s = TcpStream::connect(TCP_ADDR)?;
        Ok(Self { w: s.try_clone()?, r: BufReader::new(s), cursor: 0 })
    }

    /// Returns param metadata for all parameters in index order.
    pub fn list(&mut self) -> io::Result<Vec<ParamInfo>> {
        self.send("GET /params HTTP/1.1\r\n\r\n")?;
        let (body, _) = self.recv()?;
        Ok(body.lines().filter_map(parse_param_line).collect())
    }

    /// Returns parameters changed since the last call as (name, value) pairs.
    /// On the first call returns all parameters.
    pub fn get(&mut self) -> io::Result<Vec<(String, String)>> {
        self.send(&format!("GET /values?cursor={} HTTP/1.1\r\n\r\n", self.cursor))?;
        let (body, cursor) = self.recv()?;
        self.cursor = cursor;
        let mut results = Vec::new();
        for line in body.lines() {
            if let Some((name, val)) = line.split_once('\t') {
                results.push((name.to_owned(), val.to_owned()));
            }
        }
        Ok(results)
    }

    /// Sets one or more parameters by name.
    pub fn set(&mut self, updates: &[(&str, &str)]) -> io::Result<()> {
        let body: String = updates.iter()
            .map(|(n, v)| format!("{}\t{}\n", n, v))
            .collect();
        self.send(&format!(
            "PUT /params HTTP/1.1\r\nContent-Length: {}\r\n\r\n{}",
            body.len(), body
        ))?;
        self.recv()?;
        Ok(())
    }

    /// Change the display unit for a parameter.
    pub fn set_unit(&mut self, name: &str, unit: &str) -> io::Result<()> {
        let body = format!("{}\t{}\n", name, unit);
        self.send(&format!(
            "PUT /unit HTTP/1.1\r\nContent-Length: {}\r\n\r\n{}",
            body.len(), body
        ))?;
        self.recv()?;
        Ok(())
    }

    fn send(&mut self, req: &str) -> io::Result<()> {
        self.w.write_all(req.as_bytes())
    }

    fn recv(&mut self) -> io::Result<(String, u64)> {
        let mut line = String::new();
        self.r.read_line(&mut line)?; // status line
        let mut content_length = 0usize;
        let mut cursor = self.cursor;
        loop {
            line.clear();
            self.r.read_line(&mut line)?;
            let h = line.trim();
            if h.is_empty() { break; }
            if let Some((k, v)) = h.split_once(": ") {
                if k.eq_ignore_ascii_case("content-length") {
                    content_length = v.parse().unwrap_or(0);
                } else if k.eq_ignore_ascii_case("x-cursor") {
                    cursor = v.parse().unwrap_or(self.cursor);
                }
            }
        }
        let mut body = vec![0u8; content_length];
        self.r.read_exact(&mut body)?;
        Ok((String::from_utf8_lossy(&body).into_owned(), cursor))
    }
}
