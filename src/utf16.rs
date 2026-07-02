//! UTF-16 code-unit → UTF-8 byte-offset mapping.
//!
//! Telegram's `MessageEntity.offset`/`length` are in UTF-16 code units
//! (<https://core.telegram.org/bots/api#messageentity>), but Rust strings are
//! UTF-8. teloxide converted internally (`parse_entities()`); frankenstein
//! hands the raw offsets through, so this maps them safely: malformed offsets
//! (splitting a surrogate pair, past the end) return `None` instead of
//! panicking on a non-boundary slice — a panic here would cross the FFI
//! boundary and abort the host.

use std::ops::Range;

pub(crate) struct Utf16Index {
    /// `map[u]` = UTF-8 byte offset of UTF-16 boundary `u`; `None` marks the
    /// low-surrogate slot inside a supplementary-plane character.
    map: Vec<Option<usize>>,
}

impl Utf16Index {
    /// One O(n) pass; build once per message, then O(1) per entity.
    pub(crate) fn new(text: &str) -> Self {
        let mut map = Vec::with_capacity(text.len() + 1);
        for (byte_idx, ch) in text.char_indices() {
            map.push(Some(byte_idx));
            if ch.len_utf16() == 2 {
                map.push(None);
            }
        }
        map.push(Some(text.len()));
        Self { map }
    }

    /// Byte range for a UTF-16 (offset, length) pair, or `None` if either
    /// boundary is out of range or would split a code point.
    pub(crate) fn byte_range(&self, offset: u16, length: u16) -> Option<Range<usize>> {
        let start = (*self.map.get(usize::from(offset))?)?;
        let end = (*self.map.get(usize::from(offset) + usize::from(length))?)?;
        Some(start..end)
    }
}

#[cfg(test)]
#[path = "../tests/unit/utf16_tests.rs"]
mod utf16_tests;
