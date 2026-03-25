// SPDX-License-Identifier: MIT
// Author: Jakob Kastelic
// Copyright (c) 2026 Stanford Research Systems, Inc.

use param_serv::TCP_ADDR;
use std::io::{self, BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

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

// ---- State --------------------------------------------------

struct State {
    values: Vec<String>,
    versions: Vec<u64>,
    clock: u64,
    sse_event: Arc<String>,
}

impl State {
    fn new(params: &[Param]) -> Self {
        let mut s = State {
            values: params.iter()
                .map(|p| format!("{:.6}", p.default))
                .collect(),
            versions: vec![1; params.len()],
            clock: 1,
            sse_event: Arc::new(String::new()),
        };
        s.sse_event = build_sse_event(&s, params);
        s
    }
}

fn build_sse_event(s: &State, params: &[Param]) -> Arc<String> {
    let mut ev = format!("data: {{\"c\":{},\"p\":{{", s.clock);
    for (i, p) in params.iter().enumerate() {
        if i > 0 { ev.push(','); }
        ev.push('"');
        ev.push_str(&p.name);
        ev.push_str("\":");
        ev.push_str(&s.values[i]);
    }
    ev.push_str("}}\n\n");
    Arc::new(ev)
}

// ---- HTTP helpers -------------------------------------------

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
    let path   = words.next().unwrap_or("/").to_string();

    let mut content_length = 0usize;
    loop {
        match read_line(r)? {
            Some(line) if line.is_empty() => break,
            Some(line) => {
                if let Some((k, v)) = line.split_once(": ")
                    && k.eq_ignore_ascii_case("content-length") {
                    content_length = v.trim().parse().unwrap_or(0);
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
        status, text, body.len()
    );
    for (k, v) in extra {
        hdr.push_str(&format!("{}: {}\r\n", k, v));
    }
    hdr.push_str("\r\n");
    w.write_all(hdr.as_bytes())?;
    w.write_all(body)
}

// ---- SSE ----------------------------------------------------

fn serve_sse(
    w: &mut impl Write,
    state: &Arc<Mutex<State>>,
    notify: &Arc<Condvar>,
    min_interval: Duration,
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
    let mut last_sent = Instant::now().checked_sub(min_interval).unwrap_or(Instant::now());

    loop {
        let event = {
            let s = notify
                .wait_while(state.lock().unwrap(), |s| s.clock <= cursor)
                .unwrap();
            cursor = s.clock;
            Arc::clone(&s.sse_event)
        };
        let elapsed = last_sent.elapsed();
        if elapsed < min_interval {
            thread::sleep(min_interval - elapsed);
        }
        if w.write_all(event.as_bytes()).is_err() {
            return;
        }
        if w.flush().is_err() {
            return;
        }
        last_sent = Instant::now();
    }
}

// ---- TCP request handler ------------------------------------

fn handle(
    stream: TcpStream,
    params: Arc<Vec<Param>>,
    state: Arc<Mutex<State>>,
    notify: Arc<Condvar>,
    min_sse_interval: Duration,
) {
    let mut writer = stream.try_clone().expect("clone");
    let mut reader = BufReader::new(stream);

    loop {
        let req = match parse_request(&mut reader) {
            Ok(Some(r)) => r,
            _ => return,
        };

        if req.method == "GET" && req.path == "/events" {
            serve_sse(&mut writer, &state, &notify, min_sse_interval);
            return;
        }

        let result = match (req.method.as_str(), req.path.split('?').next().unwrap_or("")) {

            // List parameter names
            ("GET", "/params") => {
                let body: String = params.iter()
                    .map(|p| format!("{}\n", p.name))
                    .collect();
                respond(&mut writer, 200, "OK", &[], body.as_bytes())
            }

            // Get changed values since cursor
            ("GET", "/values") => {
                let cursor: u64 = req.path.split_once('?')
                    .and_then(|(_, q)| q.split('&')
                        .find_map(|p| p.strip_prefix("cursor="))
                        .and_then(|v| v.parse().ok()))
                    .unwrap_or(0);
                let (body, new_cursor) = {
                    let s = state.lock().unwrap();
                    let mut body = String::new();
                    for (i, p) in params.iter().enumerate() {
                        if s.versions[i] > cursor {
                            body.push_str(&p.name);
                            body.push('\t');
                            body.push_str(&s.values[i]);
                            body.push('\n');
                        }
                    }
                    (body, s.clock)
                };
                respond(
                    &mut writer, 200, "OK",
                    &[("X-Cursor", &new_cursor.to_string())],
                    body.as_bytes(),
                )
            }

            // Set parameters (text: "name\tvalue\n" per line)
            ("PUT", "/params") => {
                if let Ok(text) = std::str::from_utf8(&req.body) {
                    let mut s = state.lock().unwrap();
                    s.clock += 1;
                    for line in text.lines() {
                        if let Some((name, val)) = line.split_once('\t')
                            && let Some(i) = params.iter().position(|p| p.name == name)
                            && let Ok(v) = val.parse::<f64>() {
                            s.values[i] = format!("{:.6}", v);
                            s.versions[i] = s.clock;
                        }
                    }
                    s.sse_event = build_sse_event(&s, &params);
                }
                notify.notify_all();
                respond(&mut writer, 200, "OK", &[], &[])
            }

            ("OPTIONS", _) => respond(
                &mut writer, 204, "No Content",
                &[
                    ("Access-Control-Allow-Methods", "GET, PUT, OPTIONS"),
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

// ---- main ---------------------------------------------------

unsafe extern "C" {
    fn signal(signum: i32, handler: usize) -> usize;
}
const SIGPIPE: i32 = 13;
const SIG_IGN: usize = 1;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: param_serv <params.txt> [--max-sse-hz N]");

    let mut max_sse_hz: f64 = 30.0;
    while let Some(arg) = args.next() {
        if arg == "--max-sse-hz"
            && let Some(v) = args.next().and_then(|s| s.parse().ok()) {
            max_sse_hz = v;
        }
    }
    let min_sse_interval = Duration::from_secs_f64(1.0 / max_sse_hz);

    let params: Arc<Vec<Param>> = Arc::new(
        load_params(&path).unwrap_or_else(|e| {
            eprintln!("error loading {}: {}", path, e);
            std::process::exit(1);
        })
    );

    unsafe { signal(SIGPIPE, SIG_IGN); }

    let state = Arc::new(Mutex::new(State::new(&params)));
    let notify = Arc::new(Condvar::new());

    let listener = TcpListener::bind(TCP_ADDR).expect("TCP bind");
    eprintln!("param_serv listening on {}", TCP_ADDR);

    for stream in listener.incoming().flatten() {
        let p = Arc::clone(&params);
        let s = Arc::clone(&state);
        let n = Arc::clone(&notify);
        thread::spawn(move || handle(stream, p, s, n, min_sse_interval));
    }
}
