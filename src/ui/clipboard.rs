//! System-clipboard writes via the OSC 52 terminal escape. OSC 52 is delivered
//! in-band on stdout, so it works over SSH (the *local* terminal performs the
//! copy) where a native clipboard crate would write the remote host's
//! clipboard. base64 is hand-rolled to avoid a dependency.

use std::env;
use std::io::Write;

#[allow(dead_code)]
const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with `=` padding.
#[allow(dead_code)]
pub fn base64_encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[((n >> 18) & 0x3f) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 0x3f) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[((n >> 6) & 0x3f) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(n & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// Build the OSC 52 sequence that sets the system clipboard to `text`.
/// When running inside tmux, wrap it in tmux's passthrough so the escape
/// reaches the outer terminal.
#[allow(dead_code)]
pub fn osc52(text: &str) -> String {
    let payload = base64_encode(text.as_bytes());
    let seq = format!("\x1b]52;c;{}\x07", payload);
    if env::var_os("TMUX").is_some() {
        // tmux passthrough: wrap in DCS, and double every ESC inside the body.
        format!("\x1bPtmux;{}\x1b\\", seq.replace('\x1b', "\x1b\x1b"))
    } else {
        seq
    }
}

/// Write the clipboard escape to stdout. Best effort: errors are ignored
/// because a failed clipboard write must never crash the UI.
#[allow(dead_code)]
pub fn copy_to_clipboard(text: &str) {
    let seq = osc52(text);
    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(seq.as_bytes());
    let _ = stdout.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn osc52_wraps_payload() {
        // Without TMUX in the test env, expect the bare OSC 52 form.
        if std::env::var_os("TMUX").is_none() {
            assert_eq!(osc52("foobar"), "\x1b]52;c;Zm9vYmFy\x07");
        }
    }
}
