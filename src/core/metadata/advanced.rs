//! Built-in metadata extractors (image EXIF, audio tags/properties) and the
//! "Key: Value" parser shared with external providers.

use lofty::{file::AudioFile, prelude::*};
use std::{fs, io::BufReader, path::Path};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MetaItem {
    pub key: String,
    pub value: String,
}

#[derive(Clone, Debug)]
pub struct MetaGroup {
    pub title: String,
    pub items: Vec<MetaItem>,
}

const MAX_ITEMS: usize = 80;
const MAX_VALUE: usize = 400;

fn item(k: &str, v: impl Into<String>) -> MetaItem {
    MetaItem { key: k.to_string(), value: v.into() }
}

/// Parses `Key: Value` or `key=value` lines. Blank, over-long or key-less
/// lines are skipped; output is capped.
pub fn parse_kv(out: &str) -> Vec<MetaItem> {
    let mut v = Vec::new();
    for line in out.lines() {
        let Some((k, val)) = line.split_once(": ").or_else(|| line.split_once('=')).or_else(|| line.split_once(':'))
        else {
            continue;
        };
        let (k, val) = (k.trim(), val.trim());
        if k.is_empty() || val.is_empty() || k.len() > 60 {
            continue;
        }
        let key = {
            let k = k.replace('_', " ");
            let mut c = k.chars();
            c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
        };
        v.push(MetaItem { key, value: val.chars().take(MAX_VALUE).collect() });
        if v.len() >= MAX_ITEMS {
            break;
        }
    }
    v
}

fn dms_to_deg(v: &exif::Value) -> Option<f64> {
    match v {
        exif::Value::Rational(r) if r.len() >= 3 => Some(r[0].to_f64() + r[1].to_f64() / 60.0 + r[2].to_f64() / 3600.0),
        _ => None,
    }
}

pub fn image_meta(path: &Path) -> Vec<MetaItem> {
    let Ok(f) = fs::File::open(path) else { return vec![] };
    let Ok(ex) = exif::Reader::new().read_from_container(&mut BufReader::new(f)) else { return vec![] };
    let get = |t: exif::Tag| {
        ex.get_field(t, exif::In::PRIMARY)
            .map(|f| f.display_value().with_unit(&ex).to_string().trim_matches('"').trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let mut out = Vec::new();
    for (label, tag) in [
        ("Camera make", exif::Tag::Make),
        ("Camera model", exif::Tag::Model),
        ("Lens", exif::Tag::LensModel),
        ("Taken", exif::Tag::DateTimeOriginal),
        ("Exposure", exif::Tag::ExposureTime),
        ("Aperture", exif::Tag::FNumber),
        ("ISO", exif::Tag::PhotographicSensitivity),
        ("Focal length", exif::Tag::FocalLength),
        ("Software", exif::Tag::Software),
    ] {
        if let Some(v) = get(tag) {
            out.push(item(label, v));
        }
    }
    let coord = |t: exif::Tag, r: exif::Tag, neg: &str| -> Option<f64> {
        let deg = dms_to_deg(&ex.get_field(t, exif::In::PRIMARY)?.value)?;
        let south_west = get(r).is_some_and(|s| s.starts_with(neg));
        Some(if south_west { -deg } else { deg })
    };
    if let (Some(lat), Some(lon)) = (
        coord(exif::Tag::GPSLatitude, exif::Tag::GPSLatitudeRef, "S"),
        coord(exif::Tag::GPSLongitude, exif::Tag::GPSLongitudeRef, "W"),
    ) {
        out.push(item("GPS", format!("{lat:.5}, {lon:.5}")));
    }
    out
}

fn fmt_duration(d: std::time::Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

pub fn audio_meta(path: &Path) -> Vec<MetaItem> {
    let Ok(tf) = lofty::read_from_path(path) else { return vec![] };
    let mut out = Vec::new();
    if let Some(t) = tf.primary_tag().or_else(|| tf.first_tag()) {
        for (label, v) in [
            ("Title", t.title().map(|s| s.to_string())),
            ("Artist", t.artist().map(|s| s.to_string())),
            ("Album", t.album().map(|s| s.to_string())),
            ("Genre", t.genre().map(|s| s.to_string())),
            ("Year", t.date().map(|d| d.year.to_string())),
            ("Track", t.track().map(|n| n.to_string())),
        ] {
            if let Some(v) = v.filter(|s| !s.trim().is_empty()) {
                out.push(item(label, v));
            }
        }
    }
    let p = tf.properties();
    if !p.duration().is_zero() {
        out.push(item("Duration", fmt_duration(p.duration())));
    }
    if let Some(b) = p.audio_bitrate() {
        out.push(item("Bitrate", format!("{b} kbps")));
    }
    if let Some(r) = p.sample_rate() {
        out.push(item("Sample rate", format!("{r} Hz")));
    }
    if let Some(c) = p.channels() {
        out.push(item("Channels", c.to_string()));
    }
    out
}

/// Extractors that need no external program, chosen by content type.
pub fn extract_builtin(path: &Path, mime: &str) -> Vec<MetaGroup> {
    let mut g = Vec::new();
    if mime.starts_with("image/") {
        let items = image_meta(path);
        if !items.is_empty() {
            g.push(MetaGroup { title: "Photo".into(), items });
        }
    } else if mime.starts_with("audio/") {
        let items = audio_meta(path);
        if !items.is_empty() {
            g.push(MetaGroup { title: "Audio".into(), items });
        }
    }
    g
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/test-tmp").join(name);
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn kv_parsing() {
        let v = parse_kv("Title: My Doc\nPages=12\nbit_rate: 128000\n\nnonsense line\nEmpty:\n: nokey\nTime: 10:30");
        let pairs: Vec<_> = v.iter().map(|i| (i.key.as_str(), i.value.as_str())).collect();
        assert_eq!(pairs, [("Title", "My Doc"), ("Pages", "12"), ("Bit rate", "128000"), ("Time", "10:30")]);
    }

    #[test]
    fn exif_make_from_handcrafted_jpeg() {
        let d = tmp("exif");
        let mut j: Vec<u8> = vec![0xFF, 0xD8, 0xFF, 0xE1, 0x00, 0x22];
        j.extend_from_slice(b"Exif\0\0");
        j.extend_from_slice(&[
            0x49, 0x49, 0x2A, 0, 8, 0, 0, 0, 1, 0, 0x0F, 0x01, 2, 0, 4, 0, 0, 0, b'a', b'b', b'c', 0, 0, 0, 0, 0,
        ]);
        j.extend_from_slice(&[0xFF, 0xD9]);
        fs::write(d.join("t.jpg"), j).unwrap();
        let g = extract_builtin(&d.join("t.jpg"), "image/jpeg");
        assert_eq!(g[0].items[0], item("Camera make", "abc"));
        assert!(extract_builtin(&d.join("t.jpg"), "text/plain").is_empty());
    }

    #[test]
    fn wav_properties() {
        let d = tmp("wav");
        let data = vec![0u8; 16000];
        let mut w = Vec::new();
        w.extend_from_slice(b"RIFF");
        w.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        w.extend_from_slice(b"WAVEfmt ");
        w.extend_from_slice(&16u32.to_le_bytes());
        w.extend_from_slice(&[1, 0, 1, 0]); // PCM, mono
        w.extend_from_slice(&8000u32.to_le_bytes());
        w.extend_from_slice(&16000u32.to_le_bytes());
        w.extend_from_slice(&[2, 0, 16, 0]);
        w.extend_from_slice(b"data");
        w.extend_from_slice(&(data.len() as u32).to_le_bytes());
        w.extend_from_slice(&data);
        fs::write(d.join("s.wav"), w).unwrap();
        let items = audio_meta(&d.join("s.wav"));
        let find = |k: &str| items.iter().find(|i| i.key == k).map(|i| i.value.clone());
        assert_eq!(find("Duration").as_deref(), Some("0:01"));
        assert_eq!(find("Sample rate").as_deref(), Some("8000 Hz"));
        assert_eq!(find("Channels").as_deref(), Some("1"));
    }
}
