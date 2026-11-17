//! Helpers shared by the importers: INI parsing, text decoding, block-cipher modes.

use aes::cipher::{Block, BlockCipherDecrypt, BlockCipherEncrypt, KeyInit};

pub(super) struct IniSection {
    pub name: String,
    pub entries: Vec<(String, String)>,
}

impl IniSection {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }
}

/// Minimal INI reader that keeps order and never treats `#` or `;` inside a value as a comment.
pub(super) fn parse_ini(text: &str) -> Vec<IniSection> {
    let mut sections: Vec<IniSection> = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            sections.push(IniSection {
                name: name.trim().to_string(),
                entries: Vec::new(),
            });
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if sections.is_empty() {
            sections.push(IniSection {
                name: String::new(),
                entries: Vec::new(),
            });
        }
        if let Some(section) = sections.last_mut() {
            section.entries.push((key.trim().to_string(), value.trim().to_string()));
        }
    }
    sections
}

pub(super) fn find_section<'a>(sections: &'a [IniSection], name: &str) -> Option<&'a IniSection> {
    sections.iter().find(|s| s.name.eq_ignore_ascii_case(name))
}

/// UTF-8 (with or without BOM), falling back to Windows-1252.
pub(super) fn decode_text(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => bytes.iter().map(|&b| cp1252(b)).collect(),
    }
}

fn cp1252(b: u8) -> char {
    const HIGH: [char; 32] = [
        '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8d}', 'Ž', '\u{8f}', '\u{90}', '‘',
        '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9d}', 'ž', 'Ÿ',
    ];
    match b {
        0x80..=0x9f => HIGH[(b - 0x80) as usize],
        _ => b as char,
    }
}

/// A host or folder name must not contain '/', which separates folders in `Host::path()`.
pub(super) fn clean_segment(s: &str) -> String {
    s.trim().replace('/', "-")
}

/// Normalizes "a//b/" or " a / b " to "a/b".
pub(super) fn clean_folder(s: &str) -> String {
    s.split('/')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("/")
}

pub(super) fn non_empty(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// Port 22 is ssh's default, so it is stored as `None`.
pub(super) fn port_opt(port: u16) -> Option<u16> {
    (port != 22 && port != 0).then_some(port)
}

pub(super) fn hex_decode(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| s.get(i..i + 2).and_then(|h| u8::from_str_radix(h, 16).ok()))
        .collect()
}

pub(super) fn cbc_decrypt<C: BlockCipherDecrypt>(cipher: &C, iv: &[u8], data: &[u8]) -> Option<Vec<u8>> {
    let bs = C::block_size();
    if data.is_empty() || !data.len().is_multiple_of(bs) || iv.len() != bs {
        return None;
    }
    let mut prev = iv;
    let mut out = Vec::with_capacity(data.len());
    for chunk in data.chunks(bs) {
        let mut block = Block::<C>::try_from(chunk).ok()?;
        cipher.decrypt_block(&mut block);
        out.extend(block.iter().zip(prev).map(|(b, p)| b ^ p));
        prev = chunk;
    }
    Some(out)
}

pub(super) fn pkcs7_unpad(mut data: Vec<u8>, bs: usize) -> Option<Vec<u8>> {
    let pad = *data.last()? as usize;
    if pad == 0 || pad > bs || pad > data.len() || !data[data.len() - pad..].iter().all(|&b| b as usize == pad) {
        return None;
    }
    data.truncate(data.len() - pad);
    Some(data)
}

/// AES-256 in CFB mode with 8-bit feedback.
pub(super) fn aes256_cfb8_decrypt(key: &[u8], iv: &[u8], data: &[u8]) -> Option<Vec<u8>> {
    let cipher = aes::Aes256::new_from_slice(key).ok()?;
    let mut register = Block::<aes::Aes256>::try_from(iv).ok()?;
    let mut out = Vec::with_capacity(data.len());
    for &c in data {
        let mut keystream = register;
        cipher.encrypt_block(&mut keystream);
        out.push(c ^ keystream[0]);
        register.copy_within(1.., 0);
        register[15] = c;
    }
    Some(out)
}

pub(super) fn aes256_encrypt_block(key: &[u8], block: [u8; 16]) -> Option<[u8; 16]> {
    let cipher = aes::Aes256::new_from_slice(key).ok()?;
    let mut b = Block::<aes::Aes256>::from(block);
    cipher.encrypt_block(&mut b);
    Some(b.into())
}

/// A decrypted secret is accepted only if it is printable UTF-8; garbage means a wrong key.
pub(super) fn printable_secret(bytes: Vec<u8>) -> Option<String> {
    let s = String::from_utf8(bytes).ok()?;
    (!s.chars().any(char::is_control)).then_some(s)
}

/// POSIX-shell-like word splitting (quotes and backslashes); `None` on an unterminated quote.
pub(super) fn shell_split(s: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '\'' => break,
                        ch => cur.push(ch),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => match chars.next()? {
                            ch @ ('"' | '\\' | '$' | '`') => cur.push(ch),
                            ch => {
                                cur.push('\\');
                                cur.push(ch);
                            }
                        },
                        ch => cur.push(ch),
                    }
                }
            }
            '\\' => {
                in_word = true;
                if let Some(ch) = chars.next() {
                    cur.push(ch);
                }
            }
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            c => {
                in_word = true;
                cur.push(c);
            }
        }
    }
    if in_word {
        words.push(cur);
    }
    Some(words)
}

/// Shell-style wildcard match with `*` and `?`.
pub(super) fn wildcard_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

/// Counts skipped entries per kind and renders one warning per kind.
#[derive(Default)]
pub(super) struct SkipCounter(std::collections::BTreeMap<String, Vec<String>>);

impl SkipCounter {
    pub fn add(&mut self, kind: &str, name: &str) {
        self.0.entry(kind.to_string()).or_default().push(name.to_string());
    }

    pub fn warnings(self, app: &str, only: &str, out: &mut Vec<String>) {
        for (kind, names) in self.0 {
            let mut list = names
                .iter()
                .take(5)
                .map(|n| format!("'{n}'"))
                .collect::<Vec<_>>()
                .join(", ");
            if names.len() > 5 {
                list.push_str(&format!(" and {} more", names.len() - 5));
            }
            out.push(format!(
                "{app}: skipped {} {kind} session(s) ({list}); only {only} sessions are imported",
                names.len()
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ini_keeps_hashes_in_values() {
        let s = parse_ini("; c\n[A]\nk = #1#0%x%22\n[b]\nz=\n");
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].get("k"), Some("#1#0%x%22"));
        assert_eq!(find_section(&s, "B").and_then(|s| s.get("z")), Some(""));
    }

    #[test]
    fn cp1252_fallback() {
        assert_eq!(decode_text(b"caf\xe9 \x80"), "café €");
        assert_eq!(decode_text("\u{feff}ñ".as_bytes()), "ñ");
    }

    #[test]
    fn split_and_match() {
        assert_eq!(
            shell_split(r#" -X -o "ConnectTimeout=10" -o 'A B' c\ d"#).unwrap(),
            vec!["-X", "-o", "ConnectTimeout=10", "-o", "A B", "c d"]
        );
        assert!(shell_split("\"open").is_none());
        assert!(wildcard_match("*.conf", "a.conf"));
        assert!(wildcard_match("web-?", "web-1"));
        assert!(!wildcard_match("*.conf", "a.conf.bak"));
    }

    #[test]
    fn padding_and_hex() {
        assert_eq!(pkcs7_unpad(vec![1, 2, 3, 5, 5, 5, 5, 5], 8), Some(vec![1, 2, 3]));
        assert_eq!(pkcs7_unpad(vec![1, 2, 3, 4, 5, 6, 7, 9], 8), None);
        assert_eq!(pkcs7_unpad(vec![8; 8], 8), Some(vec![]));
        assert_eq!(hex_decode("0aFf"), Some(vec![10, 255]));
        assert_eq!(hex_decode("zz"), None);
        assert_eq!(hex_decode("abc"), None);
    }
}
