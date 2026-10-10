//! input-director: give an AI agent its own cursor, keyboard and focus.

mod mcp;

use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use seat_core::{ops, session};

#[derive(Parser)]
#[command(name = "input-director", version, about)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Manage isolated seat sessions (private headless KWin).
    #[command(subcommand)]
    Session(SessionCmd),
    /// Launch a program inside a seat.
    Launch {
        name: String,
        #[arg(trailing_var_arg = true, required = true)]
        command: Vec<String>,
    },
    /// Save a PNG screenshot of the seat.
    Screenshot {
        name: String,
        #[arg(short, long, default_value = "seat.png")]
        output: String,
    },
    /// Move the seat's cursor.
    Move { name: String, x: f32, y: f32 },
    /// Click at a position.
    Click {
        name: String,
        x: f32,
        y: f32,
        #[arg(long, default_value = "left")]
        button: String,
        #[arg(long, default_value_t = 1)]
        count: u32,
    },
    /// Scroll wheel notches at a position (positive dy = down).
    Scroll {
        name: String,
        x: f32,
        y: f32,
        #[arg(long, default_value_t = 0, allow_hyphen_values = true)]
        dx: i32,
        #[arg(long, default_value_t = 0, allow_hyphen_values = true)]
        dy: i32,
    },
    /// Press a key chord, e.g. ctrl+shift+t, Return, alt+F4.
    Key { name: String, chord: String },
    /// Type text into the focused window.
    Type { name: String, text: String },
    /// Run as an MCP server on stdio.
    Mcp,
}

#[derive(Subcommand)]
enum SessionCmd {
    /// Start a new seat.
    Start {
        name: String,
        #[arg(long, default_value = "1280x800")]
        size: String,
        #[arg(long, default_value_t = 1.0)]
        scale: f64,
        /// Also start rootless Xwayland so X11 apps can run in the seat.
        #[arg(long)]
        xwayland: bool,
    },
    /// Stop a seat and all its processes.
    Stop { name: String },
    /// List seats.
    List {
        #[arg(long)]
        json: bool,
    },
}

fn parse_size(s: &str) -> Result<(u32, u32)> {
    let Some((w, h)) = s.split_once('x') else {
        bail!("size must look like 1280x800");
    };
    Ok((w.parse()?, h.parse()?))
}

fn main() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::Session(SessionCmd::Start { name, size, scale, xwayland }) => {
            let (w, h) = parse_size(&size)?;
            let mut spec = session::SessionSpec::new(&name);
            spec.width = w;
            spec.height = h;
            spec.scale = scale;
            spec.xwayland = xwayland;
            let info = session::start(&spec)?;
            println!(
                "seat '{}' up: {}x{}  wayland={}  kwin_pid={}  dbus_pid={}\nstate: {}",
                info.name, info.width, info.height, info.socket, info.kwin.pid, info.dbus.pid,
                info.dir.display()
            );
        }
        Cmd::Session(SessionCmd::Stop { name }) => {
            session::stop(&name)?;
            println!("seat '{name}' stopped");
        }
        Cmd::Session(SessionCmd::List { json }) => {
            let list = session::list()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&list)?);
            } else if list.is_empty() {
                println!("no seats");
            } else {
                for s in list {
                    println!(
                        "{:<16} {} {}x{} apps={} kwin_pid={}",
                        s.name,
                        if s.kwin.alive() { "up  " } else { "DEAD" },
                        s.width,
                        s.height,
                        s.apps.iter().filter(|p| p.alive()).count(),
                        s.kwin.pid
                    );
                }
            }
        }
        Cmd::Launch { name, command } => {
            let p = session::launch(&name, &command, &[])?;
            println!("launched pid {} in seat '{name}'", p.pid);
        }
        Cmd::Screenshot { name, output } => {
            let png = ops::screenshot_png(&name)?;
            std::fs::write(&output, &png)?;
            println!("{output} ({} bytes)", png.len());
        }
        Cmd::Move { name, x, y } => ops::move_to(&name, x, y)?,
        Cmd::Click { name, x, y, button, count } => ops::click(&name, x, y, &button, count)?,
        Cmd::Scroll { name, x, y, dx, dy } => ops::scroll(&name, x, y, dx, dy)?,
        Cmd::Key { name, chord } => ops::key(&name, &chord)?,
        Cmd::Type { name, text } => ops::type_text(&name, &text)?,
        Cmd::Mcp => mcp::serve()?,
    }
    Ok(())
}
