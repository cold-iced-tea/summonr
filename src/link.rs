//! How a client reaches a core: unsigned on the owner's socket (as today), or
//! signed as a familiar, over the local socket or HTTP to a pinned peer.
//! Only this module and `keys` ever touch an agent's private key.

use crate::envelope::{new_nonce, open_reply, seal_request};
use crate::keys::{Keypair, PublicKey};
use crate::rpc::{Client, RpcErr};
use crate::util::{now_ms, Paths};
use serde_json::{json, Value};
use std::sync::Arc;

/// One way to make core calls.
pub trait CoreLink: Send {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, RpcErr>;
}

impl CoreLink for Client {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, RpcErr> {
        Client::call(self, method, params)
    }
}

/// Makes a fresh link per use (each MCP worker gets its own).
pub type LinkFactory = Arc<dyn Fn() -> Result<Box<dyn CoreLink>, RpcErr> + Send + Sync>;

/// Sends a signed request frame and returns the core's reply frame.
pub type Sender = Box<dyn FnMut(&Value) -> Result<Value, RpcErr> + Send>;

/// Calls as a familiar: every request signed, every reply verified against
/// the pinned core key and bound to its request's nonce.
pub struct SignedLink {
    key: Keypair,
    core: PublicKey,
    send: Sender,
}

impl SignedLink {
    pub fn new(key: Keypair, core: PublicKey, send: Sender) -> SignedLink {
        SignedLink { key, core, send }
    }
}

impl CoreLink for SignedLink {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, RpcErr> {
        let nonce = new_nonce();
        let frame = seal_request(&self.key, &self.core.id(), method, &params, now_ms(), &nonce);
        let reply = (self.send)(&frame)?;
        let sealed = json!({"signed": reply.get("signed").cloned().unwrap_or(Value::Null)});
        match open_reply(&sealed, &self.core, &nonce) {
            Ok(outcome) => outcome,
            Err(why) => {
                eprintln!("summonr: rejected a reply: {why}");
                Err(RpcErr::new("not_authenticated", 4, "reply not authenticated"))
            }
        }
    }
}

/// Send a signed frame over this device's core socket.
pub fn socket_send(paths: &Paths, frame: &Value) -> Result<Value, RpcErr> {
    Client::connect(paths)?.send_frame(json!({"signed": frame["signed"]}))
}

/// POST a signed frame to a peer core's `listen` endpoint.
pub fn http_send(url: &str, frame: &Value) -> Result<Value, RpcErr> {
    let endpoint = format!("{url}/summonr/v1/signed");
    let unavailable = |e: String| RpcErr::new("backend_unavailable", 8, format!("cannot reach {endpoint}: {e}"));
    let resp = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(330))
        .build()
        .post(&endpoint)
        .set("Content-Type", "application/json")
        .send_string(&frame.to_string());
    let body = match resp {
        Ok(r) => r.into_string().map_err(|e| unavailable(e.to_string()))?,
        Err(ureq::Error::Status(code, _)) => return Err(unavailable(format!("HTTP {code}"))),
        Err(e) => return Err(unavailable(e.to_string())),
    };
    serde_json::from_str(&body).map_err(|e| unavailable(format!("not JSON: {e}")))
}

/// The link for `summonr mcp` and other clients: unsigned without a name;
/// otherwise signed with `config/familiars/NAME.key`, to this device's core
/// or to the pinned `peer`.
pub fn link_for(paths: &Paths, name: Option<&str>, peer: Option<&str>) -> Result<Box<dyn CoreLink>, String> {
    if peer.is_none() && !paths.socket().exists() && !paths.db().exists() {
        return Err("no core is running and no state database exists".into());
    }
    let Some(name) = name else {
        if peer.is_some() {
            return Err("a peer needs a familiar: give `as NAME` too".into());
        }
        return Client::connect(paths).map(|c| Box::new(c) as Box<dyn CoreLink>).map_err(|e| e.message);
    };
    let key_file = paths.config.join("familiars").join(format!("{name}.key"));
    let key = crate::keys::load(&key_file)?;
    // Pins come from this device's own core, over the owner's socket.
    let mut owner = Client::connect(paths).map_err(|e| e.message)?;
    match peer {
        None => {
            let me = owner.call("identity", json!({})).map_err(|e| e.message)?;
            let core = PublicKey::parse(me["key"].as_str().unwrap_or(""))?;
            let paths = paths.clone();
            Ok(Box::new(SignedLink::new(key, core, Box::new(move |f: &Value| socket_send(&paths, f)))))
        }
        Some(peer) => {
            let peers = owner.call("peer_list", json!({})).map_err(|e| e.message)?;
            let pinned = peers
                .as_array()
                .into_iter()
                .flatten()
                .find(|p| p["name"] == peer)
                .ok_or_else(|| format!("no pinned peer `{peer}`; add it with `summonr peer add {peer} url URL key K`"))?;
            let core = PublicKey::parse(pinned["key"].as_str().unwrap_or(""))?;
            let url = pinned["url"].as_str().unwrap_or("").to_string();
            Ok(Box::new(SignedLink::new(key, core, Box::new(move |f: &Value| http_send(&url, f)))))
        }
    }
}

/// A factory over `link_for`, checked once up front so a bad name or key file
/// fails at startup rather than on the first tool call.
pub fn factory(paths: &Paths, name: Option<&str>, peer: Option<&str>) -> Result<LinkFactory, String> {
    if name.is_some() {
        link_for(paths, name, peer)?;
    }
    let (paths, name, peer) = (paths.clone(), name.map(String::from), peer.map(String::from));
    Ok(Arc::new(move || {
        link_for(&paths, name.as_deref(), peer.as_deref()).map_err(|e| RpcErr::new("backend_unavailable", 8, e))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use crate::core::Core;
    use crate::util::{new_id, Paths};
    use serde_json::json;
    use std::time::Duration;

    /// A core serving a temporary home on its socket, with `total-receipt`
    /// inscribed and familiar `alpha` allowed to call it.
    fn serving() -> (Paths, Arc<Core>) {
        let root = std::env::temp_dir().join(new_id("summonr-link"));
        let p = Paths { runtime: root.join("run"), state: root.join("state"), config: root.join("config") };
        let core = Core::open(p.clone()).unwrap();
        let ritual = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rituals/total-receipt.yaml");
        core.dispatch("inscribe", json!({"path": ritual.to_str().unwrap()})).unwrap();
        core.dispatch("familiar_new", json!({"name": "alpha"})).unwrap();
        core.dispatch("familiar_allow", json!({"name": "alpha", "rituals": ["total-receipt"]})).unwrap();
        let serving = core.clone();
        std::thread::spawn(move || serving.serve());
        for _ in 0..200 {
            if std::os::unix::net::UnixStream::connect(p.socket()).is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        (p, core)
    }

    fn done(p: &Paths, core: &Core) {
        core.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = std::fs::remove_dir_all(p.state.parent().unwrap());
    }

    #[test]
    fn signed_link_round_trip_over_socket() {
        let (p, core) = serving();
        let mut link = link_for(&p, Some("alpha"), None).unwrap();
        let listed = link.call("rituals", json!({})).unwrap();
        assert_eq!(listed.as_array().unwrap().len(), 1, "{listed}");
        let started = link
            .call("invoke", json!({"target": "total-receipt", "args": {"text": "{\"amounts_minor\":[100,25]}"}}))
            .unwrap();
        let id = started["summon"].as_str().unwrap();
        assert_eq!(core.owner_of(id), Some(Some("familiar:alpha".to_string())));
        let ended = link.call("wait", json!({"summon": id, "timeout_ms": 10000})).unwrap();
        assert_eq!(ended["result"]["total_minor"], 125, "{ended}");
        // Errors come back signed too, and intact.
        let e = link.call("grant", json!({})).unwrap_err();
        assert_eq!(e.code, "not_authorized");
        done(&p, &core);
    }

    #[test]
    fn tampered_reply_rejected() {
        let (p, core) = serving();
        let key = crate::keys::load(&p.config.join("familiars/alpha.key")).unwrap();
        let paths = p.clone();
        let mut link = SignedLink::new(
            key,
            core.key.public(),
            Box::new(move |frame: &Value| {
                let mut reply = socket_send(&paths, frame)?;
                let b64 = base64::engine::general_purpose::STANDARD;
                let mut bytes = b64.decode(reply["signed"]["payload"].as_str().unwrap()).unwrap();
                let last = bytes.len() - 2;
                bytes[last] ^= 1;
                reply["signed"]["payload"] = json!(b64.encode(bytes));
                Ok(reply)
            }),
        );
        let e = link.call("rituals", json!({})).unwrap_err();
        assert_eq!((e.code.as_str(), e.message.as_str()), ("not_authenticated", "reply not authenticated"));
        done(&p, &core);
    }

    #[test]
    fn reply_from_other_core_rejected() {
        let (p, core) = serving();
        let key = crate::keys::load(&p.config.join("familiars/alpha.key")).unwrap();
        let paths = p.clone();
        // Pinned to a key that is not this core's.
        let mut link = SignedLink::new(key, crate::keys::generate().public(), Box::new(move |f: &Value| socket_send(&paths, f)));
        let e = link.call("rituals", json!({})).unwrap_err();
        assert_eq!(e.code, "not_authenticated");
        done(&p, &core);
    }

    #[test]
    fn missing_key_file_is_clear_error() {
        let (p, core) = serving();
        let e = link_for(&p, Some("nobody"), None).err().unwrap();
        assert!(e.contains("familiars/nobody.key"), "{e}");
        let e = link_for(&p, Some("alpha"), Some("nowhere")).err().unwrap();
        assert!(e.contains("nowhere"), "{e}");
        done(&p, &core);
    }

    #[test]
    fn unsigned_link_is_todays_client() {
        let (p, core) = serving();
        let mut link = link_for(&p, None, None).unwrap();
        assert!(link.call("identity", json!({})).is_ok(), "the owner's socket is unchanged");
        done(&p, &core);
    }
}
