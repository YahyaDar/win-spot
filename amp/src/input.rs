//! Decodes UTF-16 code units (such as WM_CHAR messages) into Unicode characters.

/// Turns a stream of UTF-16 code units (WM_CHAR values) into chars.
#[derive(Default)]
pub struct Utf16Decoder {
    high: Option<u32>,
}

impl Utf16Decoder {
    /// Feed one unit. Returns a char when one is complete, otherwise None.
    pub fn push(&mut self, unit: u32) -> Option<char> {
        if unit > 0xFFFF {
            return None;
        }
        match unit {
            0xD800..=0xDBFF => {
                self.high = Some(unit);
                None
            }
            0xDC00..=0xDFFF => {
                let hi = self.high.take()?;
                let cp = 0x10000 + ((hi - 0xD800) << 10) + (unit - 0xDC00);
                char::from_u32(cp)
            }
            _ => {
                self.high = None;
                char::from_u32(unit)
            }
        }
    }

    /// Forget a pending high surrogate.
    pub fn reset(&mut self) {
        self.high = None;
    }
}

/// Action to take on a key event while typing in search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypingKeyAction {
    /// Submit query (Enter)
    Submit,
    /// Cancel typing (Esc)
    Cancel,
    /// Backspace
    Backspace,
    /// Clear query (Ctrl+U)
    Clear,
    /// Paste clipboard (Ctrl+V)
    Paste,
    /// Character to be processed via WM_CHAR
    Char,
}

/// Determines the action for a virtual key while typing in search.
pub fn typing_key_action(vk: u16, ctrl: bool) -> TypingKeyAction {
    match vk {
        0x0D => TypingKeyAction::Submit,    // VK_RETURN
        0x1B => TypingKeyAction::Cancel,    // VK_ESCAPE
        0x08 => TypingKeyAction::Backspace, // VK_BACK
        vk if ctrl && vk == b'U' as u16 => TypingKeyAction::Clear,
        vk if ctrl && vk == b'V' as u16 => TypingKeyAction::Paste,
        _ => TypingKeyAction::Char,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ascii() {
        let mut d = Utf16Decoder::default();
        assert_eq!(d.push(b'a' as u32), Some('a'));
    }

    #[test]
    fn test_bmp_non_ascii() {
        let mut d = Utf16Decoder::default();
        assert_eq!(d.push(0xE9), Some('é'));
        assert_eq!(d.push(0x65E5), Some('日'));
        assert_eq!(d.push(0x20AC), Some('€'));
    }

    #[test]
    fn test_emoji_surrogate_pair() {
        let mut d = Utf16Decoder::default();
        // 😀 is U+1F600 -> 0xD83D 0xDE00
        assert_eq!(d.push(0xD83D), None);
        assert_eq!(d.push(0xDE00), Some('😀'));
    }

    #[test]
    fn test_other_surrogate_pairs() {
        let mut d = Utf16Decoder::default();
        // 𝄞 (U+1D11E) -> 0xD834 0xDD1E
        assert_eq!(d.push(0xD834), None);
        assert_eq!(d.push(0xDD1E), Some('𝄞'));

        // Last valid surrogate pair: U+10FFFF -> 0xDBFF 0xDFFF
        assert_eq!(d.push(0xDBFF), None);
        assert_eq!(d.push(0xDFFF), Some('\u{10FFFF}'));
    }

    #[test]
    fn test_lone_low_surrogate() {
        let mut d = Utf16Decoder::default();
        assert_eq!(d.push(0xDE00), None);
    }

    #[test]
    fn test_high_then_normal() {
        let mut d = Utf16Decoder::default();
        assert_eq!(d.push(0xD83D), None);
        assert_eq!(d.push(b'a' as u32), Some('a'));
    }

    #[test]
    fn test_high_high_low() {
        let mut d = Utf16Decoder::default();
        assert_eq!(d.push(0xD834), None);
        // Replace with 0xD83D (emoji high)
        assert_eq!(d.push(0xD83D), None);
        assert_eq!(d.push(0xDE00), Some('😀'));
    }

    #[test]
    fn test_high_reset_low() {
        let mut d = Utf16Decoder::default();
        assert_eq!(d.push(0xD83D), None);
        d.reset();
        assert_eq!(d.push(0xDE00), None);
    }

    #[test]
    fn test_large_units() {
        let mut d = Utf16Decoder::default();
        assert_eq!(d.push(0x10000), None);
        assert_eq!(d.push(0x110000), None);
        assert_eq!(d.push(u32::MAX), None);
    }

    #[test]
    fn test_nul_and_control() {
        let mut d = Utf16Decoder::default();
        assert_eq!(d.push(0), Some('\0'));
        assert_eq!(d.push(0x1B), Some('\x1B'));
        assert_eq!(d.push(b'\n' as u32), Some('\n'));
    }

    #[test]
    fn test_two_emoji_in_a_row() {
        let mut d = Utf16Decoder::default();
        assert_eq!(d.push(0xD83D), None);
        assert_eq!(d.push(0xDE00), Some('😀'));
        // 𝄞 (U+1D11E) -> 0xD834 0xDD1E
        assert_eq!(d.push(0xD834), None);
        assert_eq!(d.push(0xDD1E), Some('𝄞'));
    }

    #[test]
    fn test_exhaustive_no_panic() {
        let mut d = Utf16Decoder::default();
        for unit in 0..=0xFFFF {
            let _ = d.push(unit);
        }
    }

    #[test]
    fn test_typing_key_action() {
        assert_eq!(typing_key_action(0x0D, false), TypingKeyAction::Submit);
        assert_eq!(typing_key_action(0x1B, false), TypingKeyAction::Cancel);
        assert_eq!(typing_key_action(0x08, false), TypingKeyAction::Backspace);
        assert_eq!(typing_key_action(b'U' as u16, true), TypingKeyAction::Clear);
        assert_eq!(typing_key_action(b'U' as u16, false), TypingKeyAction::Char);
        assert_eq!(typing_key_action(b'V' as u16, true), TypingKeyAction::Paste);
        assert_eq!(typing_key_action(b'V' as u16, false), TypingKeyAction::Char);
        assert_eq!(typing_key_action(b'A' as u16, false), TypingKeyAction::Char);
    }
}
