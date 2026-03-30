use mlua::prelude::*;
use std::io;

pub struct ParamDef {
    pub name: String,
    pub default: String,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub opts: Vec<String>,
    pub prec: Option<usize>,
    pub unit: Option<String>,
    pub unit_conv: Vec<(String, i32)>,
    pub step: Option<f64>,
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

pub fn load(path: &str) -> io::Result<Config> {
    let src = std::fs::read_to_string(path)?;
    parse(&src).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
}

fn parse(src: &str) -> LuaResult<Config> {
    let lua = Lua::new();
    lua.load(src).exec()?;
    let mut cfg = Config::default();

    // settings
    if let Ok(settings) = lua.globals().get::<LuaTable>("settings") {
        if let Ok(hz) = settings.get::<f64>("max_sse_hz") {
            cfg.max_sse_hz = hz;
        }
    }

    // params
    let params: LuaTable = lua.globals().get("params")?;
    for entry in params.sequence_values::<LuaTable>() {
        let t = entry?;
        let name: String = t.get("name")?;
        let default: String = match t.get::<LuaValue>("default")? {
            LuaValue::Number(n) => format!("{}", n),
            LuaValue::Integer(n) => format!("{}", n),
            LuaValue::String(s) => s.to_str()?.to_owned(),
            _ => "0".to_owned(),
        };
        let min: Option<f64> = t.get("min").ok();
        let max: Option<f64> = t.get("max").ok();
        let prec: Option<usize> = t.get::<Option<u32>>("prec").ok().flatten().map(|n| n as usize);
        let unit: Option<String> = t.get("unit").ok();
        let step: Option<f64> = t.get("step").ok();

        let mut unit_conv = Vec::new();
        if let Ok(tbl) = t.get::<LuaTable>("unit_conv") {
            for pair in tbl.pairs::<String, i32>() {
                let (k, v) = pair?;
                unit_conv.push((k, v));
            }
            unit_conv.sort_by_key(|&(_, exp)| exp);
        }

        let mut opts = Vec::new();
        if let Ok(tbl) = t.get::<LuaTable>("opts") {
            for v in tbl.sequence_values::<String>() {
                opts.push(v?);
            }
        }

        cfg.params.push(ParamDef { name, default, min, max, opts, prec, unit, unit_conv, step });
    }

    Ok(cfg)
}
