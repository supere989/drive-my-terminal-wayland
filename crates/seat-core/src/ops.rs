//! High-level operations shared by the CLI and the MCP server.

use std::time::Duration;

use anyhow::{bail, Result};

use crate::eis::{Eis, BTN_LEFT, BTN_MIDDLE, BTN_RIGHT};
use crate::session;
use crate::shot;

const EIS_TIMEOUT: Duration = Duration::from_secs(10);

pub fn parse_button(name: &str) -> Result<u32> {
    Ok(match name.to_ascii_lowercase().as_str() {
        "left" | "l" => BTN_LEFT,
        "right" | "r" => BTN_RIGHT,
        "middle" | "m" => BTN_MIDDLE,
        other => bail!("unknown button '{other}' (left|right|middle)"),
    })
}

/// Connect to the session's EIS server, run `f`, disconnect.
pub fn with_eis<T>(name: &str, f: impl FnOnce(&mut Eis) -> Result<T>) -> Result<T> {
    let info = session::load(name)?;
    if !info.kwin.alive() {
        bail!("session '{name}' is not running");
    }
    let mut eis = Eis::connect(&info.bus_address, EIS_TIMEOUT)?;
    f(&mut eis)
}

pub fn check_bounds(eis: &Eis, x: f32, y: f32) -> Result<()> {
    let (bx, by, bw, bh) = eis.bounds;
    if x < bx as f32 || y < by as f32 || x >= (bx + bw) as f32 || y >= (by + bh) as f32 {
        bail!(
            "({x}, {y}) is outside the session screen {bw}x{bh} at ({bx}, {by})"
        );
    }
    Ok(())
}

pub fn screenshot_png(name: &str) -> Result<Vec<u8>> {
    let info = session::load(name)?;
    if !info.kwin.alive() {
        bail!("session '{name}' is not running");
    }
    let conn = shot::connect_bus(&info.bus_address)?;
    let img = shot::capture_workspace(&conn)?;
    shot::encode_png(&img)
}

pub fn move_to(name: &str, x: f32, y: f32) -> Result<()> {
    with_eis(name, |e| {
        check_bounds(e, x, y)?;
        e.move_to(x, y)
    })
}

pub fn click(name: &str, x: f32, y: f32, button: &str, count: u32) -> Result<()> {
    let b = parse_button(button)?;
    with_eis(name, |e| {
        check_bounds(e, x, y)?;
        e.click(x, y, b, count)
    })
}

pub fn scroll(name: &str, x: f32, y: f32, dx: i32, dy: i32) -> Result<()> {
    with_eis(name, |e| {
        check_bounds(e, x, y)?;
        e.scroll(x, y, dx, dy)
    })
}

pub fn key(name: &str, chord: &str) -> Result<()> {
    with_eis(name, |e| e.key(chord))
}

pub fn type_text(name: &str, text: &str) -> Result<()> {
    with_eis(name, |e| e.type_text(text))
}
