//! Chrome / Edge native messaging framing: u32 length (native byte order) + UTF-8 JSON.
use std::io::{self, Read, Write};

/// Chrome limits host→browser messages to 1 MB; we accept up to 4 MB from the browser.
pub const MAX_IN: u32 = 4 << 20;

pub fn read_message<R: Read>(r: &mut R) -> io::Result<Option<serde_json::Value>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let n = u32::from_ne_bytes(len);
    if n > MAX_IN {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("message too large ({n} bytes)")));
    }
    let mut buf = vec![0u8; n as usize];
    r.read_exact(&mut buf)?;
    serde_json::from_slice(&buf).map(Some).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

pub fn write_message<W: Write>(w: &mut W, v: &serde_json::Value) -> io::Result<()> {
    let body = serde_json::to_vec(v).unwrap();
    if body.len() > 1 << 20 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "reply too large"));
    }
    w.write_all(&(body.len() as u32).to_ne_bytes())?;
    w.write_all(&body)?;
    w.flush()
}
