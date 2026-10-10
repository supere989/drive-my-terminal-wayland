//! Minimal MCP (Model Context Protocol) server over stdio, newline-delimited JSON-RPC 2.0.
//! Exposes the same operations as the CLI as tools, so any MCP-capable agent can drive an
//! isolated agent seat.

use std::io::{BufRead, Write};

use anyhow::{anyhow, Result};
use base64::Engine;
use seat_core::{ops, session};
use serde_json::{json, Value};

fn tool_defs() -> Value {
    let name = json!({"type": "string", "description": "Seat/session name"});
    let xy = |d: &str| json!({"type": "number", "description": d});
    json!([
        {"name": "seat_start",
         "description": "Create an isolated agent seat: a private headless KWin session with its own cursor, keyboard and focus. Never touches the human's session.",
         "inputSchema": {"type": "object", "properties": {
            "name": name, "width": {"type": "integer"}, "height": {"type": "integer"},
            "scale": {"type": "number"}, "xwayland": {"type": "boolean"}}, "required": ["name"]}},
        {"name": "seat_stop", "description": "Stop a seat and every process inside it.",
         "inputSchema": {"type": "object", "properties": {"name": name}, "required": ["name"]}},
        {"name": "seat_list", "description": "List seats and whether they are alive.",
         "inputSchema": {"type": "object", "properties": {}}},
        {"name": "seat_launch",
         "description": "Launch a program inside the seat (Wayland, private D-Bus). Use an argv array.",
         "inputSchema": {"type": "object", "properties": {"name": name,
            "command": {"type": "array", "items": {"type": "string"}}}, "required": ["name", "command"]}},
        {"name": "seat_screenshot", "description": "Capture the seat's screen (PNG, includes the agent cursor).",
         "inputSchema": {"type": "object", "properties": {"name": name}, "required": ["name"]}},
        {"name": "seat_move", "description": "Move the seat's cursor to absolute coordinates.",
         "inputSchema": {"type": "object", "properties": {"name": name, "x": xy("x"), "y": xy("y")}, "required": ["name", "x", "y"]}},
        {"name": "seat_click", "description": "Click at coordinates in the seat.",
         "inputSchema": {"type": "object", "properties": {"name": name, "x": xy("x"), "y": xy("y"),
            "button": {"type": "string", "enum": ["left", "right", "middle"]},
            "count": {"type": "integer", "description": "2 = double click"}}, "required": ["name", "x", "y"]}},
        {"name": "seat_scroll", "description": "Scroll wheel notches at coordinates (positive dy = down).",
         "inputSchema": {"type": "object", "properties": {"name": name, "x": xy("x"), "y": xy("y"),
            "dx": {"type": "integer"}, "dy": {"type": "integer"}}, "required": ["name", "x", "y"]}},
        {"name": "seat_key", "description": "Press a key chord in the seat, e.g. 'ctrl+shift+t', 'Return', 'alt+F4'.",
         "inputSchema": {"type": "object", "properties": {"name": name, "chord": {"type": "string"}}, "required": ["name", "chord"]}},
        {"name": "seat_type", "description": "Type text into the focused window of the seat.",
         "inputSchema": {"type": "object", "properties": {"name": name, "text": {"type": "string"}}, "required": ["name", "text"]}}
    ])
}

fn s<'a>(a: &'a Value, k: &str) -> Result<&'a str> {
    a.get(k).and_then(Value::as_str).ok_or_else(|| anyhow!("missing string argument '{k}'"))
}
fn f(a: &Value, k: &str) -> Result<f32> {
    a.get(k).and_then(Value::as_f64).map(|v| v as f32).ok_or_else(|| anyhow!("missing number argument '{k}'"))
}
fn i(a: &Value, k: &str, d: i64) -> i64 {
    a.get(k).and_then(Value::as_i64).unwrap_or(d)
}

fn text(t: impl Into<String>) -> Value {
    json!([{"type": "text", "text": t.into()}])
}

fn call_tool(tool: &str, a: &Value) -> Result<Value> {
    match tool {
        "seat_start" => {
            let mut spec = session::SessionSpec::new(s(a, "name")?);
            spec.width = i(a, "width", 1280) as u32;
            spec.height = i(a, "height", 800) as u32;
            spec.scale = a.get("scale").and_then(Value::as_f64).unwrap_or(1.0);
            spec.xwayland = a.get("xwayland").and_then(Value::as_bool).unwrap_or(false);
            let info = session::start(&spec)?;
            Ok(text(format!(
                "seat '{}' started: {}x{}, wayland socket {}, kwin pid {}",
                info.name, info.width, info.height, info.socket, info.kwin.pid
            )))
        }
        "seat_stop" => {
            session::stop(s(a, "name")?)?;
            Ok(text("stopped"))
        }
        "seat_list" => {
            let rows: Vec<Value> = session::list()?
                .iter()
                .map(|x| json!({"name": x.name, "alive": x.kwin.alive(), "size": format!("{}x{}", x.width, x.height),
                    "apps": x.apps.iter().filter(|p| p.alive()).map(|p| json!({"pid": p.pid, "cmd": p.cmd})).collect::<Vec<_>>()}))
                .collect();
            Ok(text(serde_json::to_string_pretty(&rows)?))
        }
        "seat_launch" => {
            let argv: Vec<String> = a
                .get("command")
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow!("missing array argument 'command'"))?
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
            let p = session::launch(s(a, "name")?, &argv, &[])?;
            Ok(text(format!("launched pid {}", p.pid)))
        }
        "seat_screenshot" => {
            let png = ops::screenshot_png(s(a, "name")?)?;
            Ok(json!([{"type": "image", "mimeType": "image/png",
                "data": base64::engine::general_purpose::STANDARD.encode(png)}]))
        }
        "seat_move" => {
            ops::move_to(s(a, "name")?, f(a, "x")?, f(a, "y")?)?;
            Ok(text("ok"))
        }
        "seat_click" => {
            ops::click(s(a, "name")?, f(a, "x")?, f(a, "y")?,
                a.get("button").and_then(Value::as_str).unwrap_or("left"), i(a, "count", 1) as u32)?;
            Ok(text("ok"))
        }
        "seat_scroll" => {
            ops::scroll(s(a, "name")?, f(a, "x")?, f(a, "y")?, i(a, "dx", 0) as i32, i(a, "dy", 0) as i32)?;
            Ok(text("ok"))
        }
        "seat_key" => {
            ops::key(s(a, "name")?, s(a, "chord")?)?;
            Ok(text("ok"))
        }
        "seat_type" => {
            ops::type_text(s(a, "name")?, s(a, "text")?)?;
            Ok(text("ok"))
        }
        other => Err(anyhow!("unknown tool '{other}'")),
    }
}

fn handle(msg: &Value) -> Option<Value> {
    let method = msg.get("method").and_then(Value::as_str)?;
    let id = msg.get("id").cloned();
    // Notifications carry no id and get no response.
    let id = match id {
        Some(id) => id,
        None => return None,
    };
    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": msg["params"]["protocolVersion"].as_str().unwrap_or("2024-11-05"),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "input-director", "version": env!("CARGO_PKG_VERSION")}
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools": tool_defs()})),
        "tools/call" => {
            let p = &msg["params"];
            let tool = p.get("name").and_then(Value::as_str).unwrap_or("");
            let args = p.get("arguments").cloned().unwrap_or(json!({}));
            Ok(match call_tool(tool, &args) {
                Ok(content) => json!({"content": content, "isError": false}),
                Err(e) => json!({"content": text(format!("{e:#}")), "isError": true}),
            })
        }
        other => Err(json!({"code": -32601, "message": format!("method not found: {other}")})),
    };
    Some(match result {
        Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
        Err(e) => json!({"jsonrpc": "2.0", "id": id, "error": e}),
    })
}

pub fn serve() -> Result<()> {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => handle(&msg),
            Err(e) => Some(json!({"jsonrpc": "2.0", "id": null,
                "error": {"code": -32700, "message": format!("parse error: {e}")}})),
        };
        if let Some(r) = reply {
            writeln!(out, "{r}")?;
            out.flush()?;
        }
    }
    Ok(())
}
