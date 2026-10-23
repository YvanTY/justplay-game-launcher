//! Reader for PS3 `PARAM.SFO` files (title, serial, category).

use std::collections::HashMap;

const MAGIC: &[u8; 4] = b"\0PSF";
const HEADER_LEN: usize = 20;
const ENTRY_LEN: usize = 16;
const FMT_INT: u16 = 0x0404;

pub fn parse(bytes: &[u8]) -> Result<HashMap<String, String>, String> {
    if bytes.len() < HEADER_LEN || &bytes[..4] != MAGIC {
        return Err("not a PARAM.SFO file".into());
    }
    let key_table = u32_at(bytes, 8)? as usize;
    let data_table = u32_at(bytes, 12)? as usize;
    let count = u32_at(bytes, 16)? as usize;

    let mut values = HashMap::with_capacity(count);
    for i in 0..count {
        let entry = HEADER_LEN + i * ENTRY_LEN;
        let key_offset = u16_at(bytes, entry)? as usize;
        let format = u16_at(bytes, entry + 2)?;
        let len = u32_at(bytes, entry + 4)? as usize;
        let data_offset = u32_at(bytes, entry + 12)? as usize;

        let key = c_string(
            bytes
                .get(key_table + key_offset..)
                .ok_or("key outside file")?,
        );
        let start = data_table + data_offset;
        let data = bytes.get(start..start + len).ok_or("value outside file")?;
        let value = if format == FMT_INT {
            u32_at(data, 0)?.to_string()
        } else {
            c_string(data)
        };
        values.insert(key, value);
    }
    Ok(values)
}

/// PS3 titles sometimes contain line breaks ("God of War\nIII").
pub fn clean_title(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn c_string(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

fn u16_at(bytes: &[u8], at: usize) -> Result<u16, String> {
    let b = bytes.get(at..at + 2).ok_or("truncated PARAM.SFO")?;
    Ok(u16::from_le_bytes([b[0], b[1]]))
}

fn u32_at(bytes: &[u8], at: usize) -> Result<u32, String> {
    let b = bytes.get(at..at + 4).ok_or("truncated PARAM.SFO")?;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

#[cfg(test)]
pub(crate) fn build(entries: &[(&str, &str)]) -> Vec<u8> {
    let mut keys: Vec<u8> = Vec::new();
    let mut data: Vec<u8> = Vec::new();
    let mut index = Vec::new();
    for (key, value) in entries {
        let value = format!("{value}\0");
        index.extend((keys.len() as u16).to_le_bytes());
        index.extend(0x0204u16.to_le_bytes());
        index.extend((value.len() as u32).to_le_bytes());
        index.extend((value.len() as u32).to_le_bytes());
        index.extend((data.len() as u32).to_le_bytes());
        keys.extend(key.as_bytes());
        keys.push(0);
        data.extend(value.as_bytes());
    }
    let key_table = HEADER_LEN + index.len();
    let data_table = key_table + keys.len();
    let mut out = Vec::new();
    out.extend(MAGIC);
    out.extend(0x0101u32.to_le_bytes());
    out.extend((key_table as u32).to_le_bytes());
    out.extend((data_table as u32).to_le_bytes());
    out.extend((entries.len() as u32).to_le_bytes());
    out.extend(index);
    out.extend(keys);
    out.extend(data);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_string_entries() {
        let sfo = build(&[
            ("CATEGORY", "DG"),
            ("TITLE", "God of War\nIII"),
            ("TITLE_ID", "BCUS98111"),
        ]);
        let values = parse(&sfo).unwrap();
        assert_eq!(values["CATEGORY"], "DG");
        assert_eq!(values["TITLE_ID"], "BCUS98111");
        assert_eq!(clean_title(&values["TITLE"]), "God of War III");
    }

    #[test]
    fn rejects_garbage_and_truncation() {
        assert!(parse(b"hello world, not an sfo").is_err());
        let sfo = build(&[("TITLE", "Dante's Inferno")]);
        assert!(parse(&sfo[..sfo.len() - 8]).is_err());
    }
}
