//! Which key an LED lights: OpenRGB names keyboard LEDs "Key: A", "Key: Left
//! Shift", "Key: Number Pad 7" and so on; type lighting needs them as the
//! Windows virtual-key codes the keyboard hook reports.

/// The virtual-key code for an OpenRGB LED name, if it's a key.
pub fn vk_for(led_name: &str) -> Option<u16> {
    let name = led_name.strip_prefix("Key: ")?.trim();
    // Letters and the number row.
    if name.len() == 1 {
        let c = name.chars().next()?.to_ascii_uppercase();
        if c.is_ascii_alphanumeric() {
            return Some(c as u16);
        }
        return Some(match c {
            '`' => 0xC0,
            '-' => 0xBD,
            '=' => 0xBB,
            '[' => 0xDB,
            ']' => 0xDD,
            '\\' => 0xDC,
            ';' => 0xBA,
            '\'' => 0xDE,
            ',' => 0xBC,
            '.' => 0xBE,
            '/' => 0xBF,
            _ => return None,
        });
    }
    if let Some(n) = name.strip_prefix('F').and_then(|n| n.parse::<u16>().ok()) {
        if (1..=24).contains(&n) {
            return Some(0x70 + n - 1);
        }
    }
    if let Some(rest) = name.strip_prefix("Number Pad ") {
        if let Ok(d) = rest.parse::<u16>() {
            return (d <= 9).then_some(0x60 + d);
        }
        return Some(match rest {
            "/" => 0x6F,
            "*" => 0x6A,
            "-" => 0x6D,
            "+" => 0x6B,
            "." => 0x6E,
            "Enter" => 0x0D,
            _ => return None,
        });
    }
    let lower = name.to_ascii_lowercase();
    Some(match lower.as_str() {
        "escape" | "esc" => 0x1B,
        "backspace" => 0x08,
        "tab" => 0x09,
        "caps lock" => 0x14,
        "enter" | "enter (iso)" => 0x0D,
        "left shift" => 0xA0,
        "right shift" => 0xA1,
        "left control" | "left ctrl" => 0xA2,
        "right control" | "right ctrl" => 0xA3,
        "left windows" | "left win" => 0x5B,
        "right windows" | "right win" => 0x5C,
        "left alt" => 0xA4,
        "right alt" => 0xA5,
        "menu" | "right fn" => 0x5D,
        "space" => 0x20,
        "print screen" => 0x2C,
        "scroll lock" => 0x91,
        "pause/break" | "pause" => 0x13,
        "insert" => 0x2D,
        "home" => 0x24,
        "page up" => 0x21,
        "delete" => 0x2E,
        "end" => 0x23,
        "page down" => 0x22,
        "up arrow" => 0x26,
        "left arrow" => 0x25,
        "down arrow" => 0x28,
        "right arrow" => 0x27,
        "num lock" => 0x90,
        "\\ (ansi)" | "#" | "# (iso)" => 0xDC,
        "\\ (iso)" => 0xE2,
        "media play/pause" => 0xB3,
        "media next" => 0xB0,
        "media previous" => 0xB1,
        "media stop" => 0xB2,
        "media mute" => 0xAD,
        "media volume +" => 0xAF,
        "media volume -" => 0xAE,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_to_keys() {
        assert_eq!(vk_for("Key: A"), Some(b'A' as u16));
        assert_eq!(vk_for("Key: 7"), Some(b'7' as u16));
        assert_eq!(vk_for("Key: F12"), Some(0x7B));
        assert_eq!(vk_for("Key: Left Shift"), Some(0xA0));
        assert_eq!(vk_for("Key: Number Pad 7"), Some(0x67));
        assert_eq!(vk_for("Key: Number Pad Enter"), Some(0x0D));
        assert_eq!(vk_for("Key: Escape"), Some(0x1B));
        assert_eq!(vk_for("Logo"), None);
    }
}
