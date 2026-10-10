//! Screenshot capture from a nested KWin via `org.kde.KWin.ScreenShot2`.

use std::collections::HashMap;
use std::io::Read;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::net::UnixStream;

use anyhow::{anyhow, bail, Context, Result};
use zbus::zvariant::{Fd, OwnedValue, Value};

pub fn connect_bus(address: &str) -> Result<zbus::blocking::Connection> {
    zbus::blocking::connection::Builder::address(address)
        .context("bad bus address")?
        .build()
        .context("connecting to the session's private bus")
}

/// Raw image as delivered by KWin, converted to tightly packed RGB8.
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgb: Vec<u8>,
}

fn get_u32(map: &HashMap<String, OwnedValue>, key: &str) -> Result<u32> {
    let v = map.get(key).ok_or_else(|| anyhow!("ScreenShot2 reply missing '{key}'"))?;
    u32::try_from(v).map_err(|e| anyhow!("ScreenShot2 '{key}' not u32: {e}"))
}

/// Capture the whole workspace (all outputs) including the agent's cursor.
pub fn capture_workspace(conn: &zbus::blocking::Connection) -> Result<Image> {
    // A pipe would also work, but a socketpair avoids the 64 KiB pipe limit surprises.
    let (rd, wr) = UnixStream::pair()?;
    let reader = std::thread::spawn(move || -> std::io::Result<Vec<u8>> {
        let mut buf = Vec::new();
        let mut rd = rd;
        rd.read_to_end(&mut buf)?;
        Ok(buf)
    });

    let mut options: HashMap<&str, Value> = HashMap::new();
    options.insert("include-cursor", Value::Bool(true));
    options.insert("native-resolution", Value::Bool(true));
    let wr_fd: OwnedFd = wr.into();
    let reply = conn.call_method(
        Some("org.kde.KWin"),
        "/org/kde/KWin/ScreenShot2",
        Some("org.kde.KWin.ScreenShot2"),
        "CaptureWorkspace",
        &(options, Fd::from(wr_fd.as_fd())),
    );
    // Close our write end so the reader sees EOF once KWin closes its copy.
    drop(wr_fd);
    let reply = reply.context("ScreenShot2.CaptureWorkspace failed")?;
    let meta: HashMap<String, OwnedValue> = reply.body().deserialize()?;
    let data = reader
        .join()
        .map_err(|_| anyhow!("screenshot reader thread panicked"))??;

    let width = get_u32(&meta, "width")?;
    let height = get_u32(&meta, "height")?;
    let stride = get_u32(&meta, "stride")? as usize;
    let format = get_u32(&meta, "format")?;
    if data.len() < stride * height as usize {
        bail!(
            "short screenshot: got {} bytes, expected {}",
            data.len(),
            stride * height as usize
        );
    }
    let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
    match format {
        // QImage RGB32 / ARGB32 / ARGB32_Premultiplied: little-endian BGRA in memory.
        4 | 5 | 6 => {
            for y in 0..height as usize {
                let row = &data[y * stride..y * stride + width as usize * 4];
                for px in row.chunks_exact(4) {
                    rgb.extend_from_slice(&[px[2], px[1], px[0]]);
                }
            }
        }
        // RGBX8888 / RGBA8888 / RGBA8888_Premultiplied: R,G,B,A in memory.
        16 | 17 | 18 => {
            for y in 0..height as usize {
                let row = &data[y * stride..y * stride + width as usize * 4];
                for px in row.chunks_exact(4) {
                    rgb.extend_from_slice(&[px[0], px[1], px[2]]);
                }
            }
        }
        other => bail!("unsupported QImage format {other} from ScreenShot2"),
    }
    Ok(Image { width, height, rgb })
}

pub fn encode_png(img: &Image) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, img.width, img.height);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header()?;
        w.write_image_data(&img.rgb)?;
    }
    Ok(out)
}
