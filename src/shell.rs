//! POSIX-shell-style word splitting and quoting, for argument lists kept as one line of text.

/// Splits words like sh does (single/double quotes, backslashes); `None` on an unterminated quote.
pub fn split(s: &str) -> Option<Vec<String>> {
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

/// Joins words so that `split` gives them back, quoting the ones that need it.
pub fn join(words: &[String]) -> String {
    words.iter().map(|w| quote(w)).collect::<Vec<_>>().join(" ")
}

fn quote(word: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "@%+=:,./_-".contains(c);
    if !word.is_empty() && word.chars().all(safe) {
        return word.to_string();
    }
    format!("'{}'", word.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_like_sh() {
        assert_eq!(
            split(r#" -X -o "ConnectTimeout=10" -o 'A B' c\ d"#).unwrap(),
            vec!["-X", "-o", "ConnectTimeout=10", "-o", "A B", "c d"]
        );
        assert_eq!(split(r#""a\"b" 'it''s' """#).unwrap(), vec![r#"a"b"#, "its", ""]);
        assert!(split("\"open").is_none());
        assert!(split("'open").is_none());
        assert_eq!(split("  ").unwrap(), Vec::<String>::new());
    }

    #[test]
    fn join_round_trips() {
        let cases: Vec<Vec<String>> = vec![
            vec!["-o".into(), "ProxyCommand=ssh -W %h:%p gw.lab".into()],
            vec!["-o".into(), "ServerAliveInterval=30".into(), "-C".into()],
            vec!["it's".into(), "".into(), "a\"b\\c $HOME".into(), "tab\there".into()],
        ];
        for words in cases {
            assert_eq!(split(&join(&words)).unwrap(), words);
        }
        assert_eq!(join(&["-o".into(), "ServerAliveInterval=30".into()]), "-o ServerAliveInterval=30");
        assert_eq!(join(&["ProxyCommand=ssh -W %h:%p gw".into()]), "'ProxyCommand=ssh -W %h:%p gw'");
    }
}
