//! `sight`: read an image beneath a granted directory into a metadata-only
//! handle. The bytes stay on disk; a handle names them by path and SHA-256 and
//! is re-verified whenever the bytes are actually needed.
use crate::file_reagent::{fstat_fd, open_at, open_parent_and_leaf, read_bounded_bytes, segments, validate_root};
use crate::reagent::{self, Authority, Fail, Reagent, ToolSpec};
use serde_json::{json, Value};

pub struct Sight;
pub(crate) const DEFAULT_MAX_BYTES: u64 = 10_485_760;
pub(crate) const DEFAULT_MAX_PER_STEP: usize = 8;

const TOOLS: &[ToolSpec] = &[ToolSpec {
    reagent: "sight",
    name: "read_image",
    effect: "read",
    determinism: "deterministic over live state",
    description: "Read a PNG, JPEG or WebP image beneath a granted directory into a handle; no symlinks",
}];

/// The key a handle's issue puts in `Authority::selected`. Mail ids contain no
/// `/` and roots always do, so the two kinds of key cannot collide.
pub(crate) fn issue_key(handle: &Value) -> Option<String> {
    Some(format!(
        "sight:{}:{}/{}",
        handle.get("sha256")?.as_str()?,
        handle.get("root")?.as_str()?,
        handle.get("path")?.as_str()?
    ))
}

fn unsupported() -> Fail {
    Fail::Invalid("unsupported image: only PNG, JPEG and still WebP are accepted".into())
}

/// Identify an image by its magic bytes (never its extension) and read its
/// dimensions from the header: `(mime, width, height)`.
pub(crate) fn sniff(b: &[u8]) -> Result<(&'static str, u32, u32), Fail> {
    let be32 = |i: usize| b.get(i..i + 4).map(|x| u32::from_be_bytes([x[0], x[1], x[2], x[3]]));
    let le24 = |i: usize| b.get(i..i + 3).map(|x| u32::from(x[0]) | u32::from(x[1]) << 8 | u32::from(x[2]) << 16);
    let (mime, w, h) = if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        if b.get(12..16) != Some(b"IHDR") {
            return Err(unsupported());
        }
        ("image/png", be32(16), be32(20))
    } else if b.starts_with(&[0xff, 0xd8, 0xff]) {
        let (w, h) = jpeg_dimensions(b).ok_or_else(unsupported)?;
        ("image/jpeg", Some(w), Some(h))
    } else if b.len() >= 16 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        match &b[12..16] {
            b"VP8 " => {
                if b.get(23..26) != Some(&[0x9d, 0x01, 0x2a]) {
                    return Err(unsupported());
                }
                let le14 = |i: usize| b.get(i..i + 2).map(|x| u32::from(u16::from_le_bytes([x[0], x[1]]) & 0x3fff));
                ("image/webp", le14(26), le14(28))
            }
            b"VP8L" => {
                if b.get(20) != Some(&0x2f) {
                    return Err(unsupported());
                }
                let bits = b.get(21..25).map(|x| u32::from_le_bytes([x[0], x[1], x[2], x[3]]));
                ("image/webp", bits.map(|v| (v & 0x3fff) + 1), bits.map(|v| ((v >> 14) & 0x3fff) + 1))
            }
            b"VP8X" => {
                let flags = *b.get(20).ok_or_else(unsupported)?;
                if flags & 0x02 != 0 {
                    return Err(Fail::Invalid("unsupported image: animated WebP is not accepted".into()));
                }
                ("image/webp", le24(24).map(|v| v + 1), le24(27).map(|v| v + 1))
            }
            _ => return Err(unsupported()),
        }
    } else {
        return Err(unsupported());
    };
    match (w, h) {
        (Some(w), Some(h)) if w > 0 && h > 0 => Ok((mime, w, h)),
        _ => Err(unsupported()),
    }
}

/// Walk JPEG markers to the first start-of-frame segment.
fn jpeg_dimensions(b: &[u8]) -> Option<(u32, u32)> {
    let mut i = 2;
    loop {
        while *b.get(i)? != 0xff {
            i += 1;
        }
        while *b.get(i)? == 0xff {
            i += 1;
        }
        let marker = *b.get(i)?;
        i += 1;
        if marker == 0x01 || (0xd0..=0xd8).contains(&marker) {
            continue;
        }
        if marker == 0xd9 || marker == 0xda {
            return None;
        }
        let len = usize::from(u16::from_be_bytes([*b.get(i)?, *b.get(i + 1)?]));
        if matches!(marker, 0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf) {
            let h = u32::from(u16::from_be_bytes([*b.get(i + 3)?, *b.get(i + 4)?]));
            let w = u32::from(u16::from_be_bytes([*b.get(i + 5)?, *b.get(i + 6)?]));
            return Some((w, h));
        }
        if len < 2 {
            return None;
        }
        i += len;
    }
}

/// Open and read one image file beneath `root` by descriptor walk, refusing
/// symlinks, non-regular files, and anything over `max` bytes.
fn read_image_bytes(root: &str, path: &str, max: u64, auth: Option<&Authority>) -> Result<Vec<u8>, Fail> {
    let (parent, leaf) = open_parent_and_leaf(root, path)?;
    let mut file = open_at(&parent, leaf, false)?;
    let st = fstat_fd(&file, leaf)?;
    if (st.st_mode & libc::S_IFMT) == libc::S_IFREG && st.st_size as u64 > max {
        return Err(Fail::Exhausted(format!(
            "image is {} bytes, over the {max}-byte limit",
            st.st_size
        )));
    }
    read_bounded_bytes(&mut file, max as usize, auth).map_err(|e| match e {
        Fail::Exhausted(_) => Fail::Exhausted(format!("image exceeds the {max}-byte limit")),
        e => e,
    })
}

/// Reopen the file a handle names and return its bytes, provided they are
/// still the bytes the handle was issued for.
pub(crate) fn load_verified(handle: &Value, max_bytes: u64) -> Result<Vec<u8>, Fail> {
    let field = |k: &str| {
        handle
            .get(k)
            .and_then(Value::as_str)
            .ok_or_else(|| Fail::Invalid(format!("image handle lacks `{k}`")))
    };
    let (root, path, sha) = (field("root")?, field("path")?, field("sha256")?);
    let data = match read_image_bytes(root, path, max_bytes, None) {
        // The handle was issued within the limit; a file now over it is not
        // the file that was read.
        Err(Fail::Exhausted(_)) => {
            return Err(Fail::Failed(format!(
                "stale image `{path}`: now larger than the {max_bytes}-byte limit"
            )))
        }
        Err(e) => return Err(Fail::Failed(format!("stale image `{path}`: cannot reread ({e:?})"))),
        Ok(d) => d,
    };
    if crate::util::sha256_hex(&data) != sha {
        return Err(Fail::Failed(format!("stale image `{path}`: contents changed since it was read")));
    }
    Ok(data)
}

fn root_and_path(args: &Value) -> Result<(&str, &str), Fail> {
    let text = |key: &str| {
        args[key]
            .as_str()
            .ok_or_else(|| Fail::Invalid(format!("sight tools need a string argument `{key}`")))
    };
    Ok((text("root")?, text("path")?))
}

fn select(auth: &Authority, handle: &Value) {
    if let Some(key) = issue_key(handle) {
        auth.selected.lock().unwrap().insert(key);
    }
}

impl Reagent for Sight {
    fn name(&self) -> &'static str {
        "sight"
    }
    fn tools(&self) -> &'static [ToolSpec] {
        TOOLS
    }
    fn schema(&self, name: &str) -> Option<Value> {
        (name == "read_image").then(|| {
            json!({"type":"object","properties":{
                "root":{"type":"string","minLength":1,"maxLength":4096},
                "path":{"type":"string","maxLength":4096},
                "max_bytes":{"type":"integer","minimum":1,"maximum":DEFAULT_MAX_BYTES}
            },"required":["root","path"],"additionalProperties":false})
        })
    }
    fn authorize(&self, _: Option<&str>, name: &str, args: &Value, auth: &Authority) -> Result<(), Fail> {
        let (root, path) = root_and_path(args)?;
        validate_root(root).map_err(Fail::Denied)?;
        reagent::require(auth, reagent::need("sight", name, &[("root", root)], &[]))?;
        // Legacy/broad grants do not bypass explicit directory scoping.
        if !auth.grants.iter().any(|g| {
            g.reagent == "sight" && g.action == name && g.on.get("root").is_some_and(|r| r == root)
        }) {
            return Err(Fail::Denied("sight access requires an explicit root grant".into()));
        }
        if segments(path)?.is_empty() {
            return Err(Fail::Invalid("sight.read_image needs a relative file path".into()));
        }
        Ok(())
    }
    /// A replayed or simulated handle carries the same authority as a live one.
    fn adopt(&self, name: &str, output: &Value, auth: &Authority) {
        if name == "read_image" {
            select(auth, output);
        }
    }
    fn call(&self, mail_root: Option<&str>, name: &str, args: &Value, auth: &Authority) -> Result<Value, Fail> {
        self.authorize(mail_root, name, args, auth)?;
        let (root, path) = root_and_path(args)?;
        if auth.deadline.is_some_and(|d| std::time::Instant::now() >= d) {
            return Err(Fail::Exhausted("image read deadline".into()));
        }
        let max = args["max_bytes"].as_u64().unwrap_or(DEFAULT_MAX_BYTES);
        let data = read_image_bytes(root, path, max, Some(auth))?;
        let (mime, width, height) = sniff(&data)?;
        let handle = json!({
            "kind":"image","root":root,"path":path,
            "sha256":crate::util::sha256_hex(&data),
            "mime":mime,"bytes":data.len(),"width":width,"height":height
        });
        select(auth, &handle);
        Ok(handle)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::{collections::HashSet, sync::Mutex};

    pub(crate) const PNG_1X1: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d,
        0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
        0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53, 0xde, 0x00, 0x00, 0x00,
        0x0c, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0x00,
        0x00, 0x03, 0x01, 0x01, 0x00, 0xc9, 0xfe, 0x92, 0xef, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];
    pub(crate) const JPEG_1X1: &[u8] = &[
        0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10, 0x4a, 0x46, 0x49, 0x46, 0x00, 0x01,
        0x01, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0xff, 0xdb, 0x00, 0x43,
        0x00, 0x08, 0x06, 0x06, 0x07, 0x06, 0x05, 0x08, 0x07, 0x07, 0x07, 0x09,
        0x09, 0x08, 0x0a, 0x0c, 0x14, 0x0d, 0x0c, 0x0b, 0x0b, 0x0c, 0x19, 0x12,
        0x13, 0x0f, 0x14, 0x1d, 0x1a, 0x1f, 0x1e, 0x1d, 0x1a, 0x1c, 0x1c, 0x20,
        0x24, 0x2e, 0x27, 0x20, 0x22, 0x2c, 0x23, 0x1c, 0x1c, 0x28, 0x37, 0x29,
        0x2c, 0x30, 0x31, 0x34, 0x34, 0x34, 0x1f, 0x27, 0x39, 0x3d, 0x38, 0x32,
        0x3c, 0x2e, 0x33, 0x34, 0x32, 0xff, 0xdb, 0x00, 0x43, 0x01, 0x09, 0x09,
        0x09, 0x0c, 0x0b, 0x0c, 0x18, 0x0d, 0x0d, 0x18, 0x32, 0x21, 0x1c, 0x21,
        0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32,
        0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32,
        0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32,
        0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32, 0x32,
        0x32, 0x32, 0xff, 0xc0, 0x00, 0x11, 0x08, 0x00, 0x01, 0x00, 0x01, 0x03,
        0x01, 0x22, 0x00, 0x02, 0x11, 0x01, 0x03, 0x11, 0x01, 0xff, 0xc4, 0x00,
        0x1f, 0x00, 0x00, 0x01, 0x05, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05,
        0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0xff, 0xc4, 0x00, 0xb5, 0x10, 0x00,
        0x02, 0x01, 0x03, 0x03, 0x02, 0x04, 0x03, 0x05, 0x05, 0x04, 0x04, 0x00,
        0x00, 0x01, 0x7d, 0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21,
        0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07, 0x22, 0x71, 0x14, 0x32, 0x81,
        0x91, 0xa1, 0x08, 0x23, 0x42, 0xb1, 0xc1, 0x15, 0x52, 0xd1, 0xf0, 0x24,
        0x33, 0x62, 0x72, 0x82, 0x09, 0x0a, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x25,
        0x26, 0x27, 0x28, 0x29, 0x2a, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a,
        0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56,
        0x57, 0x58, 0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a,
        0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x83, 0x84, 0x85, 0x86,
        0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99,
        0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3,
        0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6,
        0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9,
        0xda, 0xe1, 0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf1,
        0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa, 0xff, 0xc4, 0x00,
        0x1f, 0x01, 0x00, 0x03, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01,
        0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x02, 0x03, 0x04, 0x05,
        0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0xff, 0xc4, 0x00, 0xb5, 0x11, 0x00,
        0x02, 0x01, 0x02, 0x04, 0x04, 0x03, 0x04, 0x07, 0x05, 0x04, 0x04, 0x00,
        0x01, 0x02, 0x77, 0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31,
        0x06, 0x12, 0x41, 0x51, 0x07, 0x61, 0x71, 0x13, 0x22, 0x32, 0x81, 0x08,
        0x14, 0x42, 0x91, 0xa1, 0xb1, 0xc1, 0x09, 0x23, 0x33, 0x52, 0xf0, 0x15,
        0x62, 0x72, 0xd1, 0x0a, 0x16, 0x24, 0x34, 0xe1, 0x25, 0xf1, 0x17, 0x18,
        0x19, 0x1a, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x35, 0x36, 0x37, 0x38, 0x39,
        0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55,
        0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69,
        0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x82, 0x83, 0x84,
        0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97,
        0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa,
        0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4,
        0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7,
        0xd8, 0xd9, 0xda, 0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea,
        0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa, 0xff, 0xda, 0x00,
        0x0c, 0x03, 0x01, 0x00, 0x02, 0x11, 0x03, 0x11, 0x00, 0x3f, 0x00, 0xe2,
        0xe8, 0xa2, 0x8a, 0xf9, 0x93, 0xf7, 0x13, 0xff, 0xd9,
    ];
    const WEBP_1X1: &[u8] = &[
        0x52, 0x49, 0x46, 0x46, 0x3c, 0x00, 0x00, 0x00, 0x57, 0x45, 0x42, 0x50,
        0x56, 0x50, 0x38, 0x20, 0x30, 0x00, 0x00, 0x00, 0xd0, 0x01, 0x00, 0x9d,
        0x01, 0x2a, 0x01, 0x00, 0x01, 0x00, 0x01, 0x40, 0x26, 0x25, 0xa0, 0x02,
        0x74, 0xba, 0x01, 0xf8, 0x00, 0x03, 0xb0, 0x00, 0xfe, 0xf2, 0xeb, 0x7f,
        0xfc, 0xd8, 0x15, 0xcd, 0x73, 0xef, 0xf7, 0xff, 0xd2, 0xe0, 0xfd, 0x2e,
        0x0f, 0xd2, 0xe0, 0xff, 0xd2, 0x90, 0x00, 0x00,
    ];

    fn tmp() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(crate::util::new_id("summonr-sight"));
        std::fs::create_dir_all(&d).unwrap();
        d
    }
    fn grants(root: &str) -> Vec<crate::ritual::Grant> {
        vec![reagent::need("sight", "read_image", &[("root", root)], &[])]
    }
    fn auth<'a>(
        grants: &'a [crate::ritual::Grant],
        selected: &'a Mutex<HashSet<String>>,
    ) -> Authority<'a> {
        Authority {
            grants,
            selected,
            deadline: None,
            on_item: None,
            expected_digest: None,
            config: None,
        }
    }

    #[test]
    fn reads_png_jpeg_webp_into_handles() {
        let dir = tmp();
        let root = dir.to_str().unwrap();
        let g = grants(root);
        let sel = Mutex::new(HashSet::new());
        let a = auth(&g, &sel);
        for (name, mime, data) in [
            ("a.png", "image/png", PNG_1X1),
            ("b.jpg", "image/jpeg", JPEG_1X1),
            ("c.webp", "image/webp", WEBP_1X1),
        ] {
            std::fs::write(dir.join(name), data).unwrap();
            let h = reagent::call(None, "sight", "read_image", &json!({"root":root,"path":name}), &a)
                .unwrap();
            assert_eq!(h["kind"], "image");
            assert_eq!(h["root"], root);
            assert_eq!(h["path"], name);
            assert_eq!(h["mime"], mime);
            assert_eq!(h["width"], 1);
            assert_eq!(h["height"], 1);
            assert_eq!(h["bytes"], data.len());
            assert_eq!(h["sha256"], crate::util::sha256_hex(data));
            assert!(sel.lock().unwrap().contains(&issue_key(&h).unwrap()));
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn refuses_by_magic_not_extension() {
        let dir = tmp();
        let root = dir.to_str().unwrap();
        let g = grants(root);
        let sel = Mutex::new(HashSet::new());
        std::fs::write(dir.join("x.png"), b"GIF89a\x01\x00\x01\x00\x00\x00\x00;").unwrap();
        let r = reagent::call(None, "sight", "read_image", &json!({"root":root,"path":"x.png"}), &auth(&g, &sel));
        match r {
            Err(Fail::Invalid(m)) => assert!(m.contains("unsupported image"), "{m}"),
            other => panic!("{other:?}"),
        }
        assert!(sel.lock().unwrap().is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn refuses_animated_webp() {
        let mut w = b"RIFF\x16\x00\x00\x00WEBPVP8X\x0a\x00\x00\x00".to_vec();
        w.extend_from_slice(&[0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        match sniff(&w) {
            Err(Fail::Invalid(m)) => assert!(m.contains("animated"), "{m}"),
            other => panic!("{other:?}"),
        }
        w[20] = 0;
        assert_eq!(sniff(&w).unwrap(), ("image/webp", 1, 1));
    }

    #[test]
    fn refuses_oversize() {
        let dir = tmp();
        let root = dir.to_str().unwrap();
        let g = grants(root);
        let sel = Mutex::new(HashSet::new());
        let mut big = PNG_1X1.to_vec();
        big.resize(101, 0);
        std::fs::write(dir.join("big.png"), &big).unwrap();
        let r = reagent::call(None, "sight", "read_image", &json!({"root":root,"path":"big.png","max_bytes":100}), &auth(&g, &sel));
        match r {
            Err(Fail::Exhausted(m)) => assert!(m.contains("100"), "{m}"),
            other => panic!("{other:?}"),
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn refuses_symlink_dotdot_and_directory() {
        let dir = tmp();
        let root = dir.to_str().unwrap();
        let g = grants(root);
        let sel = Mutex::new(HashSet::new());
        let a = auth(&g, &sel);
        std::fs::write(dir.join("a.png"), PNG_1X1).unwrap();
        std::fs::create_dir(dir.join("sub")).unwrap();
        std::os::unix::fs::symlink("a.png", dir.join("link.png")).unwrap();
        std::os::unix::fs::symlink("sub", dir.join("dlink")).unwrap();
        for p in ["link.png", "../a.png", "sub", "dlink/a.png", "/etc/passwd"] {
            let r = reagent::call(None, "sight", "read_image", &json!({"root":root,"path":p}), &a);
            assert!(r.is_err(), "{p}");
        }
        assert!(sel.lock().unwrap().is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn refuses_without_root_grant() {
        let dir = tmp();
        let root = dir.to_str().unwrap();
        std::fs::write(dir.join("a.png"), PNG_1X1).unwrap();
        let sel = Mutex::new(HashSet::new());
        for g in [
            vec![],
            grants("/somewhere/else"),
            vec![reagent::need("sight", "read_image", &[], &[])],
        ] {
            let r = reagent::call(None, "sight", "read_image", &json!({"root":root,"path":"a.png"}), &auth(&g, &sel));
            assert!(matches!(r, Err(Fail::Denied(_))), "{r:?}");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn adopt_issues_key_without_reading() {
        let h = json!({"kind":"image","root":"/nowhere","path":"p.png","sha256":"ab","mime":"image/png","bytes":1,"width":1,"height":1});
        let sel = Mutex::new(HashSet::new());
        let g = vec![];
        Sight.adopt("read_image", &h, &auth(&g, &sel));
        assert!(sel.lock().unwrap().contains("sight:ab:/nowhere/p.png"));
    }

    #[test]
    fn load_verified_detects_change() {
        let dir = tmp();
        let root = dir.to_str().unwrap();
        let g = grants(root);
        let sel = Mutex::new(HashSet::new());
        std::fs::write(dir.join("a.png"), PNG_1X1).unwrap();
        let h = reagent::call(None, "sight", "read_image", &json!({"root":root,"path":"a.png"}), &auth(&g, &sel)).unwrap();
        assert_eq!(load_verified(&h, DEFAULT_MAX_BYTES).unwrap(), PNG_1X1);
        std::fs::write(dir.join("a.png"), JPEG_1X1).unwrap();
        match load_verified(&h, DEFAULT_MAX_BYTES) {
            Err(Fail::Failed(m)) => assert!(m.contains("stale image"), "{m}"),
            other => panic!("{other:?}"),
        }
        std::fs::remove_file(dir.join("a.png")).unwrap();
        assert!(matches!(load_verified(&h, DEFAULT_MAX_BYTES), Err(Fail::Failed(m)) if m.contains("stale image")));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn load_verified_grown_file_is_stale() {
        let dir = tmp();
        let root = dir.to_str().unwrap();
        let g = grants(root);
        let sel = Mutex::new(HashSet::new());
        std::fs::write(dir.join("a.png"), PNG_1X1).unwrap();
        let h = reagent::call(None, "sight", "read_image", &json!({"root":root,"path":"a.png"}), &auth(&g, &sel)).unwrap();
        std::fs::write(dir.join("a.png"), vec![0u8; 200]).unwrap();
        match load_verified(&h, 100) {
            Err(Fail::Failed(m)) => assert!(m.contains("stale image"), "{m}"),
            other => panic!("{other:?}"),
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
