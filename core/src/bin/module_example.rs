//! dlx-module-example: a tiny external resolver/hook module for Downloader.
//!
//! Reads one JSON request line on stdin and answers with at most one JSON line.
//! Resolve: the output URL is `--base` plus the last path segment of the input URL.
//! Copy it as a starting point; see docs/modules.md for the protocol.
//!
//! Options (all from module.json "command"):
//!   --base <url>        prefix for resolved URLs (required to resolve)
//!   --header <K:V>      add a response header (repeatable)
//!   --filename <name>   suggest a file name
//!   --mirror <url>      add a mirror (repeatable)
//!   --log <file>        append every request line to <file> (inside the module folder)
//!   --skip              answer {"ok":true,"skip":true}
//!   --fail <message>    answer {"ok":false,"error":message}
//!   test modes: --sleep, --flood, --garbage
use serde_json::{Map, Value, json};
use std::io::{BufRead, Write};

#[derive(Default)]
struct Options {
    base: Option<String>,
    headers: Vec<(String, String)>,
    filename: Option<String>,
    mirrors: Vec<String>,
    log: Option<String>,
    skip: bool,
    fail: Option<String>,
    mode: Option<String>,
}

fn options() -> Result<Options, String> {
    let mut o = Options::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or(format!("{arg} needs a value"));
        match arg.as_str() {
            "--base" => o.base = Some(value()?),
            "--header" => {
                let v = value()?;
                let (k, v) = v.split_once(':').ok_or("--header needs K:V")?;
                o.headers.push((k.trim().into(), v.trim().into()));
            }
            "--filename" => o.filename = Some(value()?),
            "--mirror" => o.mirrors.push(value()?),
            "--log" => o.log = Some(value()?),
            "--skip" => o.skip = true,
            "--fail" => o.fail = Some(value()?),
            "--sleep" | "--flood" | "--garbage" => o.mode = Some(arg),
            "--stdio" => {}
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok(o)
}

fn answer(o: &Options, request: &Value) -> Value {
    if let Some(error) = &o.fail {
        return json!({"ok": false, "error": error});
    }
    if o.skip {
        return json!({"ok": true, "skip": true});
    }
    let Some(base) = &o.base else {
        return json!({"ok": false, "error": "no --base configured"});
    };
    let url = request["url"].as_str().unwrap_or_default();
    let name = url
        .split(['?', '#'])
        .next()
        .unwrap_or_default()
        .rsplit('/')
        .next()
        .unwrap_or_default();
    if name.is_empty() {
        return json!({"ok": true, "skip": true});
    }
    let headers: Map<String, Value> = o
        .headers
        .iter()
        .map(|(k, v)| (k.clone(), Value::from(v.as_str())))
        .collect();
    let mut out = json!({"ok": true, "url": format!("{base}{name}"), "headers": headers, "mirrors": o.mirrors});
    if let Some(filename) = &o.filename {
        out["filename"] = filename.as_str().into();
    }
    out
}

fn main() {
    let o = match options() {
        Ok(o) => o,
        Err(e) => {
            println!("{}", json!({"ok": false, "error": e}));
            return;
        }
    };
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    if let Some(log) = &o.log
        && let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)
    {
        let _ = writeln!(file, "{}", line.trim_end());
    }
    let mut out = std::io::stdout().lock();
    match o.mode.as_deref() {
        Some("--sleep") => std::thread::sleep(std::time::Duration::from_secs(600)),
        Some("--flood") => {
            let _ = out.write_all(&vec![b'x'; 1 << 20]);
        }
        Some("--garbage") => {
            let _ = writeln!(out, "hello, not json");
        }
        _ => {
            let Ok(request) = serde_json::from_str::<Value>(&line) else {
                let _ = writeln!(
                    out,
                    "{}",
                    json!({"ok": false, "error": "request is not JSON"})
                );
                return;
            };
            match request["op"].as_str() {
                Some("resolve") => {
                    let _ = writeln!(out, "{}", answer(&o, &request));
                }
                // Events need no answer; a real hook would act on request["path"] here.
                Some("event") => {}
                _ => {
                    let _ = writeln!(out, "{}", json!({"ok": false, "error": "unknown op"}));
                }
            }
        }
    }
}
