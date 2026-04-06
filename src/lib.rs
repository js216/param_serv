// SPDX-License-Identifier: MIT
// Author: Jakob Kastelic
// Copyright (c) 2026 Stanford Research Systems, Inc.

pub mod config;

pub const TCP_ADDR_DEFAULT: &str = "127.0.0.1:7777";

pub fn tcp_addr() -> String {
    match std::env::var("PARAM_SERV_PORT") {
        Ok(port) => format!("127.0.0.1:{}", port),
        Err(_) => TCP_ADDR_DEFAULT.to_string(),
    }
}

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
                if let Some((name, exp)) = pair.split_once('=')
                    && let Ok(e) = exp.parse::<i32>()
                {
                    info.unit_conv.push((name.to_owned(), e));
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

// ---- Native Connection (SSE push + HTTP request) ----------------------------

#[cfg(not(target_os = "emscripten"))]
mod native {
    use super::*;
    use std::collections::HashMap;
    use std::io::{self, BufRead, BufReader, Read, Write};
    use std::net::TcpStream;
    use std::sync::{Arc, Mutex};
    use std::thread;

    /// Parse SSE event data: `data: {"c":N,"p":{"name":"val",...}}\n\n`
    /// Extracts name/value pairs from the "p" object.
    /// Parse SSE event and push only changed key-value pairs into `out`.
    fn parse_sse_dedup(line: &str, last: &mut HashMap<String, String>, out: &mut Vec<(String, String)>) {
        let p_start = match line.find("\"p\":{") {
            Some(i) => i + 4,
            None => return,
        };
        let inner = &line[p_start + 1..];
        let p_end = match inner.rfind('}') {
            Some(i) => i,
            None => return,
        };
        let pairs = &inner[..p_end];
        let mut rest = pairs;
        while let Some(kstart) = rest.find('"') {
            rest = &rest[kstart + 1..];
            let kend = match rest.find('"') { Some(i) => i, None => break };
            let key = &rest[..kend];
            rest = &rest[kend + 1..];
            let vstart = match rest.find('"') { Some(i) => i + 1, None => break };
            rest = &rest[vstart..];
            let mut vend = 0;
            let bytes = rest.as_bytes();
            while vend < bytes.len() {
                if bytes[vend] == b'"' && (vend == 0 || bytes[vend - 1] != b'\\') {
                    break;
                }
                vend += 1;
            }
            let raw_val = &rest[..vend];
            // Check dedup before allocating
            let changed = match last.get(key) {
                Some(prev) => prev != raw_val,
                None => true,
            };
            if changed {
                let val = if raw_val.contains('\\') {
                    raw_val.replace("\\\"", "\"").replace("\\\\", "\\")
                } else {
                    raw_val.to_owned()
                };
                last.insert(key.to_owned(), raw_val.to_owned());
                out.push((key.to_owned(), val));
            }
            rest = &rest[vend + 1..];
        }
    }

    pub struct Connection {
        buffer: Arc<Mutex<Vec<(String, String)>>>,
        has_data: Arc<std::sync::atomic::AtomicBool>,
        req_w: TcpStream,
        req_r: BufReader<TcpStream>,
    }

    impl Connection {
        pub fn new() -> io::Result<Self> {
            // Request connection (for set, list, set_unit, refresh)
            let addr = tcp_addr();
            let req = TcpStream::connect(&addr)?;
            let req_w = req.try_clone()?;
            let req_r = BufReader::new(req);

            // SSE connection (background thread reads pushed events)
            let mut sse = TcpStream::connect(&addr)?;
            sse.write_all(b"GET /events HTTP/1.1\r\n\r\n")?;

            let buffer: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
            let buf_clone = Arc::clone(&buffer);
            let has_data = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let has_data_clone = Arc::clone(&has_data);

            thread::spawn(move || {
                let mut reader = BufReader::new(sse);
                // Skip HTTP response headers
                let mut line = String::new();
                loop {
                    line.clear();
                    if reader.read_line(&mut line).is_err() { return; }
                    if line.trim().is_empty() { break; }
                }
                // Dedup: skip values that haven't changed
                let mut last: HashMap<String, String> = HashMap::new();
                let mut changed: Vec<(String, String)> = Vec::new();
                // Read SSE events
                loop {
                    line.clear();
                    match reader.read_line(&mut line) {
                        Ok(0) | Err(_) => return,
                        _ => {}
                    }
                    if let Some(data) = line.trim().strip_prefix("data: ") {
                        changed.clear();
                        parse_sse_dedup(data, &mut last, &mut changed);
                        if !changed.is_empty() {
                            buf_clone.lock().unwrap().extend(changed.drain(..));
                            has_data_clone.store(true, std::sync::atomic::Ordering::Release);
                        }
                    }
                }
            });

            Ok(Connection { buffer, has_data, req_w, req_r })
        }

        /// Non-blocking: drain all buffered SSE changes since last call.
        /// Uses an atomic flag to avoid the mutex lock when the buffer is empty.
        pub fn get(&mut self) -> io::Result<Vec<(String, String)>> {
            if !self.has_data.load(std::sync::atomic::Ordering::Acquire) {
                return Ok(Vec::new());
            }
            self.has_data.store(false, std::sync::atomic::Ordering::Relaxed);
            let mut buf = self.buffer.lock().unwrap();
            Ok(std::mem::take(&mut *buf))
        }

        /// Fetch all current values and merge into the SSE buffer.
        pub fn refresh(&mut self) -> io::Result<()> {
            self.send("GET /values?cursor=0 HTTP/1.1\r\n\r\n")?;
            let (body, _) = self.recv()?;
            let mut buf = self.buffer.lock().unwrap();
            for line in body.lines() {
                if let Some((name, val)) = line.split_once('\t') {
                    buf.push((name.to_owned(), val.to_owned()));
                }
            }
            Ok(())
        }

        pub fn list(&mut self) -> io::Result<Vec<ParamInfo>> {
            self.send("GET /params HTTP/1.1\r\n\r\n")?;
            let (body, _) = self.recv()?;
            Ok(body.lines().filter_map(parse_param_line).collect())
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
            self.req_w.write_all(req.as_bytes())
        }

        fn recv(&mut self) -> io::Result<(String, u64)> {
            let mut line = String::new();
            self.req_r.read_line(&mut line)?;
            let mut content_length = 0usize;
            let mut cursor = 0u64;
            loop {
                line.clear();
                self.req_r.read_line(&mut line)?;
                let h = line.trim();
                if h.is_empty() { break; }
                if let Some((k, v)) = h.split_once(": ") {
                    if k.eq_ignore_ascii_case("content-length") {
                        content_length = v.parse().unwrap_or(0);
                    } else if k.eq_ignore_ascii_case("x-cursor") {
                        cursor = v.parse().unwrap_or(0);
                    }
                }
            }
            let mut body = vec![0u8; content_length];
            self.req_r.read_exact(&mut body)?;
            Ok((String::from_utf8_lossy(&body).into_owned(), cursor))
        }
    }
}

#[cfg(not(target_os = "emscripten"))]
pub use native::Connection;

#[cfg(not(target_os = "emscripten"))]
pub fn poll_key() -> Option<String> { None }

#[cfg(not(target_os = "emscripten"))]
pub fn set_led(_name: &str, _on: bool) {}


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
        fn gui_poll_key(out: *mut u8, out_len: usize) -> usize;
        fn gui_get_server_url(out: *mut u8, out_len: usize) -> usize;
        fn gui_set_led(name: *const u8, value: u32);
        fn gui_sse_start(url: *const u8);
        fn gui_sse_drain(out: *mut u8, out_len: usize) -> usize;
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

    pub struct Connection {
        base_url: String,
    }

    impl Connection {
        pub fn new() -> io::Result<Self> {
            let base_url = server_url();
            // Start SSE EventSource in JS
            let sse_url = format!("{}/events\0", base_url);
            unsafe { gui_sse_start(sse_url.as_ptr()); }
            Ok(Self { base_url })
        }

        /// Non-blocking: drain SSE events buffered by JS EventSource.
        pub fn get(&mut self) -> io::Result<Vec<(String, String)>> {
            let mut out = vec![0u8; 65536];
            let n = unsafe { gui_sse_drain(out.as_mut_ptr(), out.len()) };
            if n == 0 { return Ok(Vec::new()); }
            let text = String::from_utf8_lossy(&out[..n]);
            let mut results = Vec::new();
            for line in text.lines() {
                if let Some((name, val)) = line.split_once('\t') {
                    results.push((name.to_owned(), val.to_owned()));
                }
            }
            Ok(results)
        }

        /// Fetch all current values via one-shot HTTP request.
        pub fn refresh(&mut self) -> io::Result<()> {
            // SSE will deliver all values on connect; nothing extra needed.
            // But if we need a forced refresh, do a GET /values?cursor=0.
            // The values will appear in the next get() drain.
            Ok(())
        }

        pub fn list(&mut self) -> io::Result<Vec<ParamInfo>> {
            let body = http("GET", &format!("{}/params", self.base_url), "")?;
            Ok(body.lines().filter_map(parse_param_line).collect())
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

    pub fn poll_key() -> Option<String> {
        let mut buf = [0u8; 64];
        let n = unsafe { gui_poll_key(buf.as_mut_ptr(), buf.len()) };
        if n == 0 { return None; }
        Some(String::from_utf8_lossy(&buf[..n]).into_owned())
    }

    pub fn set_led(name: &str, on: bool) {
        let cname = std::ffi::CString::new(name).unwrap();
        unsafe { gui_set_led(cname.as_ptr() as *const u8, on as u32); }
    }
}

#[cfg(target_os = "emscripten")]
pub use web::Connection;

#[cfg(target_os = "emscripten")]
pub use web::poll_key;

#[cfg(target_os = "emscripten")]
pub use web::set_led;
