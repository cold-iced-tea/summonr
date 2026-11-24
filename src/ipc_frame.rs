use serde_json::Value;
use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
};
const MAX_FRAME: usize = 64 * 1024 * 1024;

pub fn write_frame(s: &mut UnixStream, v: &Value) -> std::io::Result<()> {
    let body = serde_json::to_vec(v)?;
    s.write_all(&(body.len() as u32).to_be_bytes())?;
    s.write_all(&body)?;
    s.flush()
}

pub fn read_frame(s: &mut UnixStream) -> std::io::Result<Option<Value>> {
    let mut len = [0u8; 4];
    match s.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let n = u32::from_be_bytes(len) as usize;
    if n > MAX_FRAME {
        return Err(std::io::Error::other("frame too large"));
    }
    let mut body = vec![0u8; n];
    s.read_exact(&mut body)?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(std::io::Error::other)
}
