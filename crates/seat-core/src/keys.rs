//! XKB-backed mapping from characters / key names to evdev keycodes plus the modifiers that
//! must be held, using the keymap that the compositor's EIS server advertises.

use anyhow::{anyhow, bail, Result};
use xkbcommon::xkb;

/// One physical key press: the evdev keycode and the modifier keycodes to hold around it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyStroke {
    pub keycode: u32,
    pub modifiers: Vec<u32>,
}

pub struct KeyMap {
    keymap: xkb::Keymap,
}

const XKB_OFFSET: u32 = 8;

impl KeyMap {
    /// Compile the keymap string delivered over EIS.
    pub fn from_string(text: &str) -> Result<Self> {
        let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let keymap = xkb::Keymap::new_from_string(
            &ctx,
            text.to_string(),
            xkb::KEYMAP_FORMAT_TEXT_V1,
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        )
        .ok_or_else(|| anyhow!("failed to compile XKB keymap from EIS"))?;
        Ok(Self { keymap })
    }

    /// A plain `us` keymap, used when the compositor sends no keymap.
    pub fn default_us() -> Result<Self> {
        let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let keymap = xkb::Keymap::new_from_names(
            &ctx,
            "",
            "",
            "us",
            "",
            None,
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        )
        .ok_or_else(|| anyhow!("failed to build default us keymap"))?;
        Ok(Self { keymap })
    }

    /// Find the lowest (keycode, level) producing `sym` on layout 0.
    fn locate(&self, sym: xkb::Keysym) -> Option<(u32, u32)> {
        let min = self.keymap.min_keycode().raw();
        let max = self.keymap.max_keycode().raw();
        let mut best: Option<(u32, u32)> = None;
        for raw in min..=max {
            let kc = xkb::Keycode::new(raw);
            if self.keymap.num_layouts_for_key(kc) == 0 {
                continue;
            }
            let levels = self.keymap.num_levels_for_key(kc, 0);
            for level in 0..levels {
                if self.keymap.key_get_syms_by_level(kc, 0, level).contains(&sym) {
                    let cand = (raw, level);
                    if best.map_or(true, |b| level < b.1) {
                        best = Some(cand);
                    }
                    break;
                }
            }
        }
        best
    }

    fn modifier_key(&self, sym: xkb::Keysym) -> Option<u32> {
        self.locate(sym).map(|(kc, _)| kc - XKB_OFFSET)
    }

    pub fn stroke_for_keysym(&self, sym: xkb::Keysym) -> Option<KeyStroke> {
        let (raw, level) = self.locate(sym)?;
        let mut modifiers = Vec::new();
        // Level numbering for the common TWO_LEVEL / ALPHABETIC / FOUR_LEVEL types.
        if level == 1 || level == 3 {
            modifiers.push(self.modifier_key(xkb::Keysym::Shift_L)?);
        }
        if level == 2 || level == 3 {
            modifiers.push(self.modifier_key(xkb::Keysym::ISO_Level3_Shift)?);
        }
        Some(KeyStroke {
            keycode: raw - XKB_OFFSET,
            modifiers,
        })
    }

    pub fn stroke_for_char(&self, c: char) -> Option<KeyStroke> {
        let sym = match c {
            '\n' | '\r' => xkb::Keysym::Return,
            '\t' => xkb::Keysym::Tab,
            ' ' => xkb::Keysym::space,
            '\u{8}' => xkb::Keysym::BackSpace,
            '\u{1b}' => xkb::Keysym::Escape,
            c => xkb::utf32_to_keysym(c as u32),
        };
        self.stroke_for_keysym(sym)
    }

    /// Parse a chord such as `ctrl+shift+t`, `Return`, `alt+F4`, `super+e`.
    /// Returns the keycodes to press in order (modifiers first, key last); release in
    /// reverse order.
    pub fn parse_chord(&self, spec: &str) -> Result<Vec<u32>> {
        let parts: Vec<&str> = spec.split('+').map(str::trim).filter(|p| !p.is_empty()).collect();
        if parts.is_empty() {
            bail!("empty key chord");
        }
        let (key_name, mod_names) = parts.split_last().expect("non-empty");
        let mut seq: Vec<u32> = Vec::new();
        for m in mod_names {
            let sym = match m.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => xkb::Keysym::Control_L,
                "alt" | "option" => xkb::Keysym::Alt_L,
                "shift" => xkb::Keysym::Shift_L,
                "super" | "meta" | "win" | "cmd" => xkb::Keysym::Super_L,
                "altgr" => xkb::Keysym::ISO_Level3_Shift,
                other => bail!("unknown modifier '{other}'"),
            };
            let kc = self
                .modifier_key(sym)
                .ok_or_else(|| anyhow!("modifier '{m}' not in keymap"))?;
            if !seq.contains(&kc) {
                seq.push(kc);
            }
        }
        let mut chars = key_name.chars();
        let sym = match (chars.next(), chars.next()) {
            (Some(c), None) => {
                if let Some(s) = self.stroke_for_char(c).map(|_| xkb::utf32_to_keysym(c as u32)) {
                    s
                } else {
                    bail!("no key produces '{c}'");
                }
            }
            _ => {
                let alias = match key_name.to_ascii_lowercase().as_str() {
                    "enter" | "return" => "Return",
                    "esc" | "escape" => "Escape",
                    "space" => "space",
                    "tab" => "Tab",
                    "backspace" => "BackSpace",
                    "delete" | "del" => "Delete",
                    "up" => "Up",
                    "down" => "Down",
                    "left" => "Left",
                    "right" => "Right",
                    "home" => "Home",
                    "end" => "End",
                    "pageup" | "pgup" => "Prior",
                    "pagedown" | "pgdn" => "Next",
                    _ => key_name,
                };
                let s = xkb::keysym_from_name(alias, xkb::KEYSYM_CASE_INSENSITIVE);
                if s.raw() == 0 {
                    bail!("unknown key '{key_name}'");
                }
                s
            }
        };
        let stroke = self
            .stroke_for_keysym(sym)
            .ok_or_else(|| anyhow!("key '{key_name}' not present in keymap"))?;
        for m in stroke.modifiers {
            if !seq.contains(&m) {
                seq.push(m);
            }
        }
        seq.push(stroke.keycode);
        Ok(seq)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_lower_upper_and_symbols() {
        let km = KeyMap::default_us().expect("us keymap");
        let a = km.stroke_for_char('a').unwrap();
        assert_eq!(a.keycode, 30); // KEY_A
        assert!(a.modifiers.is_empty());
        let upper = km.stroke_for_char('A').unwrap();
        assert_eq!(upper.keycode, 30);
        assert_eq!(upper.modifiers, vec![42]); // KEY_LEFTSHIFT
        let bang = km.stroke_for_char('!').unwrap();
        assert_eq!(bang.keycode, 2); // KEY_1
        assert_eq!(bang.modifiers, vec![42]);
        assert_eq!(km.stroke_for_char('\n').unwrap().keycode, 28); // KEY_ENTER
    }

    #[test]
    fn parses_chords() {
        let km = KeyMap::default_us().expect("us keymap");
        assert_eq!(km.parse_chord("ctrl+shift+t").unwrap(), vec![29, 42, 20]);
        assert_eq!(km.parse_chord("Return").unwrap(), vec![28]);
        assert_eq!(km.parse_chord("alt+F4").unwrap(), vec![56, 62]);
        assert!(km.parse_chord("hyper+x").is_err());
    }
}
