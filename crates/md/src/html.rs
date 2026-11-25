//! Tiny HTML tag tokenizer used for inline HTML and simplifiable HTML blocks.

pub(crate) struct Tag {
    /// Lowercased tag name.
    pub name: String,
    pub closing: bool,
    pub attrs: Vec<(String, String)>,
}

impl Tag {
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

/// Parses a tag at the start of `s` (which must begin with `<`). Returns the tag and its
/// byte length. Comments, doctypes and processing instructions are not tags.
pub(crate) fn parse_tag(s: &str) -> Option<(Tag, usize)> {
    let b = s.as_bytes();
    if b.first() != Some(&b'<') {
        return None;
    }
    let mut i = 1;
    let closing = b.get(i) == Some(&b'/');
    if closing {
        i += 1;
    }
    let name_start = i;
    while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'-' || b[i] == b':') {
        i += 1;
    }
    if i == name_start || !b[name_start].is_ascii_alphabetic() {
        return None;
    }
    let name = s[name_start..i].to_ascii_lowercase();
    let mut attrs = Vec::new();
    loop {
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        match b.get(i) {
            None => return None,
            Some(b'>') => {
                i += 1;
                break;
            }
            Some(b'/') => {
                i += 1;
                continue;
            }
            _ => {}
        }
        let ks = i;
        while i < b.len() && !b[i].is_ascii_whitespace() && !matches!(b[i], b'=' | b'>' | b'/') {
            i += 1;
        }
        if i == ks {
            // stray char such as '='; skip it to guarantee progress
            i += 1;
            continue;
        }
        let key = s[ks..i].to_ascii_lowercase();
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut val = "";
        if b.get(i) == Some(&b'=') {
            i += 1;
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            match b.get(i) {
                Some(&q) if q == b'"' || q == b'\'' => {
                    let vs = i + 1;
                    let rel = memchr::memchr(q, &b[vs..])?;
                    val = &s[vs..vs + rel];
                    i = vs + rel + 1;
                }
                Some(_) => {
                    let vs = i;
                    while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'>' {
                        i += 1;
                    }
                    val = &s[vs..i];
                }
                None => return None,
            }
        }
        attrs.push((key, decode_entities(val)));
    }
    Some((Tag { name, closing, attrs }, i))
}

/// Decodes the handful of entities that matter in READMEs.
pub(crate) fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(p) = rest.find('&') {
        out.push_str(&rest[..p]);
        rest = &rest[p..];
        let end = rest.find(';').filter(|&e| e <= 10);
        let decoded = end.and_then(|e| match &rest[1..e] {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some('\u{a0}'),
            ent if ent.starts_with("#x") || ent.starts_with("#X") => u32::from_str_radix(&ent[2..], 16).ok().and_then(char::from_u32),
            ent if ent.starts_with('#') => ent[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        });
        match (decoded, end) {
            (Some(c), Some(e)) => {
                out.push(c);
                rest = &rest[e + 1..];
            }
            _ => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}
