//! Reliquary: signed, content-hashed ritual packs that travel between cores
//! or offline as portable bundles. Pure: no storage, no network, no clock.

use crate::keys::{Keypair, PublicKey};
use crate::ritual::{self, Ritual, Step};
use crate::util::{canonical_json, sha256_hex};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Every pack manifest is signed over this domain prefix + canonical JSON bytes.
pub const PACK_DOMAIN: &[u8] = b"summonr-pack-v1\0";
pub const PACK_FORMAT_V1: u32 = 1;

/// One pinned ritual bundled in a pack (either the root or a transitive child).
#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct PackRitual {
    pub name: String,
    pub version: u32,
    pub hash: String,
    pub definition: Ritual,
}

/// Author-claimed assay / touchstone evidence bundled with a pack.
/// Always labeled as author-claimed and never treated as a local baseline.
#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct PackEvidence {
    pub ritual: String,
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub set_name: String,
    pub set_hash: String,
    pub fingerprint: Value,
    pub account: Value,
    pub created: i64,
    pub mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub touchstone: Option<Value>,
    #[serde(default = "default_true")]
    pub author_claimed: bool,
}

fn default_true() -> bool {
    true
}

/// Unsigned manifest describing a ritual pack and its full dependency closure.
/// Grants are intentionally absent and never travel with a pack.
#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct PackManifest {
    pub format: u32,
    pub name: String,
    pub version: u32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    pub author_key: String,
    pub created: i64,
    pub root: PackRitual,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<PackRitual>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<PackEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retracted: Option<String>,
}

/// A signed, content-hashed ritual pack.
#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct SignedPack {
    pub manifest: PackManifest,
    pub manifest_hash: String,
    pub signature: String,
}

/// Canonical JSON bytes of a `PackManifest`.
pub fn manifest_bytes(manifest: &PackManifest) -> String {
    let val = serde_json::to_value(manifest).expect("PackManifest is serializable");
    canonical_json(&val)
}

/// Sign a `PackManifest` with the author core's Ed25519 key.
pub fn sign_pack(key: &Keypair, mut manifest: PackManifest) -> SignedPack {
    manifest.format = PACK_FORMAT_V1;
    manifest.author_key = key.public().text();
    for ev in &mut manifest.evidence {
        ev.author_claimed = true;
    }
    let bytes = manifest_bytes(&manifest);
    let hash = sha256_hex(bytes.as_bytes());
    let sig_bytes = key.sign(PACK_DOMAIN, bytes.as_bytes());
    let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sig_bytes);
    SignedPack {
        manifest,
        manifest_hash: hash,
        signature,
    }
}

/// Collect every pinned `(ritual_name, version, hash)` referenced by a step list.
pub fn collect_pinned_children(steps: &[Step], out: &mut Vec<(String, Option<u32>, Option<String>)>) {
    for step in steps {
        if let Some(ref name) = step.ritual {
            out.push((name.clone(), step.version, step.hash.clone()));
        }
        if let Some(ref sub) = step.steps {
            collect_pinned_children(sub, out);
        }
        if let Some(ref sub) = step.then {
            collect_pinned_children(sub, out);
        }
        if let Some(ref sub) = step.otherwise {
            collect_pinned_children(sub, out);
        }
        if let Some(ref fb) = step.fallback {
            collect_pinned_children(fb, out);
        }
    }
}

/// Strictly verify a `SignedPack` against an explicitly trusted `PublicKey`:
/// 1. Format version and author key match the trusted key.
/// 2. Canonical manifest hash and Ed25519 signature over `PACK_DOMAIN` match.
/// 3. Root and every bundled child ritual match their declared `content_hash`.
/// 4. Every pinned child reference inside root/children resolves inside the pack.
pub fn verify_pack(pack: &SignedPack, trusted_key: &PublicKey) -> Result<(), String> {
    if pack.manifest.format != PACK_FORMAT_V1 {
        return Err(format!(
            "unsupported pack format `{}`; expected {}",
            pack.manifest.format, PACK_FORMAT_V1
        ));
    }
    if pack.manifest.author_key != trusted_key.text() {
        return Err(format!(
            "pack author key `{}` does not match trusted key `{}`",
            pack.manifest.author_key,
            trusted_key.text()
        ));
    }
    let bytes = manifest_bytes(&pack.manifest);
    let expected_hash = sha256_hex(bytes.as_bytes());
    if pack.manifest_hash != expected_hash {
        return Err(format!(
            "pack manifest hash mismatch: declared {}, computed {}",
            pack.manifest_hash, expected_hash
        ));
    }
    let raw_sig = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(&pack.signature)
        .map_err(|e| format!("invalid pack signature encoding: {e}"))?;
    let sig_arr: [u8; 64] = raw_sig
        .try_into()
        .map_err(|_| "invalid pack signature length".to_string())?;
    if !trusted_key.verify(PACK_DOMAIN, bytes.as_bytes(), &sig_arr) {
        return Err("invalid pack signature".into());
    }

    // Verify root and child content hashes.
    let check_ritual = |pr: &PackRitual, label: &str| -> Result<(), String> {
        if pr.definition.ritual != pr.name {
            return Err(format!(
                "{label} name `{}` disagrees with definition name `{}`",
                pr.name, pr.definition.ritual
            ));
        }
        let computed = ritual::content_hash(&pr.definition);
        if pr.hash != computed {
            return Err(format!(
                "{label} `{}` hash mismatch: declared {}, computed {}",
                pr.name, pr.hash, computed
            ));
        }
        Ok(())
    };
    check_ritual(&pack.manifest.root, "root ritual")?;
    let mut by_key: BTreeMap<(String, u32), &PackRitual> = BTreeMap::new();
    let mut by_name: BTreeMap<String, Vec<&PackRitual>> = BTreeMap::new();
    for ch in &pack.manifest.children {
        check_ritual(ch, "child ritual")?;
        if by_key.insert((ch.name.clone(), ch.version), ch).is_some() {
            return Err(format!(
                "duplicate child ritual `{}` version {} in pack",
                ch.name, ch.version
            ));
        }
        by_name.entry(ch.name.clone()).or_default().push(ch);
    }

    // Verify every pinned child reference in root and children is present in the closure.
    let all_rituals = std::iter::once(&pack.manifest.root).chain(pack.manifest.children.iter());
    for pr in all_rituals {
        let mut refs = Vec::new();
        collect_pinned_children(&pr.definition.steps, &mut refs);
        for (cname, cver, chash) in refs {
            let found = match cver {
                Some(v) => by_key.get(&(cname.clone(), v)).copied(),
                None => by_name
                    .get(&cname)
                    .and_then(|list| list.last().copied()),
            };
            let Some(child) = found else {
                return Err(format!(
                    "ritual `{}` references child `{cname}` which is missing from the pack closure",
                    pr.name
                ));
            };
            if let Some(expected_h) = chash {
                if child.hash != expected_h {
                    return Err(format!(
                        "ritual `{}` pins child `{cname}` hash {expected_h}, but pack bundles {}",
                        pr.name, child.hash
                    ));
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys;
    use crate::ritual;

    fn sample_ritual(name: &str, child: Option<(&str, u32, &str)>) -> Ritual {
        let yaml = match child {
            None => format!(
                "ritual: {name}\ninputs:\n  x: {{type: string}}\nessence: {{requests: 0, time: 1m}}\nsteps:\n  - id: s1\n    operation: data.trim\n    arguments: {{text: \"${{inputs.x}}\"}}\nreturn:\n  out: \"${{steps.s1.output}}\"\n"
            ),
            Some((cname, cver, chash)) => format!(
                "ritual: {name}\ninputs:\n  x: {{type: string}}\nessence: {{requests: 0, time: 1m}}\nsteps:\n  - id: c1\n    ritual: {cname}\n    version: {cver}\n    hash: {chash}\n    arguments: {{x: \"${{inputs.x}}\"}}\nreturn:\n  out: \"${{steps.c1.output.out}}\"\n"
            ),
        };
        ritual::parse_file(&yaml).unwrap()
    }

    #[test]
    fn sign_and_verify_pack_with_child_closure() {
        let key = keys::generate();
        let child_def = sample_ritual("helper", None);
        let child_hash = ritual::content_hash(&child_def);
        let root_def = sample_ritual("parent", Some(("helper", 1, &child_hash)));
        let root_hash = ritual::content_hash(&root_def);

        let manifest = PackManifest {
            format: PACK_FORMAT_V1,
            name: "parent".into(),
            version: 1,
            description: "Parent with helper".into(),
            author_key: String::new(),
            created: 1_700_000_000_000,
            root: PackRitual {
                name: "parent".into(),
                version: 1,
                hash: root_hash,
                definition: root_def,
            },
            children: vec![PackRitual {
                name: "helper".into(),
                version: 1,
                hash: child_hash,
                definition: child_def,
            }],
            evidence: vec![],
            retracted: None,
        };

        let signed = sign_pack(&key, manifest);
        assert!(verify_pack(&signed, &key.public()).is_ok());

        // Wrong key must be rejected.
        let other = keys::generate();
        assert!(verify_pack(&signed, &other.public()).is_err());

        // Tampered child hash must be rejected.
        let mut tampered = signed.clone();
        tampered.manifest.children[0].definition.description = "tampered".into();
        assert!(verify_pack(&tampered, &key.public()).is_err());
    }

    #[test]
    fn embedded_grants_field_is_rejected() {
        let key = keys::generate();
        let def = sample_ritual("leaf", None);
        let hash = ritual::content_hash(&def);
        let signed = sign_pack(
            &key,
            PackManifest {
                format: PACK_FORMAT_V1,
                name: "leaf".into(),
                version: 1,
                description: String::new(),
                author_key: String::new(),
                created: 1,
                root: PackRitual {
                    name: "leaf".into(),
                    version: 1,
                    hash,
                    definition: def,
                },
                children: vec![],
                evidence: vec![],
                retracted: None,
            },
        );
        let mut val = serde_json::to_value(&signed).unwrap();
        val["manifest"]["grants"] = serde_json::json!([{"reagent": "mail", "action": "move"}]);
        let parsed: Result<SignedPack, _> = serde_json::from_value(val);
        assert!(parsed.is_err(), "unknown `grants` field must be rejected");
    }

    #[test]
    fn missing_child_in_closure_is_rejected() {
        let key = keys::generate();
        let child_def = sample_ritual("helper", None);
        let child_hash = ritual::content_hash(&child_def);
        let root_def = sample_ritual("parent", Some(("helper", 1, &child_hash)));
        let root_hash = ritual::content_hash(&root_def);

        let signed = sign_pack(
            &key,
            PackManifest {
                format: PACK_FORMAT_V1,
                name: "parent".into(),
                version: 1,
                description: String::new(),
                author_key: String::new(),
                created: 1,
                root: PackRitual {
                    name: "parent".into(),
                    version: 1,
                    hash: root_hash,
                    definition: root_def,
                },
                children: vec![],
                evidence: vec![],
                retracted: None,
            },
        );
        let err = verify_pack(&signed, &key.public()).unwrap_err();
        assert!(err.contains("missing from the pack closure"), "got: {err}");
    }
}
