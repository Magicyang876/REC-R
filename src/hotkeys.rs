//! Parsing and formatting of hotkey specs such as `Ctrl+Alt+S`.
//!
//! Kept dependency-free (no Windows types) so it can be unit tested and reused
//! by the CLI/MCP when validating user input.

use anyhow::{bail, Result};

pub const MOD_ALT: u32 = 0x0001;
pub const MOD_CONTROL: u32 = 0x0002;
pub const MOD_SHIFT: u32 = 0x0004;
pub const MOD_WIN: u32 = 0x0008;
pub const MOD_NOREPEAT: u32 = 0x4000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hotkey {
    pub mods: u32,
    pub vk: u32,
}

impl Hotkey {
    /// `Ctrl+Alt+S` style label, with `NoRepeat` hidden.
    pub fn label(&self) -> String {
        let mut parts = Vec::new();
        if self.mods & MOD_CONTROL != 0 {
            parts.push("Ctrl".to_string());
        }
        if self.mods & MOD_ALT != 0 {
            parts.push("Alt".to_string());
        }
        if self.mods & MOD_SHIFT != 0 {
            parts.push("Shift".to_string());
        }
        if self.mods & MOD_WIN != 0 {
            parts.push("Win".to_string());
        }
        parts.push(vk_name(self.vk));
        parts.join("+")
    }

    /// Same combination with Shift toggled -- the fallback tried when another
    /// application already owns the configured one.
    pub fn shifted_fallback(&self) -> Hotkey {
        Hotkey {
            mods: self.mods ^ MOD_SHIFT,
            vk: self.vk,
        }
    }

    /// Modifier bits as `RegisterHotKey` wants them.
    pub fn register_mods(&self) -> u32 {
        self.mods | MOD_NOREPEAT
    }
}

/// `None` for an empty spec, which means "disabled".
pub fn parse(spec: &str) -> Result<Option<Hotkey>> {
    let spec = spec.trim();
    if spec.is_empty() || spec.eq_ignore_ascii_case("none") || spec == "off" {
        return Ok(None);
    }
    let mut mods = 0u32;
    let mut vk: Option<u32> = None;
    for token in spec.split('+').map(str::trim).filter(|t| !t.is_empty()) {
        let lower = token.to_ascii_lowercase();
        match lower.as_str() {
            "ctrl" | "control" | "ctl" => mods |= MOD_CONTROL,
            "alt" => mods |= MOD_ALT,
            "shift" => mods |= MOD_SHIFT,
            "win" | "super" | "meta" | "cmd" => mods |= MOD_WIN,
            _ => {
                if vk.is_some() {
                    let first = vk_name(vk.unwrap());
                    bail!("hotkey {spec:?}: more than one key ({first} and {token})");
                }
                vk = Some(key_code(token).ok_or_else(|| anyhow::anyhow!("hotkey {spec:?}: unknown key {token:?}"))?);
            }
        }
    }
    let vk = vk.ok_or_else(|| anyhow::anyhow!("hotkey {spec:?}: no key, only modifiers"))?;
    if mods == 0 {
        bail!("hotkey {spec:?}: needs at least one of Ctrl/Alt/Shift/Win so it does not swallow typing");
    }
    Ok(Some(Hotkey { mods, vk }))
}

fn key_code(token: &str) -> Option<u32> {
    let upper = token.to_ascii_uppercase();
    if upper.len() == 1 {
        let c = upper.as_bytes()[0] as char;
        if c.is_ascii_alphanumeric() {
            return Some(c as u32);
        }
    }
    // F1..F24
    if let Some(rest) = upper.strip_prefix('F') {
        if let Ok(n) = rest.parse::<u32>() {
            if (1..=24).contains(&n) {
                return Some(0x70 + n - 1);
            }
        }
    }
    let named = match upper.as_str() {
        "PRINTSCREEN" | "PRTSC" | "SNAPSHOT" => 0x2C,
        "INSERT" | "INS" => 0x2D,
        "DELETE" | "DEL" => 0x2E,
        "HOME" => 0x24,
        "END" => 0x23,
        "PAGEUP" | "PGUP" => 0x21,
        "PAGEDOWN" | "PGDN" => 0x22,
        "SPACE" => 0x20,
        "TAB" => 0x09,
        "ENTER" | "RETURN" => 0x0D,
        "BACKSPACE" | "BACK" => 0x08,
        "ESC" | "ESCAPE" => 0x1B,
        "PAUSE" => 0x13,
        "SCROLLLOCK" => 0x91,
        "CAPSLOCK" => 0x14,
        "LEFT" => 0x25,
        "UP" => 0x26,
        "RIGHT" => 0x27,
        "DOWN" => 0x28,
        "NUMPAD0" => 0x60,
        "NUMPAD1" => 0x61,
        "NUMPAD2" => 0x62,
        "NUMPAD3" => 0x63,
        "NUMPAD4" => 0x64,
        "NUMPAD5" => 0x65,
        "NUMPAD6" => 0x66,
        "NUMPAD7" => 0x67,
        "NUMPAD8" => 0x68,
        "NUMPAD9" => 0x69,
        _ => {
            // Allow raw virtual-key codes for anything exotic: `Ctrl+Alt+0xA6`.
            let raw = upper.strip_prefix("VK=").unwrap_or(upper.as_str());
            return raw
                .strip_prefix("0X")
                .and_then(|hex| u32::from_str_radix(hex, 16).ok());
        }
    };
    Some(named)
}

pub fn vk_name(vk: u32) -> String {
    match vk {
        0x30..=0x39 | 0x41..=0x5A => char::from(vk as u8).to_string(),
        0x70..=0x87 => format!("F{}", vk - 0x70 + 1),
        0x60..=0x69 => format!("Numpad{}", vk - 0x60),
        _ => match vk {
            0x2C => "PrintScreen".into(),
            0x2D => "Insert".into(),
            0x2E => "Delete".into(),
            0x24 => "Home".into(),
            0x23 => "End".into(),
            0x21 => "PageUp".into(),
            0x22 => "PageDown".into(),
            0x20 => "Space".into(),
            0x09 => "Tab".into(),
            0x0D => "Enter".into(),
            0x08 => "Backspace".into(),
            0x1B => "Esc".into(),
            0x13 => "Pause".into(),
            0x25 => "Left".into(),
            0x26 => "Up".into(),
            0x27 => "Right".into(),
            0x28 => "Down".into(),
            other => format!("VK=0x{other:02X}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_defaults() {
        let h = parse("Ctrl+Alt+S").unwrap().unwrap();
        assert_eq!(h.mods, MOD_CONTROL | MOD_ALT);
        assert_eq!(h.vk, 'S' as u32);
        assert_eq!(h.label(), "Ctrl+Alt+S");
        assert_eq!(h.shifted_fallback().label(), "Ctrl+Alt+Shift+S");
    }

    #[test]
    fn case_and_spacing_insensitive() {
        assert_eq!(parse("ctrl + alt + shift + f5").unwrap().unwrap().label(), "Ctrl+Alt+Shift+F5");
        assert_eq!(parse("WIN+PrintScreen").unwrap().unwrap().label(), "Win+PrintScreen");
    }

    #[test]
    fn disabled_and_invalid() {
        assert!(parse("").unwrap().is_none());
        assert!(parse("none").unwrap().is_none());
        assert!(parse("S").is_err());
        assert!(parse("Ctrl+Alt").is_err());
        assert!(parse("Ctrl+Alt+S+X").is_err());
        assert!(parse("Ctrl+Alt+Nope").is_err());
    }
}