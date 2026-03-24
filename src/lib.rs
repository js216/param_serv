// SPDX-License-Identifier: MIT
// Author: Jakob Kastelic
// Copyright (c) 2026 Stanford Research Systems, Inc.

use std::io::{self, BufReader, Read, Write};
use std::os::unix::net::UnixStream;

pub const UDS_PATH: &str = "/tmp/param_serv";

pub enum Op {
    Set  = 1,
    Get  = 2,
    List = 3,
}

pub struct Connection {
    r: BufReader<UnixStream>,
    w: UnixStream,
    cursor: u64,
}

impl Connection {
    pub fn new() -> io::Result<Self> {
        let s = UnixStream::connect(UDS_PATH)?;
        Ok(Self { w: s.try_clone()?, r: BufReader::new(s), cursor: 0 })
    }

    pub fn list(&mut self) -> io::Result<Vec<String>> {
        self.w.write_all(&[Op::List as u8])?;
        let mut b2 = [0u8; 2];
        self.r.read_exact(&mut b2)?;
        let count = u16::from_ne_bytes(b2) as usize;
        let mut names = Vec::with_capacity(count);
        let mut nbuf = [0u8; 255];
        for _ in 0..count {
            let mut b1 = [0u8; 1];
            self.r.read_exact(&mut b1)?;
            let nlen = b1[0] as usize;
            self.r.read_exact(&mut nbuf[..nlen])?;
            names.push(
                String::from_utf8_lossy(&nbuf[..nlen]).into_owned(),
            );
        }
        Ok(names)
    }

    pub fn set(&mut self, updates: &[(&str, f64)]) -> io::Result<()> {
        let mut buf = Vec::with_capacity(
            3 + updates
                .iter()
                .map(|(n, _)| 1 + n.len() + 8)
                .sum::<usize>(),
        );
        buf.push(Op::Set as u8);
        buf.extend_from_slice(
            &(updates.len() as u16).to_ne_bytes(),
        );
        for (name, val) in updates {
            buf.push(name.len() as u8);
            buf.extend_from_slice(name.as_bytes());
            buf.extend_from_slice(&val.to_ne_bytes());
        }
        self.w.write_all(&buf)?;
        let mut ack = [0u8; 1];
        self.r.read_exact(&mut ack)?;
        Ok(())
    }

    pub fn get(&mut self) -> io::Result<Vec<(u16, String)>> {
        let mut req = [0u8; 9];
        req[0] = Op::Get as u8;
        req[1..].copy_from_slice(&self.cursor.to_ne_bytes());
        self.w.write_all(&req)?;

        let mut b8 = [0u8; 8];
        self.r.read_exact(&mut b8)?;
        self.cursor = u64::from_ne_bytes(b8);
        let mut b2 = [0u8; 2];
        self.r.read_exact(&mut b2)?;
        let count = u16::from_ne_bytes(b2) as usize;
        let mut results = Vec::with_capacity(count);
        let mut nbuf = [0u8; 255];
        for _ in 0..count {
            self.r.read_exact(&mut b2)?;
            let index = u16::from_ne_bytes(b2);
            let mut b1 = [0u8; 1];
            self.r.read_exact(&mut b1)?;
            let vlen = b1[0] as usize;
            self.r.read_exact(&mut nbuf[..vlen])?;
            let val =
                String::from_utf8_lossy(&nbuf[..vlen]).into_owned();
            results.push((index, val));
        }
        Ok(results)
    }
}
