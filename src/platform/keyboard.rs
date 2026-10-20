//! Physical US keyboard mapping for clients that do not forward VK_PACKET text.
use crate::error::{DesktopError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ScanKey {
    pub scan: u16,
    pub shift: bool,
}

pub(super) fn scan_code_text(text: &str) -> Result<Vec<ScanKey>> {
    // Validate the entire request before sending even the first key.
    text.chars()
        .map(|ch| {
            let (scan, shift) = match ch {
                'a'..='z' | 'A'..='Z' => {
                    const LETTERS: [u16; 26] = [
                        0x1e, 0x30, 0x2e, 0x20, 0x12, 0x21, 0x22, 0x23, 0x17,
                        0x24, 0x25, 0x26, 0x32, 0x31, 0x18, 0x19, 0x10, 0x13,
                        0x1f, 0x14, 0x16, 0x2f, 0x11, 0x2d, 0x15, 0x2c,
                    ];
                    (LETTERS[(ch.to_ascii_lowercase() as u8 - b'a') as usize], ch.is_ascii_uppercase())
                }
                '1'..='9' => (u16::from(ch as u8 - b'1') + 2, false),
                '0' => (0x0b, false),
                ' ' => (0x39, false),
                _ => {
                    const PUNCTUATION: [(char, char, u16); 21] = [
                        ('1', '!', 0x02), ('2', '@', 0x03), ('3', '#', 0x04),
                        ('4', '$', 0x05), ('5', '%', 0x06), ('6', '^', 0x07),
                        ('7', '&', 0x08), ('8', '*', 0x09), ('9', '(', 0x0a),
                        ('0', ')', 0x0b), ('-', '_', 0x0c), ('=', '+', 0x0d),
                        ('[', '{', 0x1a), (']', '}', 0x1b), (';', ':', 0x27),
                        ('\'', '"', 0x28), ('`', '~', 0x29), ('\\', '|', 0x2b),
                        (',', '<', 0x33), ('.', '>', 0x34), ('/', '?', 0x35),
                    ];
                    let Some(&(plain, shifted, scan)) = PUNCTUATION.iter()
                        .find(|(plain, shifted, _)| ch == *plain || ch == *shifted) else {
                        return Err(DesktopError::invalid(
                            "scan_code supports printable ASCII only; use unicode for other text and desktop_press_key for Enter/Tab",
                        ));
                    };
                    debug_assert!(ch == plain || ch == shifted);
                    (scan, ch == shifted)
                }
            };
            Ok(ScanKey { scan, shift })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_text_maps_to_physical_keys_and_shift() {
        let keys = scan_code_text("ls -la /tmp").unwrap();
        assert_eq!(
            keys[0],
            ScanKey {
                scan: 0x26,
                shift: false
            }
        );
        assert_eq!(
            keys[1],
            ScanKey {
                scan: 0x1f,
                shift: false
            }
        );
        assert_eq!(
            keys[3],
            ScanKey {
                scan: 0x0c,
                shift: false
            }
        );
        assert_eq!(
            keys[7],
            ScanKey {
                scan: 0x35,
                shift: false
            }
        );
        assert_eq!(
            scan_code_text("A:|").unwrap(),
            vec![
                ScanKey {
                    scan: 0x1e,
                    shift: true
                },
                ScanKey {
                    scan: 0x27,
                    shift: true
                },
                ScanKey {
                    scan: 0x2b,
                    shift: true
                },
            ]
        );
    }

    #[test]
    fn supports_all_printable_ascii_and_rejects_other_text_atomically() {
        let printable: String = (b' '..=b'~').map(char::from).collect();
        assert_eq!(scan_code_text(&printable).unwrap().len(), 95);
        for text in ["ls\n", "ls\r", "ls\t", "ls\0", "ls中文", "ls😀"] {
            assert!(scan_code_text(text).is_err());
        }
    }
}
