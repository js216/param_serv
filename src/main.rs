// SPDX-License-Identifier: MIT
// Author: Jakob Kastelic
// Copyright (c) 2026 Stanford Research Systems, Inc.

use param_serv::{Op, UDS_PATH};
use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread;

struct Param {
    name: String,
    default: f64,
}

fn load_params(path: &str) -> std::io::Result<Vec<Param>> {
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
    sse_event: Arc<String>, // pre-built, shared with all SSE clients
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
        s.sse_event = build_sse_event(&s);
        s
    }
}

// ---- UDS binary server ------------------------------------------
//
// OP_SET  [u8=1][u16 count][(u8 nlen)(name)(f64)]...  ->  [u8 0]
// OP_GET  [u8=2][u64 cursor]  ->
//         [u64 cursor][u16 count][(u8 nlen)(name)(f64)]...
// OP_LIST [u8=3]  ->  [u16 count][(u8 nlen)(name)]...

fn uds_serve(
    r: &mut BufReader<UnixStream>,
    w: &mut UnixStream,
    state: &Arc<Mutex<State>>,
    notify: &Arc<Condvar>,
    index: &Arc<HashMap<String, usize>>,
) -> io::Result<()> {
    let p = params();
    let resp_cap =
        10 + p.iter().map(|p| 1 + p.name.len() + 8).sum::<usize>();
    let mut resp = Vec::with_capacity(resp_cap);
    let mut updates = Vec::<(usize, f64)>::with_capacity(p.len());
    let mut nbuf = [0u8; 255];

    loop {
        let mut b1 = [0u8; 1];
        r.read_exact(&mut b1)?;
        match b1[0] {
            op if op == Op::Set as u8 => {
                let mut b2 = [0u8; 2];
                r.read_exact(&mut b2)?;
                let count = u16::from_ne_bytes(b2) as usize;
                updates.clear();
                for _ in 0..count {
                    r.read_exact(&mut b1)?;
                    let nlen = b1[0] as usize;
                    r.read_exact(&mut nbuf[..nlen])?;
                    let mut b8 = [0u8; 8];
                    r.read_exact(&mut b8)?;
                    let val = f64::from_ne_bytes(b8);
                    if let Ok(name) = std::str::from_utf8(&nbuf[..nlen])
                    {
                        if let Some(&i) = index.get(name) {
                            updates.push((i, val));
                        }
                    }
                }
                {
                    let mut s = state.lock().unwrap();
                    s.clock += 1;
                    for &(i, v) in &updates {
                        s.live[i] = v;
                        s.versions[i] = s.clock;
                    }
                    s.sse_event = build_sse_event(&s);
                }
                notify.notify_all();
                w.write_all(&[0u8])?;
            }

            op if op == Op::Get as u8 => {
                let mut b8 = [0u8; 8];
                r.read_exact(&mut b8)?;
                let cursor = u64::from_ne_bytes(b8);
                resp.clear();
                // [u64 new_cursor][u16 count] -- filled below
                resp.extend_from_slice(&[0u8; 10]);
                let mut count = 0u16;
                {
                    let s = state.lock().unwrap();
                    resp[..8].copy_from_slice(&s.clock.to_ne_bytes());
                    for (i, p) in params().iter().enumerate() {
                        if s.versions[i] > cursor {
                            resp.push(p.name.len() as u8);
                            resp.extend_from_slice(p.name.as_bytes());
                            resp.extend_from_slice(
                                &s.live[i].to_ne_bytes(),
                            );
                            count += 1;
                        }
                    }
                }
                resp[8..10].copy_from_slice(&count.to_ne_bytes());
                w.write_all(&resp)?;
            }

            op if op == Op::List as u8 => {
                resp.clear();
                resp.extend_from_slice(
                    &(params().len() as u16).to_ne_bytes(),
                );
                for p in params() {
                    resp.push(p.name.len() as u8);
                    resp.extend_from_slice(p.name.as_bytes());
                }
                w.write_all(&resp)?;
            }

            _ => return Ok(()),
        }
    }
}

fn handle_uds_client(
    stream: UnixStream,
    state: Arc<Mutex<State>>,
    notify: Arc<Condvar>,
    index: Arc<HashMap<String, usize>>,
) {
    let mut w = stream.try_clone().unwrap();
    let mut r = BufReader::new(stream);
    let _ = uds_serve(&mut r, &mut w, &state, &notify, &index);
}

fn run_uds(
    state: Arc<Mutex<State>>,
    notify: Arc<Condvar>,
    index: Arc<HashMap<String, usize>>,
) {
    let _ = std::fs::remove_file(UDS_PATH);
    let listener = UnixListener::bind(UDS_PATH).expect("UDS bind");
    eprintln!("UDS  {}", UDS_PATH);
    for stream in listener.incoming().flatten() {
        let s = Arc::clone(&state);
        let n = Arc::clone(&notify);
        let i = Arc::clone(&index);
        thread::spawn(move || handle_uds_client(stream, s, n, i));
    }
}

// ---- HTTP/SSE TCP server ----------------------------------------

fn build_sse_event(s: &State) -> Arc<String> {
    let mut ev = format!("data: {{\"c\":{},\"p\":{{", s.clock);
    for (i, p) in params().iter().enumerate() {
        if i > 0 {
            ev.push(',');
        }
        ev.push('"');
        ev.push_str(&p.name);
        ev.push_str("\":");
        ev.push_str(&format!("{:.6}", s.live[i]));
    }
    ev.push_str("}}\n\n");
    Arc::new(ev)
}

struct Request {
    method: String,
    path: String,
    body: Vec<u8>,
}

fn read_line(r: &mut impl BufRead) -> io::Result<Option<String>> {
    let mut line = String::new();
    if r.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    Ok(Some(line.trim_end_matches(['\r', '\n']).to_string()))
}

fn parse_request(r: &mut impl BufRead) -> io::Result<Option<Request>> {
    let request_line = match read_line(r)? {
        Some(l) if !l.is_empty() => l,
        _ => return Ok(None),
    };
    let mut words = request_line.splitn(3, ' ');
    let method = words.next().unwrap_or("").to_string();
    let path = words.next().unwrap_or("/").to_string();

    let mut content_length = 0usize;
    loop {
        match read_line(r)? {
            Some(line) if line.is_empty() => break,
            Some(line) => {
                if let Some((k, v)) = line.split_once(": ") {
                    if k.eq_ignore_ascii_case("content-length") {
                        content_length = v.trim().parse().unwrap_or(0);
                    }
                }
            }
            None => return Ok(None),
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

fn serve_sse(
    w: &mut impl Write,
    state: &Arc<Mutex<State>>,
    notify: &Arc<Condvar>,
) {
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
        let event = {
            let s = notify
                .wait_while(state.lock().unwrap(), |s| s.clock <= cursor)
                .unwrap();
            cursor = s.clock;
            Arc::clone(&s.sse_event)
        };
        if w.write_all(event.as_bytes()).is_err() {
            return;
        }
        if w.flush().is_err() {
            return;
        }
    }
}

fn handle_tcp_client(
    stream: TcpStream,
    state: Arc<Mutex<State>>,
    notify: Arc<Condvar>,
    index: Arc<HashMap<String, usize>>,
) {
    let mut writer = stream.try_clone().expect("clone");
    let mut reader = BufReader::new(stream);

    loop {
        let req = match parse_request(&mut reader) {
            Ok(Some(r)) => r,
            _ => return,
        };

        if req.method == "GET" && req.path == "/events" {
            serve_sse(&mut writer, &state, &notify);
            return;
        }

        let result = match (req.method.as_str(), req.path.as_str()) {
            // List param names -- used by browser and diagnostics
            ("GET", "/params") => {
                let body: String = params()
                    .iter()
                    .map(|p| format!("{}\n", p.name))
                    .collect();
                respond(&mut writer, 200, "OK", &[], body.as_bytes())
            }

            // Set params from browser -- same binary body as UDS
            // OP_SET (minus the opcode byte)
            ("PUT", "/params") => {
                let b = &req.body;
                if b.len() >= 2 {
                    let count =
                        u16::from_ne_bytes([b[0], b[1]]) as usize;
                    let mut s = state.lock().unwrap();
                    s.clock += 1;
                    let mut pos = 2;
                    for _ in 0..count {
                        if pos >= b.len() {
                            break;
                        }
                        let nlen = b[pos] as usize;
                        pos += 1;
                        if pos + nlen + 8 > b.len() {
                            break;
                        }
                        if let Ok(name) =
                            std::str::from_utf8(&b[pos..pos + nlen])
                        {
                            let val = f64::from_ne_bytes(
                                b[pos + nlen..pos + nlen + 8]
                                    .try_into()
                                    .unwrap(),
                            );
                            if let Some(&i) = index.get(name) {
                                s.live[i] = val;
                                s.versions[i] = s.clock;
                            }
                        }
                        pos += nlen + 8;
                    }
                    s.sse_event = build_sse_event(&s);
                }
                notify.notify_all();
                respond(&mut writer, 200, "OK", &[], &[])
            }

            // CORS preflight -- browser sends this before PUT/POST
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

fn run_tcp(
    state: Arc<Mutex<State>>,
    notify: Arc<Condvar>,
    index: Arc<HashMap<String, usize>>,
) {
    let listener = TcpListener::bind(TCP_ADDR).expect("TCP bind");
    eprintln!("TCP  {}", TCP_ADDR);
    for stream in listener.incoming().flatten() {
        let s = Arc::clone(&state);
        let n = Arc::clone(&notify);
        let i = Arc::clone(&index);
        thread::spawn(move || handle_tcp_client(stream, s, n, i));
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
    let notify = Arc::new(Condvar::new());
    let index: Arc<HashMap<String, usize>> = Arc::new(
        params()
            .iter()
            .enumerate()
            .map(|(i, p)| (p.name.clone(), i))
            .collect(),
    );

    {
        let s = Arc::clone(&state);
        let n = Arc::clone(&notify);
        let i = Arc::clone(&index);
        thread::spawn(move || run_uds(s, n, i));
    }

    run_tcp(state, notify, index);
}
