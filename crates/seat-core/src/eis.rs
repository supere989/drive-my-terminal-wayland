//! Input injection into a nested KWin through its private-bus
//! `org.kde.KWin.EIS.RemoteDesktop` service (libei sender context).
//!
//! The EIS devices exported there belong to the *nested* compositor's seat only, so nothing
//! sent here can reach the human's session.

use std::io::{Read, Seek, SeekFrom};
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use reis::ei;
use reis::event::{Connection, Device, DeviceCapability, EiConvertEventIterator, EiEvent};
use zbus::zvariant::OwnedFd as ZOwnedFd;

use crate::keys::KeyMap;
use crate::shot::connect_bus;

/// `connectToEIS` capability bits (see KWin `EisBackend::connectToEIS`).
const CAP_KEYBOARD: i32 = 1;
const CAP_POINTER: i32 = 2;

pub const BTN_LEFT: u32 = 0x110;
pub const BTN_RIGHT: u32 = 0x111;
pub const BTN_MIDDLE: u32 = 0x112;

pub struct Eis {
    connection: Connection,
    _events: EiConvertEventIterator,
    keyboard: Device,
    pointer: Device,
    keymap: KeyMap,
    sequence: u32,
    /// Logical size of the pointer region(s): (x, y, w, h) bounding box.
    pub bounds: (u32, u32, u32, u32),
    // Keep the bus connection alive for the lifetime of the EIS context: KWin drops the
    // context when the requesting D-Bus peer disappears.
    _bus: zbus::blocking::Connection,
}

fn monotonic_us() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: valid pointer to a timespec.
    unsafe {
        libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts);
    }
    ts.tv_sec as u64 * 1_000_000 + ts.tv_nsec as u64 / 1_000
}

fn read_keymap(km: &reis::event::Keymap) -> Result<KeyMap> {
    let fd: OwnedFd = km.fd.try_clone()?;
    let mut file = std::fs::File::from(fd);
    file.seek(SeekFrom::Start(0))?;
    let mut buf = vec![0u8; km.size as usize];
    file.read_exact(&mut buf)?;
    while buf.last() == Some(&0) {
        buf.pop();
    }
    KeyMap::from_string(std::str::from_utf8(&buf).context("keymap is not UTF-8")?)
}

struct Raw {
    connection: Connection,
    events: EiConvertEventIterator,
    keyboard: Device,
    pointer: Device,
    keymap: KeyMap,
    bounds: (u32, u32, u32, u32),
    bus: zbus::blocking::Connection,
}

// SAFETY: reis objects are `Arc`-based and only ever used from one thread at a time here
// (the handshake thread hands the finished value back through a channel).
unsafe impl Send for Raw {}

fn handshake(bus_address: &str) -> Result<Raw> {
    let bus = connect_bus(bus_address)?;
    let reply = bus
        .call_method(
            Some("org.kde.KWin"),
            "/org/kde/KWin/EIS/RemoteDesktop",
            Some("org.kde.KWin.EIS.RemoteDesktop"),
            "connectToEIS",
            &(CAP_KEYBOARD | CAP_POINTER,),
        )
        .context("org.kde.KWin.EIS.RemoteDesktop.connectToEIS failed (is the session running?)")?;
    let (fd, _cookie): (ZOwnedFd, i32) = reply.body().deserialize()?;
    let stream = UnixStream::from(OwnedFd::from(fd));
    let context = ei::Context::new(stream)?;
    let (connection, mut events) =
        context.handshake_blocking("input-director", ei::handshake::ContextType::Sender)?;

    let mut keyboard: Option<Device> = None;
    let mut pointer: Option<Device> = None;
    let mut keyboard_ready = false;
    let mut pointer_ready = false;
    let mut keymap: Option<KeyMap> = None;
    let mut bounds = (0u32, 0u32, 0u32, 0u32);

    while !(keyboard_ready && pointer_ready) {
        let Some(event) = events.next() else {
            bail!("EIS connection closed during setup");
        };
        match event? {
            EiEvent::SeatAdded(evt) => {
                evt.seat.bind_capabilities(
                    DeviceCapability::Keyboard
                        | DeviceCapability::Pointer
                        | DeviceCapability::PointerAbsolute
                        | DeviceCapability::Button
                        | DeviceCapability::Scroll,
                );
                connection.flush().ok();
            }
            EiEvent::DeviceAdded(evt) => {
                let d = evt.device;
                if d.has_capability(DeviceCapability::Keyboard) && keyboard.is_none() {
                    keymap = match d.keymap() {
                        Some(km) => Some(read_keymap(km)?),
                        None => Some(KeyMap::default_us()?),
                    };
                    keyboard = Some(d);
                } else if d.has_capability(DeviceCapability::PointerAbsolute) && pointer.is_none() {
                    let regs = d.regions();
                    if regs.is_empty() {
                        bail!("absolute pointer device has no regions");
                    }
                    let x0 = regs.iter().map(|r| r.x).min().unwrap_or(0);
                    let y0 = regs.iter().map(|r| r.y).min().unwrap_or(0);
                    let x1 = regs.iter().map(|r| r.x + r.width).max().unwrap_or(0);
                    let y1 = regs.iter().map(|r| r.y + r.height).max().unwrap_or(0);
                    bounds = (x0, y0, x1 - x0, y1 - y0);
                    pointer = Some(d);
                }
            }
            EiEvent::DeviceResumed(evt) => {
                if let Some(k) = &keyboard {
                    if k.device() == evt.device.device() {
                        keyboard_ready = true;
                    }
                }
                if let Some(p) = &pointer {
                    if p.device() == evt.device.device() {
                        pointer_ready = true;
                    }
                }
            }
            EiEvent::Disconnected(d) => bail!("EIS disconnected during setup: {d:?}"),
            _ => {}
        }
    }
    Ok(Raw {
        connection,
        events,
        keyboard: keyboard.expect("checked"),
        pointer: pointer.expect("checked"),
        keymap: keymap.expect("checked"),
        bounds,
        bus,
    })
}

impl Eis {
    /// Connect to the session's EIS server and wait for the keyboard + absolute pointer
    /// devices to be resumed (bounded by `timeout`).
    pub fn connect(bus_address: &str, timeout: Duration) -> Result<Self> {
        let (tx, rx) = mpsc::channel();
        let addr = bus_address.to_string();
        std::thread::spawn(move || {
            let _ = tx.send(handshake(&addr));
        });
        let raw = rx
            .recv_timeout(timeout)
            .map_err(|_| anyhow!("timed out waiting for EIS devices from KWin"))??;
        Ok(Self {
            connection: raw.connection,
            _events: raw.events,
            keyboard: raw.keyboard,
            pointer: raw.pointer,
            keymap: raw.keymap,
            sequence: 0,
            bounds: raw.bounds,
            _bus: raw.bus,
        })
    }

    fn emulate<F: FnOnce(&Self) -> Result<()>>(&mut self, dev: &Device, f: F) -> Result<()> {
        let serial = self.connection.serial();
        self.sequence += 1;
        dev.device().start_emulating(serial, self.sequence);
        let r = f(self);
        dev.device().stop_emulating(serial);
        self.connection.flush().ok();
        r
    }

    fn frame(&self, dev: &Device) {
        dev.device().frame(self.connection.serial(), monotonic_us());
        self.connection.flush().ok();
    }

    fn settle() {
        // KWin processes queued events before noticing a disconnect; this is just slack.
        std::thread::sleep(Duration::from_millis(40));
    }

    pub fn move_to(&mut self, x: f32, y: f32) -> Result<()> {
        let dev = self.pointer.clone();
        let abs: ei::PointerAbsolute = dev
            .interface()
            .ok_or_else(|| anyhow!("pointer device lacks ei_pointer_absolute"))?;
        self.emulate(&dev, |s| {
            abs.motion_absolute(x, y);
            s.frame(&dev);
            Ok(())
        })?;
        Self::settle();
        Ok(())
    }

    pub fn click(&mut self, x: f32, y: f32, button: u32, count: u32) -> Result<()> {
        let dev = self.pointer.clone();
        let abs: ei::PointerAbsolute = dev
            .interface()
            .ok_or_else(|| anyhow!("pointer device lacks ei_pointer_absolute"))?;
        let btn: ei::Button = dev
            .interface()
            .ok_or_else(|| anyhow!("pointer device lacks ei_button"))?;
        self.emulate(&dev, |s| {
            abs.motion_absolute(x, y);
            s.frame(&dev);
            std::thread::sleep(Duration::from_millis(20));
            for _ in 0..count.max(1) {
                btn.button(button, ei::button::ButtonState::Press);
                s.frame(&dev);
                std::thread::sleep(Duration::from_millis(15));
                btn.button(button, ei::button::ButtonState::Released);
                s.frame(&dev);
                std::thread::sleep(Duration::from_millis(40));
            }
            Ok(())
        })?;
        Self::settle();
        Ok(())
    }

    /// Press (or release) a button without moving, for drag operations.
    pub fn button(&mut self, button: u32, press: bool) -> Result<()> {
        let dev = self.pointer.clone();
        let btn: ei::Button = dev
            .interface()
            .ok_or_else(|| anyhow!("pointer device lacks ei_button"))?;
        self.emulate(&dev, |s| {
            btn.button(
                button,
                if press {
                    ei::button::ButtonState::Press
                } else {
                    ei::button::ButtonState::Released
                },
            );
            s.frame(&dev);
            Ok(())
        })?;
        Self::settle();
        Ok(())
    }

    /// Scroll in discrete wheel notches (positive y = down, positive x = right).
    pub fn scroll(&mut self, x: f32, y: f32, dx: i32, dy: i32) -> Result<()> {
        let dev = self.pointer.clone();
        let abs: ei::PointerAbsolute = dev
            .interface()
            .ok_or_else(|| anyhow!("pointer device lacks ei_pointer_absolute"))?;
        let sc: ei::Scroll = dev
            .interface()
            .ok_or_else(|| anyhow!("pointer device lacks ei_scroll"))?;
        self.emulate(&dev, |s| {
            abs.motion_absolute(x, y);
            s.frame(&dev);
            std::thread::sleep(Duration::from_millis(20));
            sc.scroll_discrete(dx * 120, dy * 120);
            s.frame(&dev);
            Ok(())
        })?;
        Self::settle();
        Ok(())
    }

    fn key_seq(&mut self, down: &[u32]) -> Result<()> {
        let dev = self.keyboard.clone();
        let kb: ei::Keyboard = dev
            .interface()
            .ok_or_else(|| anyhow!("keyboard device lacks ei_keyboard"))?;
        let down = down.to_vec();
        self.emulate(&dev, |s| {
            for k in &down {
                kb.key(*k, ei::keyboard::KeyState::Press);
                s.frame(&dev);
                std::thread::sleep(Duration::from_millis(8));
            }
            std::thread::sleep(Duration::from_millis(20));
            for k in down.iter().rev() {
                kb.key(*k, ei::keyboard::KeyState::Released);
                s.frame(&dev);
                std::thread::sleep(Duration::from_millis(8));
            }
            Ok(())
        })?;
        Ok(())
    }

    /// Press a chord such as `ctrl+shift+t` and release it.
    pub fn key(&mut self, chord: &str) -> Result<()> {
        let seq = self.keymap.parse_chord(chord)?;
        self.key_seq(&seq)?;
        Self::settle();
        Ok(())
    }

    /// Type text character by character using the session keymap.
    pub fn type_text(&mut self, text: &str) -> Result<()> {
        let dev = self.keyboard.clone();
        let kb: ei::Keyboard = dev
            .interface()
            .ok_or_else(|| anyhow!("keyboard device lacks ei_keyboard"))?;
        let mut strokes = Vec::new();
        for c in text.chars() {
            strokes.push(
                self.keymap
                    .stroke_for_char(c)
                    .ok_or_else(|| anyhow!("no key produces {c:?} in this keymap"))?,
            );
        }
        self.emulate(&dev, |s| {
            for st in &strokes {
                for m in &st.modifiers {
                    kb.key(*m, ei::keyboard::KeyState::Press);
                    s.frame(&dev);
                }
                kb.key(st.keycode, ei::keyboard::KeyState::Press);
                s.frame(&dev);
                std::thread::sleep(Duration::from_millis(6));
                kb.key(st.keycode, ei::keyboard::KeyState::Released);
                s.frame(&dev);
                for m in st.modifiers.iter().rev() {
                    kb.key(*m, ei::keyboard::KeyState::Released);
                    s.frame(&dev);
                }
                std::thread::sleep(Duration::from_millis(6));
            }
            Ok(())
        })?;
        Self::settle();
        Ok(())
    }
}
