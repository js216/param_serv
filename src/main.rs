// SPDX-License-Identifier: MIT
// Author: Jakob Kastelic
// Copyright (c) 2026 Stanford Research Systems, Inc.

use param_serv::{
    OP_GET, OP_LIST, OP_SET, UDS_PATH, read_f64, read_u8, read_u16,
    read_u64,
};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::net::UnixListener;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

struct Param {
    name: String,
    default: f64,
}

fn load_params(path: &str) -> io::Result<Vec<Param>> {
    let file = std::fs::File::open(path)?;
    let mut params = Vec::new();
    for line in BufReader::new(file).lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.splitn(2, char::is_whitespace);
        let name = parts.next().unwrap().to_string();
        let default = parts
            .next()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0.0);
        params.push(Param { name, default });
    }
    Ok(params)
}

static PARAMS: OnceLock<Vec<Param>> = OnceLock::new();

fn params() -> &'static [Param] {
    PARAMS.get().expect("params not loaded")
}

fn find_param(name: &str) -> Option<usize> {
    params().iter().position(|p| p.name == name)
}

const TCP_ADDR: &str = "0.0.0.0:7777";

unsafe extern "C" {
    fn signal(signum: i32, handler: usize) -> usize;
}
const SIGPIPE: i32 = 13;
const SIG_IGN: usize = 1;

// ---- State --------------------------------------------------

struct State {
    live: Vec<f64>,
    versions: Vec<u64>,
    clock: u64,
    sse_event: Arc<String>,
}

impl State {
    fn new() -> Self {
        let p = params();
        let mut s = State {
            live: p.iter().map(|p| p.default).collect(),
            versions: vec![1; p.len()],
            clock: 1,
            sse_event: Arc::new(String::new()),
        };
        s.rebuild_sse();
        s
    }

    fn rebuild_sse(&mut self) {
        let mut ev = format!("data: {{\"c\":{},\"p\":{{", self.clock);
        for (i, p) in params().iter().enumerate() {
            if i > 0 {
                ev.push(',');
            }
            ev.push('"');
            ev.push_str(&p.name);
            ev.push_str("\":");
            ev.push_str(&format!("{:.6}", self.live[i]));
        }
        ev.push_str("}}\n\n");
        self.sse_event = Arc::new(ev);
    }
}

// ---- Shared protocol helpers ------------------------------------

fn apply_set(
    r: &mut impl Read,
    state: &Mutex<State>,
) -> io::Result<()> {
    let count = read_u16(r)? as usize;
    let mut updates = Vec::<(usize, f64)>::with_capacity(count);
    let mut nbuf = [0u8; 255];
    for _ in 0..count {
        let nlen = read_u8(r)? as usize;
        r.read_exact(&mut nbuf[..nlen])?;
        let val = read_f64(r)?;
        if let Ok(name) = std::str::from_utf8(&nbuf[..nlen]) {
            if let Some(i) = find_param(name) {
                updates.push((i, val));
            }
        }
    }
    let mut s = state.lock().unwrap();
    s.clock += 1;
    for &(i, v) in &updates {
        s.live[i] = v;
        s.versions[i] = s.clock;
    }
    s.rebuild_sse();
    Ok(())
}

fn handle_op_get(
    r: &mut impl Read,
    w: &mut impl Write,
    state: &Mutex<State>,
    resp: &mut Vec<u8>,
) -> io::Result<()> {
    let cursor = read_u64(r)?;
    resp.clear();
    resp.extend_from_slice(&[0u8; 10]);
    let mut count = 0u16;
    {
        let s = state.lock().unwrap();
        resp[..8].copy_from_slice(&s.clock.to_ne_bytes());
        for (i, p) in params().iter().enumerate() {
            if s.versions[i] > cursor {
                resp.push(p.name.len() as u8);
                resp.extend_from_slice(p.name.as_bytes());
                resp.extend_from_slice(&s.live[i].to_ne_bytes());
                count += 1;
            }
        }
    }
    resp[8..10].copy_from_slice(&count.to_ne_bytes());
    w.write_all(resp)
}

fn handle_op_list(
    w: &mut impl Write,
    resp: &mut Vec<u8>,
) -> io::Result<()> {
    resp.clear();
    resp.extend_from_slice(&(params().len() as u16).to_ne_bytes());
    for p in params() {
        resp.push(p.name.len() as u8);
        resp.extend_from_slice(p.name.as_bytes());
    }
    w.write_all(resp)
}

// ---- UDS binary server ------------------------------------------
//
// OP_SET  [u8=1][u16 count][(u8 nlen)(name)(f64)]...  ->  [u8 0]
// OP_GET  [u8=2][u64 cursor]  ->
//         [u64 cursor][u16 count][(u8 nlen)(name)(f64)]...
// OP_LIST [u8=3]  ->  [u16 count][(u8 nlen)(name)]...

fn uds_serve(
    r: &mut impl Read,
    w: &mut impl Write,
    state: &Mutex<State>,
) -> io::Result<()> {
    let mut resp = Vec::new();
    loop {
        match read_u8(r)? {
            OP_SET => {
                apply_set(r, state)?;
                w.write_all(&[0u8])?;
            }
            OP_GET => handle_op_get(r, w, state, &mut resp)?,
            OP_LIST => handle_op_list(w, &mut resp)?,
            _ => return Ok(()),
        }
    }
}

fn run_uds(state: Arc<Mutex<State>>) {
    let _ = std::fs::remove_file(UDS_PATH);
    let listener = UnixListener::bind(UDS_PATH).expect("UDS bind");
    eprintln!("UDS  {}", UDS_PATH);
    for stream in listener.incoming().flatten() {
        let s = Arc::clone(&state);
        thread::spawn(move || {
            let mut w = stream.try_clone().unwrap();
            let mut r = BufReader::new(stream);
            let _ = uds_serve(&mut r, &mut w, &s);
        });
    }
}

// ---- HTTP/SSE TCP server ----------------------------------------

struct Request {
    method: String,
    path: String,
    body: Vec<u8>,
}

fn parse_request(r: &mut impl BufRead) -> io::Result<Option<Request>> {
    let mut line = String::new();
    if r.read_line(&mut line)? == 0 || line.trim().is_empty() {
        return Ok(None);
    }
    let mut words = line.trim().splitn(3, ' ');
    let method = words.next().unwrap_or("").to_string();
    let path = words.next().unwrap_or("/").to_string();

    let mut content_length = 0usize;
    loop {
        line.clear();
        if r.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            break;
        }
        if let Some((k, v)) = trimmed.split_once(": ") {
            if k.eq_ignore_ascii_case("content-length") {
                content_length = v.trim().parse().unwrap_or(0);
            }
        }
    }
    let mut body = vec![0u8; content_length];
    r.read_exact(&mut body)?;
    Ok(Some(Request { method, path, body }))
}

fn respond(
    w: &mut impl Write,
    status: u16,
    text: &str,
    extra: &[(&str, &str)],
    body: &[u8],
) -> io::Result<()> {
    let mut hdr = format!(
        "HTTP/1.1 {} {}\r\n\
         Content-Length: {}\r\n\
         Access-Control-Allow-Origin: *\r\n\
         Access-Control-Expose-Headers: X-Cursor\r\n",
        status,
        text,
        body.len()
    );
    for (k, v) in extra {
        hdr.push_str(&format!("{}: {}\r\n", k, v));
    }
    hdr.push_str("\r\n");
    w.write_all(hdr.as_bytes())?;
    w.write_all(body)
}

fn serve_sse(w: &mut impl Write, state: &Mutex<State>) {
    let hdr = "HTTP/1.1 200 OK\r\n\
        Content-Type: text/event-stream\r\n\
        Cache-Control: no-cache\r\n\
        Access-Control-Allow-Origin: *\r\n\
        \r\n";
    if w.write_all(hdr.as_bytes()).is_err() {
        return;
    }

    let mut cursor: u64 = 0;
    loop {
        let ev = {
            let s = state.lock().unwrap();
            if s.clock > cursor {
                cursor = s.clock;
                Some(Arc::clone(&s.sse_event))
            } else {
                None
            }
        };

        if let Some(e) = ev {
            if w.write_all(e.as_bytes()).is_err() || w.flush().is_err() {
                return;
            }
        } else {
            thread::sleep(Duration::from_nanos(1_000_000_000 / 60));
        }
    }
}

fn handle_tcp_client(stream: TcpStream, state: Arc<Mutex<State>>) {
    let mut writer = stream.try_clone().expect("clone");
    let mut reader = BufReader::new(stream);

    loop {
        let req = match parse_request(&mut reader) {
            Ok(Some(r)) => r,
            _ => return,
        };

        if req.method == "GET" && req.path == "/events" {
            serve_sse(&mut writer, &state);
            return;
        }

        let result = match (req.method.as_str(), req.path.as_str()) {
            ("GET", "/params") => {
                let body: String = params()
                    .iter()
                    .map(|p| format!("{}\n", p.name))
                    .collect();
                respond(&mut writer, 200, "OK", &[], body.as_bytes())
            }

            ("PUT", "/params") => {
                let _ = apply_set(
                    &mut io::Cursor::new(&req.body),
                    &state,
                );
                respond(&mut writer, 200, "OK", &[], &[])
            }

            ("OPTIONS", _) => respond(
                &mut writer,
                204,
                "No Content",
                &[
                    (
                        "Access-Control-Allow-Methods",
                        "GET, PUT, OPTIONS",
                    ),
                    ("Access-Control-Allow-Headers", "Content-Type"),
                ],
                &[],
            ),

            _ => respond(&mut writer, 404, "Not Found", &[], &[]),
        };
        if result.is_err() {
            return;
        }
    }
}

fn run_tcp(state: Arc<Mutex<State>>) {
    let listener = TcpListener::bind(TCP_ADDR).expect("TCP bind");
    eprintln!("TCP  {}", TCP_ADDR);
    for stream in listener.incoming().flatten() {
        let s = Arc::clone(&state);
        thread::spawn(move || handle_tcp_client(stream, s));
    }
}

// ---- main -------------------------------------------------------

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: param_serv <params.txt>");
    let loaded = load_params(&path).unwrap_or_else(|e| {
        eprintln!("error loading {}: {}", path, e);
        std::process::exit(1);
    });
    PARAMS.set(loaded).ok();

    unsafe {
        signal(SIGPIPE, SIG_IGN);
    }

    let state = Arc::new(Mutex::new(State::new()));

    {
        let s = Arc::clone(&state);
        thread::spawn(move || run_uds(s));
    }

    run_tcp(state);
}
