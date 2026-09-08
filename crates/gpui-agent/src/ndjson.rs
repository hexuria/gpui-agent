use std::io::{BufRead, Read, Write};

use serde::Serialize;

/// Serialize `value` as one NDJSON line into `buf`, then write it.
///
/// Reuses `buf` so a long-lived connection does not allocate a fresh
/// `String` on every response.
pub fn write_json_line<W: Write, T: Serialize>(
    writer: &mut W,
    buf: &mut Vec<u8>,
    value: &T,
) -> std::io::Result<()> {
    buf.clear();
    serde_json::to_writer(&mut *buf, value)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
    buf.push(b'\n');
    writer.write_all(buf)
}

/// Read one NDJSON line into `buf`, capped at `max_bytes` (excluding the newline).
///
/// Returns `Ok(false)` on EOF. Oversized / non-UTF-8 lines are
/// `ErrorKind::InvalidData`. On success `buf` holds the line without `\n`/`\r`.
pub fn read_limited_line_into<R: BufRead>(
    reader: &mut R,
    buf: &mut Vec<u8>,
    max_bytes: usize,
) -> std::io::Result<bool> {
    buf.clear();
    let n = reader
        .by_ref()
        .take(max_bytes as u64 + 1)
        .read_until(b'\n', buf)?;
    if n == 0 {
        return Ok(false);
    }
    if buf.len() > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "line too long",
        ));
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }
    }
    if std::str::from_utf8(buf).is_err() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "line is not valid UTF-8",
        ));
    }
    Ok(true)
}

/// Read one NDJSON line, capped at `max_bytes` (excluding the newline).
///
/// Returns `Ok(None)` on EOF. Oversized lines are `ErrorKind::InvalidData`
/// and consume at most `max_bytes + 1` from the reader so the caller can
/// close rather than resync.
pub fn read_limited_line<R: BufRead>(
    reader: &mut R,
    max_bytes: usize,
) -> std::io::Result<Option<String>> {
    let mut buf = Vec::new();
    if !read_limited_line_into(reader, &mut buf, max_bytes)? {
        return Ok(None);
    }
    // UTF-8 already checked.
    Ok(Some(String::from_utf8(buf).expect("utf-8 checked")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_json_line_reuses_buffer() {
        let mut buf = Vec::with_capacity(64);
        let mut out = Vec::new();
        write_json_line(&mut out, &mut buf, &serde_json::json!({"ok":true})).unwrap();
        assert_eq!(out, b"{\"ok\":true}\n");
        let cap = buf.capacity();
        out.clear();
        write_json_line(&mut out, &mut buf, &serde_json::json!({"n":1})).unwrap();
        assert_eq!(out, b"{\"n\":1}\n");
        assert!(buf.capacity() >= cap);
    }
}
