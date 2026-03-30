// SPDX-License-Identifier: MIT
// Author: Jakob Kastelic
// Copyright (c) 2026 Stanford Research Systems, Inc.

pub mod config;

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

// ---- Native Connection (TCP) ------------------------------------------------

#[cfg(not(target_os = "emscripten"))]
mod native {
    use super::*;
    use std::io::{self, BufRead, BufReader, Read, Write};
    use std::net::TcpStream;

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

        /// Reset cursor so the next `get()` returns all params.
        pub fn reset_cursor(&mut self) {
            self.cursor = 0;
        }

        pub fn list(&mut self) -> io::Result<Vec<ParamInfo>> {
            self.send("GET /params HTTP/1.1\r\n\r\n")?;
            let (body, _) = self.recv()?;
            Ok(body.lines().filter_map(parse_param_line).collect())
        }

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
            self.r.read_line(&mut line)?;
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
}

#[cfg(not(target_os = "emscripten"))]
pub use native::Connection;

// ---- Emscripten Connection (XHR via JS bridge) -----------------------------

#[cfg(target_os = "emscripten")]
mod web {
    use super::*;
    use std::io;

    unsafe extern "C" {
        fn gui_http_request(
            method: *const u8, url: *const u8,
            body: *const u8, body_len: usize,
            out: *mut u8, out_len: usize,
        ) -> usize;
        fn gui_http_get_header(name: *const u8, out: *mut u8, out_len: usize) -> usize;
        fn gui_get_server_url(out: *mut u8, out_len: usize) -> usize;
    }

    fn server_url() -> String {
        let mut buf = [0u8; 256];
        let n = unsafe { gui_get_server_url(buf.as_mut_ptr(), buf.len()) };
        String::from_utf8_lossy(&buf[..n]).into_owned()
    }

    fn http(method: &str, url: &str, body: &str) -> io::Result<String> {
        let m = format!("{}\0", method);
        let u = format!("{}\0", url);
        let mut out = vec![0u8; 65536];
        let n = unsafe {
            gui_http_request(
                m.as_ptr(), u.as_ptr(),
                body.as_ptr(), body.len(),
                out.as_mut_ptr(), out.len(),
            )
        };
        Ok(String::from_utf8_lossy(&out[..n]).into_owned())
    }

    fn get_header(name: &str) -> String {
        let n_cstr = format!("{}\0", name);
        let mut buf = [0u8; 256];
        let n = unsafe { gui_http_get_header(n_cstr.as_ptr(), buf.as_mut_ptr(), buf.len()) };
        String::from_utf8_lossy(&buf[..n]).into_owned()
    }

    pub struct Connection {
        base_url: String,
        cursor: u64,
    }

    impl Connection {
        pub fn new() -> io::Result<Self> {
            Ok(Self { base_url: server_url(), cursor: 0 })
        }

        pub fn reset_cursor(&mut self) {
            self.cursor = 0;
        }

        pub fn list(&mut self) -> io::Result<Vec<ParamInfo>> {
            let body = http("GET", &format!("{}/params", self.base_url), "")?;
            Ok(body.lines().filter_map(parse_param_line).collect())
        }

        pub fn get(&mut self) -> io::Result<Vec<(String, String)>> {
            let body = http("GET", &format!("{}/values?cursor={}", self.base_url, self.cursor), "")?;
            let cursor_str = get_header("X-Cursor");
            if let Ok(c) = cursor_str.parse::<u64>() {
                self.cursor = c;
            }
            let mut results = Vec::new();
            for line in body.lines() {
                if let Some((name, val)) = line.split_once('\t') {
                    results.push((name.to_owned(), val.to_owned()));
                }
            }
            Ok(results)
        }

        pub fn set(&mut self, updates: &[(&str, &str)]) -> io::Result<()> {
            let body: String = updates.iter()
                .map(|(n, v)| format!("{}\t{}\n", n, v))
                .collect();
            http("PUT", &format!("{}/params", self.base_url), &body)?;
            Ok(())
        }

        pub fn set_unit(&mut self, name: &str, unit: &str) -> io::Result<()> {
            let body = format!("{}\t{}\n", name, unit);
            http("PUT", &format!("{}/unit", self.base_url), &body)?;
            Ok(())
        }
    }
}

#[cfg(target_os = "emscripten")]
pub use web::Connection;
