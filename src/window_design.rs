//! Optional presentation metadata carried by the pinned ritual or rite.
use serde::{Deserialize, Deserializer, Serialize, de::Error};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WindowLayout {
    Banner,
    Sidebar,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowDesign {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<WindowLayout>,
    #[serde(
        default,
        skip_serializing_if = "String::is_empty",
        deserialize_with = "label"
    )]
    pub title: String,
    #[serde(
        default,
        skip_serializing_if = "String::is_empty",
        deserialize_with = "label"
    )]
    pub by: String,
    #[serde(
        default,
        skip_serializing_if = "String::is_empty",
        deserialize_with = "accent"
    )]
    pub accent: String,
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "masthead"
    )]
    pub masthead: Vec<String>,
}

fn printable(s: &str) -> bool {
    s.chars().count() <= 64 && !s.chars().any(char::is_control)
}
fn label<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let s = String::deserialize(d)?;
    if printable(&s) {
        Ok(s)
    } else {
        Err(D::Error::custom(
            "window labels must fit 64 printable characters",
        ))
    }
}
fn accent<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let s = String::deserialize(d)?;
    if s.len() == 7 && s.starts_with('#') && s[1..].bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(s)
    } else {
        Err(D::Error::custom("window accent must be #RRGGBB"))
    }
}
fn masthead<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    let rows = Vec::<String>::deserialize(d)?;
    if rows.len() <= 5 && rows.iter().all(|s| printable(s)) {
        Ok(rows)
    } else {
        Err(D::Error::custom(
            "window masthead accepts up to five 64-character lines",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn window_design_round_trips_and_rejects_invalid_fields() {
        let style = json!({"title":"Batch gate","by":"nightjar","layout":"sidebar","accent":"#e9A879","masthead":[" /|\\ "]});
        let design: WindowDesign = serde_json::from_value(style.clone()).unwrap();
        assert_eq!(serde_json::to_value(design).unwrap(), style);
        for invalid in [
            json!({"accent":"pink"}),
            json!({"layout":"unknown"}),
            json!({"title":"title\nline"}),
            json!({"masthead":vec!["line";6]}),
            json!({"command":"launch"}),
        ] {
            assert!(serde_json::from_value::<WindowDesign>(invalid).is_err());
        }
    }
}
