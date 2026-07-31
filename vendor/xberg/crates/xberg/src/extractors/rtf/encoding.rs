//! Character encoding utilities for RTF parsing.
//!
//! Provides hex byte parsing and legacy Windows codepage decoding for RTF byte escapes.

pub(crate) use crate::text::windows_codepage::encoding_for_windows_codepage;

/// Convert a hex digit character to its numeric value.
///
/// Returns None if the character is not a valid hex digit.
#[inline]
pub(crate) fn hex_digit_to_u8(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// Parse a hex-encoded byte from two bytes.
///
/// Returns the decoded byte if both bytes are valid hex digits.
#[inline]
pub(crate) fn parse_hex_byte(h1: u8, h2: u8) -> Option<u8> {
    let high = hex_digit_to_u8(h1)?;
    let low = hex_digit_to_u8(h2)?;
    Some((high << 4) | low)
}

/// Decode RTF hex escape bytes using the active ANSI codepage.
#[inline]
pub(crate) fn decode_ansi_bytes(bytes: &[u8], codepage: u32) -> String {
    let (decoded, _, _) = encoding_for_windows_codepage(codepage).decode(bytes);
    decoded.into_owned()
}

/// Map an RTF `\fcharsetN` value to its corresponding Windows codepage number.
///
/// `\fcharset` values are a distinct enumeration from Windows codepage numbers
/// defined by the RTF 1.9.1 specification's font-charset table — they must not
/// be confused with (or fed directly to) codepage-numbered APIs.
///
/// `\fcharset1` (Default) and `\fcharset2` (Symbol) have no fixed codepage and
/// return `None`, letting the caller fall back to `\ansicpg` or 1252.
#[inline]
pub(crate) fn fcharset_to_codepage(fcharset: u8) -> Option<u32> {
    match fcharset {
        0 => Some(1252),
        77 => Some(10000),
        78 => Some(10001),
        79 => Some(10003),
        80 => Some(10008),
        81 => Some(10002),
        83 => Some(10005),
        84 => Some(10004),
        85 => Some(10006),
        86 => Some(10081),
        87 => Some(10021),
        88 => Some(10029),
        89 => Some(10007),
        128 => Some(932),
        129 => Some(949),
        130 => Some(1361),
        134 => Some(936),
        136 => Some(950),
        161 => Some(1253),
        162 => Some(1254),
        163 => Some(1258),
        177 => Some(1255),
        178 => Some(1256),
        186 => Some(1257),
        204 => Some(1251),
        222 => Some(874),
        238 => Some(1250),
        254 => Some(437),
        255 => Some(850),
        _ => None,
    }
}

/// Parse an RTF control word and extract its value.
///
/// Returns a tuple of (control_word, optional_numeric_value).
pub(crate) fn parse_rtf_control_word(chars: &mut std::iter::Peekable<std::str::Chars>) -> (String, Option<i32>) {
    let mut word = String::new();
    let mut num_str = String::new();
    let mut is_negative = false;

    while let Some(&c) = chars.peek() {
        if c.is_alphabetic() {
            word.push(c);
            chars.next();
        } else {
            break;
        }
    }

    if let Some(&c) = chars.peek()
        && c == '-'
    {
        is_negative = true;
        chars.next();
    }

    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() {
            num_str.push(c);
            chars.next();
        } else {
            break;
        }
    }

    let num_value = if !num_str.is_empty() {
        let val = num_str.parse::<i32>().unwrap_or(0);
        Some(if is_negative { -val } else { val })
    } else {
        None
    };

    if let Some(&' ') = chars.peek() {
        chars.next();
    }

    (word, num_value)
}
