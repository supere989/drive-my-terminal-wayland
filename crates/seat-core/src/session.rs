//! Nested KWin session manager (backend A).
//!
//! Each session is a private `dbus-daemon` plus a headless `kwin_wayland --virtual` with its
//! own Wayland socket and its own `XDG_CONFIG_HOME` / `XDG_STATE_HOME` / `XDG_CACHE_HOME`,
//! so nothing it does touches the human's compositor or configuration.
//!
//! Every child PID (with its `/proc` start time, to defend against PID reuse) is recorded in
//! `session.json`; teardown terminates by PID and never uses `pkill`.

use std::fs::{self, File};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Proc {
    pub pid: u32,
    /// Field 22 of `/proc/<pid>/stat` (clock ticks since boot); `0` if unknown.
    pub start_time: u64,
    pub cmd: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub name: String,
    /// Wayland socket name inside `$XDG_RUNTIME_DIR`.
    pub socket: String,
    pub bus_address: String,
    pub width: u32,
    pub height: u32,
    pub scale: f64,
    pub xwayland: bool,
    pub display: Option<String>,
    pub started_at_unix: u64,
    pub dbus: Proc,
    pub kwin: Proc,
    pub apps: Vec<Proc>,
    pub dir: PathBuf,
}

#[derive(Debug, Clone)]
pub struct SessionSpec {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub scale: f64,
    pub xwayland: bool,
    pub display: Option<String>,
}

impl SessionSpec {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            width: 1280,
            height: 800,
            scale: 1.0,
            xwayland: false,
            display: None,
        }
    }
}

pub fn runtime_dir() -> Result<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("XDG_RUNTIME_DIR is not set"))
}

pub fn state_root() -> Result<PathBuf> {
    Ok(runtime_dir()?.join("input-director"))
}

fn session_dir(name: &str) -> Result<PathBuf> {
    validate_name(name)?;
    Ok(state_root()?.join(name))
}

pub fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 32
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        bail!("session name must be 1-32 chars of [A-Za-z0-9_-]");
    }
    Ok(())
}

fn proc_start_time(pid: u32) -> u64 {
    let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return 0;
    };
    // The comm field may contain spaces/parens; fields after the last ')' are stable.
    let Some(rest) = stat.rsplit_once(')').map(|x| x.1) else {
        return 0;
    };
    rest.split_whitespace()
        .nth(19) // field 22 overall: state is field 3 -> index 0, so starttime is index 19
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

impl Proc {
    fn new(pid: u32, cmd: impl Into<String>) -> Self {
        Self {
            pid,
            start_time: proc_start_time(pid),
            cmd: cmd.into(),
        }
    }

    /// True only if the same process (pid *and* start time) is still running.
    pub fn alive(&self) -> bool {
        if !Path::new(&format!("/proc/{}", self.pid)).exists() {
            return false;
        }
        let now = proc_start_time(self.pid);
        if self.start_time != 0 && now != 0 && now != self.start_time {
            return false;
        }
        // A zombie counts as dead.
        if let Ok(stat) = fs::read_to_string(format!("/proc/{}/stat", self.pid)) {
            if let Some(rest) = stat.rsplit_once(')').map(|x| x.1) {
                if rest.trim_start().starts_with('Z') {
                    return false;
                }
            }
        }
        true
    }

    fn signal(&self, sig: i32) {
        if self.alive() {
            // SAFETY: plain kill(2) on a PID we verified is the process we spawned.
            unsafe {
                libc::kill(self.pid as i32, sig);
            }
        }
    }
}

fn env_for(info_dir: &Path, bus: &str) -> Vec<(String, String)> {
    vec![
        ("DBUS_SESSION_BUS_ADDRESS".into(), bus.into()),
        ("XDG_CONFIG_HOME".into(), info_dir.join("config").display().to_string()),
        ("XDG_STATE_HOME".into(), info_dir.join("state").display().to_string()),
        ("XDG_CACHE_HOME".into(), info_dir.join("cache").display().to_string()),
        ("XDG_SESSION_TYPE".into(), "wayland".into()),
    ]
}

/// Detach the child into its own session so it outlives the CLI process.
fn detach(cmd: &mut Command) {
    // SAFETY: setsid() is async-signal-safe and allowed between fork and exec.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
}

fn wait_for<F: FnMut() -> bool>(what: &str, timeout: Duration, mut f: F) -> Result<()> {
    let end = Instant::now() + timeout;
    while Instant::now() < end {
        if f() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    bail!("timed out waiting for {what}")
}

fn kwin_on_bus(bus: &str) -> bool {
    let Ok(builder) = zbus::blocking::connection::Builder::address(bus) else {
        return false;
    };
    let Ok(conn) = builder.build() else {
        return false;
    };
    let Ok(dbus) = zbus::blocking::fdo::DBusProxy::new(&conn) else {
        return false;
    };
    let Ok(name) = zbus::names::BusName::try_from("org.kde.KWin") else {
        return false;
    };
    dbus.name_has_owner(name).unwrap_or(false)
}

pub fn start(spec: &SessionSpec) -> Result<SessionInfo> {
    let dir = session_dir(&spec.name)?;
    if dir.join("session.json").exists() {
        let existing = load(&spec.name)?;
        if existing.kwin.alive() {
            bail!("session '{}' is already running", spec.name);
        }
        // Stale state from a crashed session: clean it up first.
        stop(&spec.name)?;
    }
    let root = state_root()?;
    fs::DirBuilder::new().recursive(true).mode(0o700).create(&root)?;
    fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
    for sub in ["config", "state", "cache"] {
        fs::DirBuilder::new().recursive(true).mode(0o700).create(dir.join(sub))?;
    }

    let socket = format!("idir-{}", spec.name);
    let bus_path = dir.join("bus");
    let bus_address = format!("unix:path={}", bus_path.display());

    // 1. private session bus
    let mut dbus_cmd = Command::new("dbus-daemon");
    dbus_cmd
        .args(["--session", "--nofork", "--nopidfile"])
        .arg(format!("--address={bus_address}"))
        .stdin(Stdio::null())
        .stdout(Stdio::from(File::create(dir.join("dbus.log"))?))
        .stderr(Stdio::from(File::create(dir.join("dbus.err.log"))?));
    detach(&mut dbus_cmd);
    let dbus_child = dbus_cmd.spawn().context("spawning dbus-daemon")?;
    let dbus = Proc::new(dbus_child.id(), "dbus-daemon");
    std::mem::forget(dbus_child);
    if let Err(e) = wait_for("private bus socket", Duration::from_secs(5), || bus_path.exists()) {
        dbus.signal(libc::SIGKILL);
        let _ = fs::remove_dir_all(&dir);
        return Err(e);
    }
    let _ = fs::set_permissions(&bus_path, fs::Permissions::from_mode(0o600));

    // 2. nested compositor
    let mut kwin_cmd = Command::new("kwin_wayland");
    if let Some(ref _disp) = spec.display {
        let host_wayland = std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "wayland-0".into());
        kwin_cmd
            .args(["--wayland-display", &host_wayland, "--socket", &socket])
            .args(["--width", &spec.width.to_string()])
            .args(["--height", &spec.height.to_string()])
            .args(["--scale", &spec.scale.to_string()])
            .args(["--no-lockscreen", "--no-global-shortcuts"]);
        if spec.xwayland {
            kwin_cmd.arg("--xwayland");
        }
        kwin_cmd
            .env("WAYLAND_DISPLAY", host_wayland)
            .env("DISPLAY", std::env::var("DISPLAY").unwrap_or_else(|_| ":0".into()))
            .env("KWIN_SCREENSHOT_NO_PERMISSION_CHECKS", "1");
    } else {
        kwin_cmd
            .args(["--virtual", "--socket", &socket])
            .args(["--width", &spec.width.to_string()])
            .args(["--height", &spec.height.to_string()])
            .args(["--scale", &spec.scale.to_string()])
            .args(["--no-lockscreen", "--no-global-shortcuts"]);
        if spec.xwayland {
            kwin_cmd.arg("--xwayland");
        }
        kwin_cmd
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("DISPLAY")
            .env("KWIN_SCREENSHOT_NO_PERMISSION_CHECKS", "1");
    }
    kwin_cmd
        .stdin(Stdio::null())
        .stdout(Stdio::from(File::create(dir.join("kwin.log"))?))
        .stderr(Stdio::from(File::create(dir.join("kwin.err.log"))?));
    for (k, v) in env_for(&dir, &bus_address) {
        kwin_cmd.env(k, v);
    }
    detach(&mut kwin_cmd);
    let kwin_child = match kwin_cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            dbus.signal(libc::SIGKILL);
            let _ = fs::remove_dir_all(&dir);
            return Err(anyhow!(e).context("spawning kwin_wayland"));
        }
    };
    let kwin = Proc::new(
        kwin_child.id(),
        if let Some(ref d) = spec.display {
            format!("kwin_wayland --wayland-display ({d})")
        } else {
            "kwin_wayland --virtual".into()
        },
    );
    std::mem::forget(kwin_child);

    let socket_path = runtime_dir()?.join(&socket);
    let ready = wait_for("kwin_wayland to become ready", Duration::from_secs(20), || {
        kwin.alive() && socket_path.exists() && kwin_on_bus(&bus_address)
    });
    if let Err(e) = ready {
        kwin.signal(libc::SIGKILL);
        dbus.signal(libc::SIGKILL);
        let tail = fs::read_to_string(dir.join("kwin.err.log")).unwrap_or_default();
        let _ = fs::remove_dir_all(&dir);
        return Err(e.context(format!("kwin stderr tail: {}", tail.lines().rev().take(5).collect::<Vec<_>>().join(" | "))));
    }

    let info = SessionInfo {
        name: spec.name.clone(),
        socket,
        bus_address,
        width: spec.width,
        height: spec.height,
        scale: spec.scale,
        xwayland: spec.xwayland,
        display: spec.display.clone(),
        started_at_unix: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        dbus,
        kwin,
        apps: vec![],
        dir: dir.clone(),
    };
    save(&info)?;
    Ok(info)
}

fn save(info: &SessionInfo) -> Result<()> {
    let tmp = info.dir.join("session.json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(info)?)?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
    fs::rename(tmp, info.dir.join("session.json"))?;
    Ok(())
}

pub fn load(name: &str) -> Result<SessionInfo> {
    let path = session_dir(name)?.join("session.json");
    let data = fs::read(&path).with_context(|| format!("no such session '{name}'"))?;
    Ok(serde_json::from_slice(&data)?)
}

pub fn list() -> Result<Vec<SessionInfo>> {
    let root = state_root()?;
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(&root) else {
        return Ok(out);
    };
    for entry in rd.flatten() {
        if let Some(name) = entry.file_name().to_str() {
            if let Ok(info) = load(name) {
                out.push(info);
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Launch a program inside the session, optionally as a specific subaccount.
pub fn launch_as(name: &str, user: Option<&str>, argv: &[String], extra_env: &[(String, String)]) -> Result<Proc> {
    let mut info = load(name)?;
    if !info.kwin.alive() {
        bail!("session '{name}' is not running");
    }
    let socket_path = runtime_dir()?.join(&info.socket);
    if let Some(username) = user {
        // Ensure subaccount has ACL access to the wayland socket
        crate::subaccount::grant_seat_access(username, &socket_path)?;
    }
    let (prog, args) = argv.split_first().ok_or_else(|| anyhow!("empty command"))?;
    let log = info.dir.join(format!("app-{}.log", info.apps.len()));
    let mut cmd = if let Some(username) = user {
        let mut c = Command::new("sudo");
        c.args(["-u", username, prog]);
        c.args(args);
        c
    } else {
        let mut c = Command::new(prog);
        c.args(args);
        c
    };
    cmd.env_remove("DISPLAY")
        .env("WAYLAND_DISPLAY", &info.socket)
        .env("QT_QPA_PLATFORM", "wayland")
        .env("GDK_BACKEND", "wayland")
        .env("MOZ_ENABLE_WAYLAND", "1")
        .env("ELECTRON_OZONE_PLATFORM_HINT", "wayland")
        .env("SDL_VIDEODRIVER", "wayland")
        .stdin(Stdio::null())
        .stdout(Stdio::from(File::create(&log)?))
        .stderr(Stdio::from(File::options().append(true).create(true).open(&log)?));
    for (k, v) in env_for(&info.dir, &info.bus_address) {
        cmd.env(k, v);
    }
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    detach(&mut cmd);
    let child = cmd.spawn().with_context(|| format!("launching {prog}"))?;
    let proc = Proc::new(
        child.id(),
        format!("{}{}", user.map_or("".into(), |u| format!("[{u}] ")), argv.join(" ")),
    );
    std::mem::forget(child);
    info.apps.push(proc.clone());
    save(&info)?;
    Ok(proc)
}

/// Launch a program inside the session. Returns the recorded process.
pub fn launch(name: &str, argv: &[String], extra_env: &[(String, String)]) -> Result<Proc> {
    launch_as(name, None, argv, extra_env)
}

fn terminate(procs: &[&Proc]) {
    for p in procs {
        p.signal(libc::SIGTERM);
    }
    let end = Instant::now() + Duration::from_secs(3);
    while Instant::now() < end && procs.iter().any(|p| p.alive()) {
        std::thread::sleep(Duration::from_millis(50));
    }
    for p in procs {
        p.signal(libc::SIGKILL);
    }
}

/// Stop a session: apps, then kwin, then the bus. Removes the state directory.
pub fn stop(name: &str) -> Result<()> {
    let dir = session_dir(name)?;
    if let Ok(info) = load(name) {
        let apps: Vec<&Proc> = info.apps.iter().collect();
        terminate(&apps);
        terminate(&[&info.kwin]);
        terminate(&[&info.dbus]);
        let _ = fs::remove_file(runtime_dir()?.join(&info.socket));
        let _ = fs::remove_file(runtime_dir()?.join(format!("{}.lock", info.socket)));
    }
    if dir.exists() {
        fs::remove_dir_all(&dir)?;
    }
    Ok(())
}

/// Count recorded processes of a session that are still alive (for tests / status).
pub fn live_processes(info: &SessionInfo) -> usize {
    info.apps
        .iter()
        .chain([&info.kwin, &info.dbus])
        .filter(|p| p.alive())
        .count()
}
