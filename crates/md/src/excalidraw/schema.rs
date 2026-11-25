//! Tolerant Excalidraw JSON scene schema with streaming element & point caps.

use super::{ExcalidrawError, MAX_ELEMENTS, MAX_POINTS_PER_ELEMENT, MAX_TOTAL_POINTS};
use rustc_hash::FxHashMap;
use serde::de::{self, Deserializer, SeqAccess, Visitor};
use serde::Deserialize;
use std::fmt;

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct ExcalidrawScene {
    #[serde(deserialize_with = "deserialize_elements")]
    pub elements: Vec<ExcalidrawElement>,
    #[serde(rename = "appState")]
    pub app_state: ExcalidrawAppState,
    pub files: FxHashMap<String, ExcalidrawFile>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct ExcalidrawAppState {
    #[serde(rename = "viewBackgroundColor", deserialize_with = "de_opt_string")]
    pub view_background_color: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct ExcalidrawFile {
    #[serde(deserialize_with = "de_string_default")]
    pub id: String,
    #[serde(rename = "mimeType", deserialize_with = "de_string_default")]
    pub mime_type: String,
    #[serde(rename = "dataURL", deserialize_with = "de_string_default")]
    pub data_url: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Roundness {
    #[serde(rename = "type", deserialize_with = "de_u64_default")]
    pub kind: u64,
    #[serde(deserialize_with = "de_opt_f64")]
    pub value: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FontFamily {
    Numeric(u64),
    Named(String),
}

impl Default for FontFamily {
    fn default() -> Self {
        Self::Numeric(1)
    }
}

impl<'de> Deserialize<'de> for FontFamily {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct FfVisitor;
        impl<'de> Visitor<'de> for FfVisitor {
            type Value = FontFamily;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a font family id or name")
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
                Ok(FontFamily::Numeric(v))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
                Ok(FontFamily::Numeric(v.max(0) as u64))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Self::Value, E> {
                Ok(FontFamily::Numeric(if v.is_finite() && v >= 0.0 { v as u64 } else { 1 }))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(FontFamily::Named(v.to_string()))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(FontFamily::default())
            }
            fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(FontFamily::default())
            }
        }
        deserializer.deserialize_any(FfVisitor)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ExcalidrawElement {
    #[serde(deserialize_with = "de_string_default")]
    pub id: String,
    #[serde(rename = "type", deserialize_with = "de_string_default")]
    pub kind: String,
    #[serde(deserialize_with = "de_f64_zero")]
    pub x: f64,
    #[serde(deserialize_with = "de_f64_zero")]
    pub y: f64,
    #[serde(deserialize_with = "de_f64_zero")]
    pub width: f64,
    #[serde(deserialize_with = "de_f64_zero")]
    pub height: f64,
    #[serde(deserialize_with = "de_f64_zero")]
    pub angle: f64,
    #[serde(rename = "strokeColor", deserialize_with = "de_stroke_color")]
    pub stroke_color: String,
    #[serde(rename = "backgroundColor", deserialize_with = "de_bg_color")]
    pub background_color: String,
    #[serde(rename = "fillStyle", deserialize_with = "de_fill_style")]
    pub fill_style: String,
    #[serde(rename = "strokeWidth", deserialize_with = "de_f64_one")]
    pub stroke_width: f64,
    #[serde(rename = "strokeStyle", deserialize_with = "de_stroke_style")]
    pub stroke_style: String,
    #[serde(deserialize_with = "de_f64_one")]
    pub roughness: f64,
    #[serde(deserialize_with = "de_f64_100")]
    pub opacity: f64,
    pub roundness: Option<Roundness>,
    #[serde(deserialize_with = "de_seed")]
    pub seed: u64,
    #[serde(rename = "isDeleted", deserialize_with = "de_bool_false")]
    pub is_deleted: bool,
    #[serde(rename = "frameId", deserialize_with = "de_opt_string")]
    pub frame_id: Option<String>,
    #[serde(deserialize_with = "deserialize_points")]
    pub points: Vec<[f64; 2]>,
    #[serde(rename = "startArrowhead", deserialize_with = "de_opt_string")]
    pub start_arrowhead: Option<String>,
    #[serde(rename = "endArrowhead", deserialize_with = "de_opt_string")]
    pub end_arrowhead: Option<String>,
    #[serde(deserialize_with = "de_string_default")]
    pub text: String,
    #[serde(rename = "fontSize", deserialize_with = "de_f64_20")]
    pub font_size: f64,
    #[serde(rename = "fontFamily")]
    pub font_family: FontFamily,
    #[serde(rename = "textAlign", deserialize_with = "de_text_align")]
    pub text_align: String,
    #[serde(rename = "verticalAlign", deserialize_with = "de_vertical_align")]
    pub vertical_align: String,
    #[serde(rename = "lineHeight", deserialize_with = "de_f64_line_height")]
    pub line_height: f64,
    #[serde(rename = "containerId", deserialize_with = "de_opt_string")]
    pub container_id: Option<String>,
    #[serde(rename = "fileId", deserialize_with = "de_opt_string")]
    pub file_id: Option<String>,
    #[serde(deserialize_with = "de_opt_string")]
    pub name: Option<String>,
    #[serde(deserialize_with = "de_opt_string")]
    pub link: Option<String>,
}

impl Default for ExcalidrawElement {
    fn default() -> Self {
        Self {
            id: String::new(),
            kind: String::new(),
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
            angle: 0.0,
            stroke_color: "#1e1e1e".into(),
            background_color: "transparent".into(),
            fill_style: "solid".into(),
            stroke_width: 1.0,
            stroke_style: "solid".into(),
            roughness: 1.0,
            opacity: 100.0,
            roundness: None,
            seed: 1,
            is_deleted: false,
            frame_id: None,
            points: Vec::new(),
            start_arrowhead: None,
            end_arrowhead: None,
            text: String::new(),
            font_size: 20.0,
            font_family: FontFamily::default(),
            text_align: "left".into(),
            vertical_align: "top".into(),
            line_height: 1.25,
            container_id: None,
            file_id: None,
            name: None,
            link: None,
        }
    }
}

const ERR_TOO_MANY_ELEMENTS: &str = "__EXCALIDRAW_TOO_MANY_ELEMENTS__";
const ERR_TOO_MANY_POINTS: &str = "__EXCALIDRAW_TOO_MANY_POINTS__";

pub fn parse_scene_json(json: &str) -> Result<ExcalidrawScene, ExcalidrawError> {
    serde_json::from_str::<ExcalidrawScene>(json).map_err(|e| {
        let msg = e.to_string();
        if msg.contains(ERR_TOO_MANY_ELEMENTS) {
            ExcalidrawError::TooManyElements
        } else if msg.contains(ERR_TOO_MANY_POINTS) {
            ExcalidrawError::TooManyPoints
        } else {
            ExcalidrawError::InvalidJson(msg)
        }
    })
}

fn deserialize_elements<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<ExcalidrawElement>, D::Error> {
    struct ElemSeq;
    impl<'de> Visitor<'de> for ElemSeq {
        type Value = Vec<ExcalidrawElement>;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("an array of Excalidraw elements")
        }
        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(Vec::new())
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let cap = seq.size_hint().unwrap_or(0).min(MAX_ELEMENTS);
            let mut out = Vec::with_capacity(cap);
            let mut total_seen = 0usize;
            let mut total_points = 0usize;
            while let Some(el) = seq.next_element::<ExcalidrawElement>()? {
                total_seen += 1;
                if total_seen > MAX_ELEMENTS {
                    return Err(de::Error::custom(ERR_TOO_MANY_ELEMENTS));
                }
                total_points += el.points.len();
                if total_points > MAX_TOTAL_POINTS {
                    return Err(de::Error::custom(ERR_TOO_MANY_POINTS));
                }
                if !el.is_deleted {
                    out.push(el);
                }
            }
            Ok(out)
        }
    }
    deserializer.deserialize_any(ElemSeq)
}

fn deserialize_points<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<[f64; 2]>, D::Error> {
    struct PtsVisitor;
    impl<'de> Visitor<'de> for PtsVisitor {
        type Value = Vec<[f64; 2]>;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a list of [x, y] points")
        }
        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(Vec::new())
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let cap = seq.size_hint().unwrap_or(0).min(MAX_POINTS_PER_ELEMENT);
            let mut out = Vec::with_capacity(cap);
            let mut count = 0usize;
            while let Some(pt) = seq.next_element::<Option<[f64; 2]>>()? {
                count += 1;
                if count > MAX_POINTS_PER_ELEMENT {
                    return Err(de::Error::custom(ERR_TOO_MANY_POINTS));
                }
                if let Some([px, py]) = pt
                    && px.is_finite()
                    && py.is_finite()
                {
                    out.push([px, py]);
                }
            }
            Ok(out)
        }
    }
    deserializer.deserialize_any(PtsVisitor)
}

fn de_string_or_default<'de, D: Deserializer<'de>>(deserializer: D, default: &str) -> Result<String, D::Error> {
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_else(|| default.to_string()))
}
fn de_string_default<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    de_string_or_default(d, "")
}
fn de_stroke_color<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    de_string_or_default(d, "#1e1e1e")
}
fn de_bg_color<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    de_string_or_default(d, "transparent")
}
fn de_fill_style<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    de_string_or_default(d, "solid")
}
fn de_stroke_style<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    de_string_or_default(d, "solid")
}
fn de_text_align<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    de_string_or_default(d, "left")
}
fn de_vertical_align<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    de_string_or_default(d, "top")
}
fn de_opt_string<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Option::<String>::deserialize(d)
}
fn de_opt_f64<'de, D: Deserializer<'de>>(d: D) -> Result<Option<f64>, D::Error> {
    Option::<f64>::deserialize(d)
}
fn de_f64_or<'de, D: Deserializer<'de>>(d: D, default: f64) -> Result<f64, D::Error> {
    Ok(Option::<f64>::deserialize(d)?.unwrap_or(default))
}
fn de_f64_zero<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    de_f64_or(d, 0.0)
}
fn de_f64_one<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    de_f64_or(d, 1.0)
}
fn de_f64_100<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    de_f64_or(d, 100.0)
}
fn de_f64_20<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    de_f64_or(d, 20.0)
}
fn de_f64_line_height<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    de_f64_or(d, 1.25)
}
fn de_u64_default<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    Ok(Option::<u64>::deserialize(d)?.unwrap_or(0))
}
fn de_bool_false<'de, D: Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    Ok(Option::<bool>::deserialize(d)?.unwrap_or(false))
}
fn de_seed<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
    struct SeedVisitor;
    impl<'de> Visitor<'de> for SeedVisitor {
        type Value = u64;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("an integer or numeric seed")
        }
        fn visit_u64<E: de::Error>(self, v: u64) -> Result<u64, E> {
            Ok(v)
        }
        fn visit_i64<E: de::Error>(self, v: i64) -> Result<u64, E> {
            Ok(v.unsigned_abs())
        }
        fn visit_f64<E: de::Error>(self, v: f64) -> Result<u64, E> {
            Ok(if v.is_finite() { v.abs() as u64 } else { 1 })
        }
        fn visit_unit<E: de::Error>(self) -> Result<u64, E> {
            Ok(1)
        }
        fn visit_none<E: de::Error>(self) -> Result<u64, E> {
            Ok(1)
        }
    }
    deserializer.deserialize_any(SeedVisitor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_deleted_unknown_and_enforces_caps() {
        let json = r#"{
            "elements": [
                {"id": "1", "type": "rectangle", "isDeleted": true, "x": 0, "y": 0, "width": 10, "height": 10},
                {"id": "2", "type": "futureWidget", "x": 0, "y": 0, "width": 10, "height": 10},
                {"id": "3", "type": "text", "text": "ok", "fontFamily": "Virgil", "roundness": null, "seed": -42}
            ]
        }"#;
        let scene = parse_scene_json(json).unwrap();
        assert_eq!(scene.elements.len(), 2);
        assert_eq!(scene.elements[1].seed, 42);
        assert_eq!(scene.elements[1].font_family, FontFamily::Named("Virgil".into()));

        // Too many elements
        let mut many = String::from(r#"{"elements":["#);
        for i in 0..=MAX_ELEMENTS {
            if i > 0 {
                many.push(',');
            }
            many.push_str(r#"{"type":"rectangle"}"#);
        }
        many.push_str("]}");
        assert_eq!(parse_scene_json(&many).unwrap_err(), ExcalidrawError::TooManyElements);

        // Too many points in one freedraw
        let mut pts = String::from(r#"{"elements":[{"type":"freedraw","points":["#);
        for i in 0..=MAX_POINTS_PER_ELEMENT {
            if i > 0 {
                pts.push(',');
            }
            pts.push_str("[0,0]");
        }
        pts.push_str("]}]}");
        assert_eq!(parse_scene_json(&pts).unwrap_err(), ExcalidrawError::TooManyPoints);
    }
}
