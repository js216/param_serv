// Config loader for param_serv.
//
// Public interface is intentionally minimal so the parser can be replaced
// with mlua (or any other Lua evaluator) by rewriting `load()` only.

use std::io;

pub struct ParamDef {
    pub name: String,
    pub default: f64,
    #[allow(dead_code)]
    pub min: Option<f64>,
    #[allow(dead_code)]
    pub max: Option<f64>,
}

pub struct Config {
    pub max_sse_hz: f64,
    pub params: Vec<ParamDef>,
}

impl Default for Config {
    fn default() -> Self {
        Config { max_sse_hz: 30.0, params: Vec::new() }
    }
}

/// Load config from a Lua-subset data file.
/// To swap in mlua: replace only the body of this function.
pub fn load(path: &str) -> io::Result<Config> {
    let src = std::fs::read_to_string(path)?;
    parse(&src).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

// ---- Tokenizer ----------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    Str(String),
    Num(f64),
    Eq,
    LBrace,
    RBrace,
    Comma,
}

fn tokenize(src: &str) -> Result<Vec<Tok>, String> {
    let mut toks = Vec::new();
    let mut it = src.chars().peekable();
    while let Some(&c) = it.peek() {
        match c {
            ' ' | '\t' | '\n' | '\r' => { it.next(); }

            '-' => {
                it.next();
                if it.peek() == Some(&'-') {
                    // comment: skip to end of line
                    for ch in it.by_ref() { if ch == '\n' { break; } }
                } else {
                    // negative number
                    let mut s = String::from('-');
                    while it.peek().is_some_and(|&c| c.is_ascii_digit() || c == '.') {
                        s.push(it.next().unwrap());
                    }
                    if s == "-" { return Err("bare '-'".into()); }
                    toks.push(Tok::Num(s.parse().map_err(|_| format!("bad number: {s}"))?));
                }
            }

            c if c.is_ascii_digit() => {
                let mut s = String::new();
                while it.peek().is_some_and(|&c| c.is_ascii_digit() || c == '.') {
                    s.push(it.next().unwrap());
                }
                toks.push(Tok::Num(s.parse().map_err(|_| format!("bad number: {s}"))?));
            }

            c if c.is_alphabetic() || c == '_' => {
                let mut s = String::new();
                while it.peek().is_some_and(|&c| c.is_alphanumeric() || c == '_') {
                    s.push(it.next().unwrap());
                }
                toks.push(Tok::Ident(s));
            }

            '"' => {
                it.next();
                let s: String = it.by_ref().take_while(|&c| c != '"').collect();
                toks.push(Tok::Str(s));
            }

            '=' => { it.next(); toks.push(Tok::Eq); }
            '{' => { it.next(); toks.push(Tok::LBrace); }
            '}' => { it.next(); toks.push(Tok::RBrace); }
            ',' => { it.next(); toks.push(Tok::Comma); }

            c => return Err(format!("unexpected char: {c:?}")),
        }
    }
    Ok(toks)
}

// ---- Parser -------------------------------------------------------------

struct P<'a> {
    t: &'a [Tok],
    i: usize,
}

impl<'a> P<'a> {
    fn peek(&self) -> Option<&'a Tok> { self.t.get(self.i) }

    fn next(&mut self) -> Option<&'a Tok> {
        let t = self.t.get(self.i);
        self.i += 1;
        t
    }

    fn eat_comma(&mut self) {
        if self.peek() == Some(&Tok::Comma) { self.next(); }
    }

    fn expect_eq(&mut self) -> Result<(), String> {
        match self.next() {
            Some(Tok::Eq) => Ok(()),
            t => Err(format!("expected '=', got {t:?}")),
        }
    }

    fn expect_lbrace(&mut self) -> Result<(), String> {
        match self.next() {
            Some(Tok::LBrace) => Ok(()),
            t => Err(format!("expected '{{', got {t:?}")),
        }
    }

    fn scalar(&mut self) -> Result<Scalar, String> {
        match self.next() {
            Some(Tok::Num(n))   => Ok(Scalar::Num(*n)),
            Some(Tok::Str(s))   => Ok(Scalar::Str(s.clone())),
            Some(Tok::Ident(s)) => Ok(Scalar::Str(s.clone())),
            t => Err(format!("expected scalar value, got {t:?}")),
        }
    }

    // Parse { key = scalar, ... }
    fn kv_table(&mut self) -> Result<Vec<(String, Scalar)>, String> {
        self.expect_lbrace()?;
        let mut pairs = Vec::new();
        loop {
            match self.peek() {
                Some(Tok::RBrace) => { self.next(); break; }
                Some(Tok::Ident(_)) => {
                    let key = match self.next() {
                        Some(Tok::Ident(k)) => k.clone(),
                        _ => unreachable!(),
                    };
                    self.expect_eq()?;
                    pairs.push((key, self.scalar()?));
                    self.eat_comma();
                }
                t => return Err(format!("expected key or '}}', got {t:?}")),
            }
        }
        Ok(pairs)
    }
}

enum Scalar {
    Num(f64),
    Str(String),
}

impl Scalar {
    fn num(self, key: &str) -> Result<f64, String> {
        match self { Scalar::Num(n) => Ok(n), _ => Err(format!("expected number for '{key}'")) }
    }
    fn str(self, key: &str) -> Result<String, String> {
        match self { Scalar::Str(s) => Ok(s), _ => Err(format!("expected string for '{key}'")) }
    }
}

// ---- Top-level parse ----------------------------------------------------

fn parse(src: &str) -> Result<Config, String> {
    let toks = tokenize(src)?;
    let mut p = P { t: &toks, i: 0 };
    let mut cfg = Config::default();

    while p.peek().is_some() {
        let key = match p.next() {
            Some(Tok::Ident(k)) => k.clone(),
            t => return Err(format!("expected top-level key, got {t:?}")),
        };
        p.expect_eq()?;

        match key.as_str() {
            "settings" => {
                for (k, v) in p.kv_table()? {
                    if k.as_str() == "max_sse_hz" {
                        cfg.max_sse_hz = v.num("max_sse_hz")?;
                    }
                }
            }
            "params" => {
                p.expect_lbrace()?;
                loop {
                    match p.peek() {
                        Some(Tok::RBrace) => { p.next(); break; }
                        Some(Tok::LBrace) => {
                            let mut name = String::new();
                            let mut default = 0.0f64;
                            let mut min = None;
                            let mut max = None;
                            for (k, v) in p.kv_table()? {
                                match k.as_str() {
                                    "name"    => name    = v.str("name")?,
                                    "default" => default = v.num("default")?,
                                    "min"     => min     = Some(v.num("min")?),
                                    "max"     => max     = Some(v.num("max")?),
                                    _ => {}
                                }
                            }
                            if name.is_empty() {
                                return Err("param entry missing 'name'".into());
                            }
                            cfg.params.push(ParamDef { name, default, min, max });
                            p.eat_comma();
                        }
                        t => return Err(format!("expected '{{' or '}}', got {t:?}")),
                    }
                }
            }
            k => return Err(format!("unknown top-level key: '{k}'")),
        }
    }

    Ok(cfg)
}
