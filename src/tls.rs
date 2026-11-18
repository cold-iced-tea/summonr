//! Built-in TLS server configuration and stream wrapper (via `rustls`) for
//! `summonr listen` and `summonr mcp http`.
use base64::Engine;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs1KeyDer, PrivatePkcs8KeyDer, PrivateSec1KeyDer};
use rustls::{ServerConfig, ServerConnection, StreamOwned};
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::Duration;

pub enum NetStream {
    Plain(TcpStream),
    Tls(Box<StreamOwned<ServerConnection, TcpStream>>),
}

impl NetStream {
    pub fn wrap(stream: TcpStream, tls: Option<&Arc<ServerConfig>>) -> io::Result<Self> {
        match tls {
            Some(cfg) => {
                let conn = ServerConnection::new(cfg.clone())
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
                Ok(Self::Tls(Box::new(StreamOwned::new(conn, stream))))
            }
            None => Ok(Self::Plain(stream)),
        }
    }

    pub fn set_read_timeout(&self, dur: Option<Duration>) -> io::Result<()> {
        match self {
            Self::Plain(s) => s.set_read_timeout(dur),
            Self::Tls(s) => s.sock.set_read_timeout(dur),
        }
    }

    pub fn set_write_timeout(&self, dur: Option<Duration>) -> io::Result<()> {
        match self {
            Self::Plain(s) => s.set_write_timeout(dur),
            Self::Tls(s) => s.sock.set_write_timeout(dur),
        }
    }

    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        match self {
            Self::Plain(s) => s.peer_addr(),
            Self::Tls(s) => s.sock.peer_addr(),
        }
    }
}

impl Read for NetStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Plain(s) => s.read(buf),
            Self::Tls(s) => s.read(buf),
        }
    }
}

impl Write for NetStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Plain(s) => s.write(buf),
            Self::Tls(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Plain(s) => s.flush(),
            Self::Tls(s) => s.flush(),
        }
    }
}

fn pem_blocks(pem: &str) -> Result<Vec<(String, Vec<u8>)>, String> {
    let mut blocks = Vec::new();
    let mut current_label: Option<String> = None;
    let mut b64 = String::new();
    for line in pem.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("-----BEGIN ").and_then(|s| s.strip_suffix("-----")) {
            current_label = Some(rest.to_string());
            b64.clear();
        } else if let Some(label) = &current_label {
            if let Some(end_label) = t.strip_prefix("-----END ").and_then(|s| s.strip_suffix("-----")) {
                if end_label != label {
                    return Err(format!("mismatched PEM end label `{end_label}`"));
                }
                let der = base64::engine::general_purpose::STANDARD
                    .decode(b64.as_bytes())
                    .map_err(|e| format!("invalid base64 in PEM block `{label}`: {e}"))?;
                blocks.push((label.clone(), der));
                current_label = None;
                b64.clear();
            } else {
                b64.push_str(t);
            }
        }
    }
    if current_label.is_some() {
        return Err("unterminated PEM block".into());
    }
    Ok(blocks)
}

pub fn load_server_config(cert_path: &str, key_path: &str) -> Result<Arc<ServerConfig>, String> {
    let cert_pem = std::fs::read_to_string(cert_path)
        .map_err(|e| format!("cannot read TLS certificate {cert_path}: {e}"))?;
    let key_pem = std::fs::read_to_string(key_path)
        .map_err(|e| format!("cannot read TLS private key {key_path}: {e}"))?;
    let certs: Vec<CertificateDer<'static>> = pem_blocks(&cert_pem)?
        .into_iter()
        .filter(|(label, _)| label == "CERTIFICATE")
        .map(|(_, der)| CertificateDer::from(der))
        .collect();
    if certs.is_empty() {
        return Err(format!("no CERTIFICATE block found in {cert_path}"));
    }
    let key = pem_blocks(&key_pem)?
        .into_iter()
        .find_map(|(label, der)| match label.as_str() {
            "PRIVATE KEY" => Some(PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(der))),
            "RSA PRIVATE KEY" => Some(PrivateKeyDer::Pkcs1(PrivatePkcs1KeyDer::from(der))),
            "EC PRIVATE KEY" => Some(PrivateKeyDer::Sec1(PrivateSec1KeyDer::from(der))),
            _ => None,
        })
        .ok_or_else(|| format!("no private key block found in {key_path}"))?;
    let cfg = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| format!("invalid TLS certificate/key: {e}"))?;
    Ok(Arc::new(cfg))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pem_parser_extracts_blocks_and_rejects_unterminated() {
        let sample = "-----BEGIN CERTIFICATE-----\nAQID\n-----END CERTIFICATE-----\n";
        let blocks = pem_blocks(sample).unwrap();
        assert_eq!(blocks, vec![("CERTIFICATE".into(), vec![1, 2, 3])]);
        assert!(pem_blocks("-----BEGIN PRIVATE KEY-----\nAQID\n").is_err());
    }
}
