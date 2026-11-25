//! Pure-Rust Pieroxy LZ-String `decompressFromBase64` with streaming bomb protection.

use super::ExcalidrawError;

#[inline]
fn base64_val(b: u8) -> Result<u32, ExcalidrawError> {
    match b {
        b'A'..=b'Z' => Ok((b - b'A') as u32),
        b'a'..=b'z' => Ok((b - b'a' + 26) as u32),
        b'0'..=b'9' => Ok((b - b'0' + 52) as u32),
        b'+' => Ok(62),
        b'/' => Ok(63),
        _ => Err(ExcalidrawError::DecompressionFailed(format!("invalid base64 character '{}'", b as char))),
    }
}

struct BitReader<'a> {
    bytes: &'a [u8],
    index: usize,
    val: u32,
    position: u32,
    eof: bool,
}

impl<'a> BitReader<'a> {
    fn new(bytes: &'a [u8]) -> Result<Self, ExcalidrawError> {
        if bytes.is_empty() {
            return Err(ExcalidrawError::DecompressionFailed("empty lz-string payload".into()));
        }
        if bytes[0] == b'=' {
            return Err(ExcalidrawError::DecompressionFailed("unexpected '=' padding at start".into()));
        }
        Ok(Self {
            bytes,
            index: 1,
            val: base64_val(bytes[0])?,
            position: 32,
            eof: false,
        })
    }

    #[inline]
    fn read_bits(&mut self, n_bits: u32) -> Result<u32, ExcalidrawError> {
        let mut bits = 0u32;
        let maxpower = 1u32 << n_bits;
        let mut power = 1u32;
        while power != maxpower {
            if self.eof {
                return Err(ExcalidrawError::DecompressionFailed("truncated lz-string stream".into()));
            }
            let resb = self.val & self.position;
            self.position >>= 1;
            if self.position == 0 {
                self.position = 32;
                if self.index < self.bytes.len() {
                    let b = self.bytes[self.index];
                    self.index += 1;
                    if b == b'=' {
                        self.eof = true;
                        self.val = 0;
                    } else {
                        self.val = base64_val(b)?;
                    }
                } else {
                    self.eof = true;
                    self.val = 0;
                }
            }
            if resb != 0 {
                bits |= power;
            }
            power <<= 1;
        }
        Ok(bits)
    }
}

#[derive(Clone, Copy)]
struct DictEntry {
    prefix: u32,
    ch: u16,
    first: u16,
    len: u32,
}

#[inline]
fn emit_entry(dict: &[DictEntry], idx: u32, extra_last: Option<u16>, out: &mut Vec<u16>, max_units: usize) -> Result<(), ExcalidrawError> {
    let base_len = dict[idx as usize].len as usize;
    let total_len = base_len + usize::from(extra_last.is_some());
    if out.len().saturating_add(total_len) > max_units {
        return Err(ExcalidrawError::DecompressionLimit);
    }
    let start = out.len();
    out.resize(start + total_len, 0);
    if let Some(last) = extra_last {
        out[start + base_len] = last;
    }
    let mut cur = idx;
    for pos in (start..start + base_len).rev() {
        let e = dict[cur as usize];
        out[pos] = e.ch;
        cur = e.prefix;
    }
    Ok(())
}

/// Decompresses a Pieroxy `lz-string` Base64 string (with optional ASCII whitespace/line-wrapping).
/// Stops immediately if output UTF-16 code units exceed `max_utf16_units`.
pub fn decompress_from_base64(input: &str, max_utf16_units: usize) -> Result<String, ExcalidrawError> {
    let mut cleaned = Vec::with_capacity(input.len());
    let mut seen_pad = false;
    for &b in input.as_bytes() {
        if b.is_ascii_whitespace() {
            continue;
        }
        if b == b'=' {
            seen_pad = true;
            cleaned.push(b);
        } else if seen_pad {
            return Err(ExcalidrawError::DecompressionFailed("non-padding character after '='".into()));
        } else {
            cleaned.push(b);
        }
    }

    let mut reader = BitReader::new(&cleaned)?;
    let mut dict: Vec<DictEntry> = Vec::with_capacity(256);
    for i in 0..3 {
        dict.push(DictEntry { prefix: u32::MAX, ch: i, first: i, len: 1 });
    }

    let mut enlarge_in: u32 = 4;
    let mut num_bits: u32 = 3;
    let mut result: Vec<u16> = Vec::new();

    let first_ch: u16 = match reader.read_bits(2)? {
        0 => reader.read_bits(8)? as u16,
        1 => reader.read_bits(16)? as u16,
        2 => return Ok(String::new()),
        _ => return Err(ExcalidrawError::DecompressionFailed("invalid initial token".into())),
    };

    if max_utf16_units == 0 {
        return Err(ExcalidrawError::DecompressionLimit);
    }
    dict.push(DictEntry { prefix: u32::MAX, ch: first_ch, first: first_ch, len: 1 });
    result.push(first_ch);
    let mut w_idx: u32 = 3;

    loop {
        if result.len() > max_utf16_units {
            return Err(ExcalidrawError::DecompressionLimit);
        }
        let mut c = reader.read_bits(num_bits)?;
        match c {
            0 => {
                let ch = reader.read_bits(8)? as u16;
                c = dict.len() as u32;
                dict.push(DictEntry { prefix: u32::MAX, ch, first: ch, len: 1 });
                enlarge_in -= 1;
            }
            1 => {
                let ch = reader.read_bits(16)? as u16;
                c = dict.len() as u32;
                dict.push(DictEntry { prefix: u32::MAX, ch, first: ch, len: 1 });
                enlarge_in -= 1;
            }
            2 => break,
            _ => {}
        }

        if enlarge_in == 0 {
            enlarge_in = 1u32 << num_bits;
            num_bits += 1;
            if num_bits > 26 {
                return Err(ExcalidrawError::DecompressionLimit);
            }
        }

        let dict_len = dict.len() as u32;
        let entry_first = if c < dict_len {
            emit_entry(&dict, c, None, &mut result, max_utf16_units)?;
            dict[c as usize].first
        } else if c == dict_len {
            let w_first = dict[w_idx as usize].first;
            emit_entry(&dict, w_idx, Some(w_first), &mut result, max_utf16_units)?;
            w_first
        } else {
            return Err(ExcalidrawError::DecompressionFailed("invalid dictionary index".into()));
        };

        if dict.len() >= max_utf16_units.saturating_add(4) {
            return Err(ExcalidrawError::DecompressionLimit);
        }
        let w_entry = dict[w_idx as usize];
        let next_len = w_entry.len.saturating_add(1);
        dict.push(DictEntry {
            prefix: w_idx,
            ch: entry_first,
            first: w_entry.first,
            len: next_len,
        });
        enlarge_in -= 1;
        w_idx = c;

        if enlarge_in == 0 {
            enlarge_in = 1u32 << num_bits;
            num_bits += 1;
            if num_bits > 26 {
                return Err(ExcalidrawError::DecompressionLimit);
            }
        }
    }

    String::from_utf16(&result).map_err(|_| ExcalidrawError::DecompressionFailed("output is not valid UTF-16".into()))
}

#[cfg(test)]
pub fn compress_to_base64(input: &str) -> String {
    use rustc_hash::{FxHashMap, FxHashSet};

    const KEY: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    if input.is_empty() {
        return String::new();
    }

    struct BitWriter {
        out: String,
        val: u32,
        pos: u32,
    }
    impl BitWriter {
        fn push_bit(&mut self, bit: u32) {
            self.val = (self.val << 1) | (bit & 1);
            if self.pos == 5 {
                self.pos = 0;
                self.out.push(KEY[self.val as usize] as char);
                self.val = 0;
            } else {
                self.pos += 1;
            }
        }
        fn write_bits(&mut self, n: u32, mut v: u32) {
            for _ in 0..n {
                self.push_bit(v & 1);
                v >>= 1;
            }
        }
    }

    let mut dict: FxHashMap<Vec<u16>, u32> = FxHashMap::default();
    let mut to_create: FxHashSet<Vec<u16>> = FxHashSet::default();
    let mut w: Vec<u16> = Vec::new();
    let mut enlarge_in: u32 = 2;
    let mut dict_size: u32 = 3;
    let mut num_bits: u32 = 2;
    let mut bw = BitWriter { out: String::new(), val: 0, pos: 0 };

    for unit in input.encode_utf16() {
        let single = vec![unit];
        if !dict.contains_key(&single) {
            dict.insert(single.clone(), dict_size);
            dict_size += 1;
            to_create.insert(single);
        }
        let mut wc = w.clone();
        wc.push(unit);
        if dict.contains_key(&wc) {
            w = wc;
        } else {
            if to_create.remove(&w) {
                let first = w[0] as u32;
                if first < 256 {
                    bw.write_bits(num_bits, 0);
                    bw.write_bits(8, first);
                } else {
                    bw.write_bits(num_bits, 1);
                    bw.write_bits(16, first);
                }
                enlarge_in -= 1;
                if enlarge_in == 0 {
                    enlarge_in = 1 << num_bits;
                    num_bits += 1;
                }
            } else {
                let code = dict[&w];
                bw.write_bits(num_bits, code);
            }
            enlarge_in -= 1;
            if enlarge_in == 0 {
                enlarge_in = 1 << num_bits;
                num_bits += 1;
            }
            dict.insert(wc, dict_size);
            dict_size += 1;
            w = vec![unit];
        }
    }

    if !w.is_empty() {
        if to_create.remove(&w) {
            let first = w[0] as u32;
            if first < 256 {
                bw.write_bits(num_bits, 0);
                bw.write_bits(8, first);
            } else {
                bw.write_bits(num_bits, 1);
                bw.write_bits(16, first);
            }
            enlarge_in -= 1;
            if enlarge_in == 0 {
                enlarge_in = 1 << num_bits;
                num_bits += 1;
            }
        } else {
            let code = dict[&w];
            bw.write_bits(num_bits, code);
        }
        enlarge_in -= 1;
        if enlarge_in == 0 {
            num_bits += 1;
        }
    }

    bw.write_bits(num_bits, 2);
    loop {
        bw.val <<= 1;
        if bw.pos == 5 {
            bw.out.push(KEY[bw.val as usize] as char);
            break;
        } else {
            bw.pos += 1;
        }
    }
    while !bw.out.len().is_multiple_of(4) {
        bw.out.push('=');
    }
    bw.out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_vectors_and_roundtrips() {
        // Exact Pieroxy LZString.compressToBase64("hello") vector
        assert_eq!(compress_to_base64("hello"), "BYUwNmD2Q===");
        assert_eq!(decompress_from_base64("BYUwNmD2Q===", 1024).unwrap(), "hello");
        // Without padding
        assert_eq!(decompress_from_base64("BYUwNmD2Q", 1024).unwrap(), "hello");

        let samples = [
            "a",
            "{\"type\":\"excalidraw\",\"elements\":[]}",
            "Unicode: 日本語 🚀🎨✨ — curly “quotes” and surrogate pairs 𐐷",
            &"repeated pattern 12345 ".repeat(500),
        ];
        for s in samples {
            let c = compress_to_base64(s);
            // Line-wrap at 256 chars with blank lines like Obsidian Excalidraw
            let wrapped = c.as_bytes().chunks(256).map(|x| std::str::from_utf8(x).unwrap()).collect::<Vec<_>>().join("\n\n");
            let d = decompress_from_base64(&wrapped, 1024 * 1024).unwrap();
            assert_eq!(d, s);
        }
    }

    #[test]
    fn rejects_bomb_truncated_and_invalid() {
        let big = "A".repeat(10_000);
        let compressed = compress_to_base64(&big);
        assert_eq!(decompress_from_base64(&compressed, 512), Err(ExcalidrawError::DecompressionLimit));

        assert!(matches!(decompress_from_base64("", 1024), Err(ExcalidrawError::DecompressionFailed(_))));
        assert!(matches!(decompress_from_base64("!!!", 1024), Err(ExcalidrawError::DecompressionFailed(_))));
        assert!(matches!(decompress_from_base64("BYUw", 1024), Err(ExcalidrawError::DecompressionFailed(_))));
        assert!(matches!(decompress_from_base64("BYUw=A==", 1024), Err(ExcalidrawError::DecompressionFailed(_))));
    }
}
