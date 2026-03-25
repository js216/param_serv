// SPDX-License-Identifier: MIT
// Author: Jakob Kastelic
// Copyright (c) 2026 Stanford Research Systems, Inc.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpStream;

pub const TCP_ADDR: &str = "127.0.0.1:7777";

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

    /// Returns all parameter names in index order.
    pub fn list(&mut self) -> io::Result<Vec<String>> {
        self.send("GET /params HTTP/1.1\r\n\r\n")?;
        let (body, _) = self.recv()?;
        Ok(body.lines().filter(|l| !l.is_empty()).map(str::to_owned).collect())
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
