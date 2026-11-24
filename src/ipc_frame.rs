use serde_json::Value;
use std::io::{Read, Write};
const MAX_FRAME: usize = 64 * 1024 * 1024;

pub fn write_frame(s: &mut impl Write, v: &Value) -> std::io::Result<()> {
    let body = serde_json::to_vec(v)?;
    if body.len() > MAX_FRAME {
        return Err(std::io::Error::other("frame too large"));
    }
    s.write_all(&(body.len() as u32).to_be_bytes())?;
    s.write_all(&body)?;
    s.flush()
}

pub fn read_frame(s: &mut impl Read) -> std::io::Result<Option<Value>> {
    let mut len = [0u8; 4];
    loop {
        match s.read(&mut len[..1]) {
            Ok(0) => return Ok(None),
            Ok(_) => break,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    s.read_exact(&mut len[1..])?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::ErrorKind;

    #[test]
    fn oversized_frame_is_rejected_before_writing_header() {
        let mut stream = Vec::new();
        let value = Value::String("a".repeat(MAX_FRAME));
        assert!(write_frame(&mut stream, &value).is_err());
        assert!(stream.is_empty());
    }

    #[test]
    fn partial_header_is_not_clean_eof() {
        let mut reader = &[0, 0][..];
        assert_eq!(
            read_frame(&mut reader).unwrap_err().kind(),
            ErrorKind::UnexpectedEof
        );
    }
}
