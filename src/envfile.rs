use crate::Result;
use std::collections::BTreeMap;
use std::fs;
use std::ops::Range;
use std::path::Path;

pub fn read_vars(path: &Path) -> Result<Vec<(String, String)>> {
    let bytes = fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    read_vars_bytes(&bytes)
}

pub fn keys(path: &Path) -> Result<Vec<String>> {
    let mut result: Vec<String> = read_vars(path)?.into_iter().map(|(key, _)| key).collect();
    result.sort();
    result.dedup();
    Ok(result)
}

pub fn get(path: &Path, key: &str) -> Result<String> {
    read_vars(path)?
        .into_iter()
        .rev()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value)
        .ok_or_else(|| format!("key '{key}' is absent"))
}

pub fn read_vars_bytes(bytes: &[u8]) -> Result<Vec<(String, String)>> {
    read_vars_with_context(bytes, &inherited_vars())
}

pub fn inherited_vars() -> BTreeMap<String, String> {
    std::env::vars_os()
        .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
        .collect()
}

// Mask references before dotenvy parses so it cannot consult the process environment.
// Resolve them once, in assignment order, against the caller's effective environment.
pub fn read_vars_with_context(
    bytes: &[u8],
    inherited: &BTreeMap<String, String>,
) -> Result<Vec<(String, String)>> {
    let text = std::str::from_utf8(bytes).map_err(|_| "env file is not UTF-8")?;
    let mut prefix = "__ENVRUN_REF_".to_string();
    while text.contains(&prefix) {
        prefix.push('_');
    }
    let mut masked = String::new();
    let mut references = Vec::new();
    let mut chars = text.chars().peekable();
    let mut quote = None;
    let mut escaped = false;
    let mut comment = false;
    let mut previous = '\n';
    let mut previous_escaped = false;
    let mut seen_equal = false;
    let mut at_value_start = false;
    while let Some(c) = chars.next() {
        let was_escaped = escaped;
        if comment {
            if c == '\n' {
                comment = false;
            }
        } else if escaped {
            escaped = false;
        } else if c == '\\' && quote != Some('\'') {
            escaped = true;
        } else if (c == '\'' && quote != Some('"')) || (c == '"' && quote != Some('\'')) {
            quote = if quote.is_some() { None } else { Some(c) };
        } else if c == '#'
            && quote.is_none()
            && ((previous.is_whitespace() && !previous_escaped) || at_value_start)
        {
            comment = true;
        } else if c == '$' && quote != Some('\'') {
            let mut name = String::new();
            if chars.peek() == Some(&'{') {
                chars.next();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some(c) => name.push(c),
                        None => return Err("unterminated env variable reference".into()),
                    }
                }
                if !valid_key(&name) {
                    return Err("unsupported env variable reference; use ${NAME}".into());
                }
            } else {
                while chars
                    .peek()
                    .is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_')
                {
                    name.push(chars.next().unwrap());
                }
            }
            if name.is_empty() {
                masked.push_str("\\$");
            } else {
                masked.push_str(&format!("{prefix}{}__", references.len()));
                references.push(name);
            }
            previous = '$';
            previous_escaped = false;
            at_value_start = false;
            continue;
        }
        masked.push(c);
        previous = c;
        previous_escaped = was_escaped;
        if c == '\n' && quote.is_none() {
            seen_equal = false;
            at_value_start = false;
        } else if c == '=' && !seen_equal {
            seen_equal = true;
            at_value_start = true;
        } else if !c.is_whitespace() {
            at_value_start = false;
        }
    }
    let mut context = inherited.clone();
    let mut result = Vec::new();
    for item in dotenvy::from_read_iter(masked.as_bytes()) {
        // Parser errors may contain secret values, so never forward their text.
        let (key, encoded) = item.map_err(|_| "invalid env file syntax".to_string())?;
        let mut value = String::new();
        let mut rest = encoded.as_str();
        while let Some(at) = rest.find(&prefix) {
            value.push_str(&rest[..at]);
            rest = &rest[at + prefix.len()..];
            let end = rest.find("__").ok_or("invalid env reference marker")?;
            let id: usize = rest[..end]
                .parse()
                .map_err(|_| "invalid env reference marker")?;
            let name = references.get(id).ok_or("invalid env reference marker")?;
            value.push_str(context.get(name).map(String::as_str).unwrap_or_default());
            rest = &rest[end + 2..];
        }
        value.push_str(rest);
        if key.contains('\0') || value.contains('\0') {
            return Err("env file contains a NUL byte".into());
        }
        context.insert(key.clone(), value.clone());
        result.push((key, value));
    }
    Ok(result)
}

fn valid_key(key: &str) -> bool {
    let mut chars = key.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
}

fn line_end(text: &str, start: usize) -> usize {
    text[start..]
        .find('\n')
        .map_or(text.len(), |at| start + at + 1)
}

fn spans(text: &str) -> Result<Vec<(String, Range<usize>)>> {
    let mut result = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let mut end = line_end(text, start);
        let line = text[start..end].trim();
        if line.is_empty() || line.starts_with('#') {
            start = end;
            continue;
        }
        // Let the same parser find complete assignments, including multiline values.
        loop {
            match read_vars_bytes(&text.as_bytes()[start..end]) {
                Ok(parsed) if parsed.len() == 1 => {
                    result.push((parsed[0].0.clone(), start..end));
                    break;
                }
                Ok(_) => return Err("assignment boundaries cannot be edited safely".into()),
                Err(_) if end < text.len() => end = line_end(text, end),
                Err(error) => return Err(error),
            }
        }
        start = end;
    }
    Ok(result)
}

fn quote_value(value: &str) -> String {
    let mut output = String::from("\"");
    for c in value.chars() {
        match c {
            '\\' => output.push_str("\\\\"),
            '"' => output.push_str("\\\""),
            '$' => output.push_str("\\$"),
            '\n' => output.push_str("\\n"),
            _ => output.push(c),
        }
    }
    output.push('"');
    output
}

pub fn edit(before: &[u8], key: &str, value: Option<&str>) -> Result<Vec<u8>> {
    if !valid_key(key) {
        return Err("invalid env key".into());
    }
    if value.is_some_and(|v| v.contains('\0')) {
        return Err("env value contains a NUL byte".into());
    }
    let parsed_before = read_vars_bytes(before)?;
    let text = std::str::from_utf8(before).map_err(|_| "env file is not UTF-8")?;
    let assignments = spans(text)?;
    if assignments
        .iter()
        .map(|(name, _)| name)
        .ne(parsed_before.iter().map(|(name, _)| name))
    {
        return Err("assignment boundaries cannot be edited safely".into());
    }
    let matches: Vec<_> = assignments
        .into_iter()
        .filter(|(name, _)| name == key)
        .collect();
    let parsed_count = parsed_before.iter().filter(|(name, _)| name == key).count();
    if parsed_count != matches.len() {
        return Err(format!(
            "key '{key}' uses assignment syntax that cannot be edited safely"
        ));
    }
    if matches.len() > 1 {
        return Err(format!(
            "key '{key}' appears more than once; edit the file manually"
        ));
    }
    let replacement = value.map(|v| format!("{key}={}", quote_value(v)));
    let mut after = String::new();
    if let Some((_, span)) = matches.first() {
        after.push_str(&text[..span.start]);
        if let Some(replacement) = &replacement {
            after.push_str(replacement);
            if text[span.clone()].ends_with('\n') {
                after.push('\n');
            }
        }
        after.push_str(&text[span.end..]);
    } else if let Some(replacement) = &replacement {
        after.push_str(text);
        if !text.is_empty() && !text.ends_with('\n') {
            after.push('\n');
        }
        after.push_str(replacement);
        after.push('\n');
    } else {
        return Err(format!("key '{key}' is absent"));
    }
    let parsed = read_vars_bytes(after.as_bytes())?;
    if parsed.iter().filter(|(name, _)| name == key).count() != usize::from(value.is_some()) {
        return Err("edit did not produce the expected assignment".into());
    }
    if parsed
        .iter()
        .filter(|(name, _)| name != key)
        .map(|(name, _)| name)
        .ne(parsed_before
            .iter()
            .filter(|(name, _)| name != key)
            .map(|(name, _)| name))
    {
        return Err("edit would change unrelated assignments".into());
    }
    if let Some(expected) = value {
        let actual = parsed
            .into_iter()
            .rev()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value);
        if actual.as_deref() != Some(expected) {
            return Err("new value did not round-trip through dotenv parser".into());
        }
    }
    Ok(after.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comments_and_mixed_quotes_cannot_consume_neighbor_assignments() {
        for assignment in [
            "KEY=\"old\" # a stray \" in comment\n",
            "KEY='old' # a stray ' in comment\n",
            "KEY=prefix\"first\nsecond\" # \"\n",
            "KEY=\"first\nsecond\" # \"\n",
        ] {
            let before = format!("{assignment}KEEP=\"must-remain\"\nTAIL=ok\n");
            let after = edit(before.as_bytes(), "KEY", Some("new")).unwrap();
            assert_eq!(after, b"KEY=\"new\"\nKEEP=\"must-remain\"\nTAIL=ok\n");
            assert_eq!(
                edit(before.as_bytes(), "KEY", None).unwrap(),
                b"KEEP=\"must-remain\"\nTAIL=ok\n"
            );
        }
    }

    #[test]
    fn expansion_uses_effective_values_and_preserves_literals() {
        let context = BTreeMap::from([
            ("HOST".into(), "shell".into()),
            ("ROOT_NAME".into(), "root".into()),
        ]);
        let values = read_vars_with_context(
            br#"HOST=file
URL="https://${HOST}/$ROOT_NAME"
LITERAL='$HOST'
ESCAPED="\$HOST"
EMPTY=${MISSING}
HASH="a # ${HOST}" # " $IGNORE
ESCAPED_SPACE=prefix\ #${HOST}
EQUAL_HASH=prefix=#${HOST}
COMMENT=# ${HOST}
"#,
            &context,
        )
        .unwrap();
        assert_eq!(
            values,
            vec![
                ("HOST".into(), "file".into()),
                ("URL".into(), "https://file/root".into()),
                ("LITERAL".into(), "$HOST".into()),
                ("ESCAPED".into(), "$HOST".into()),
                ("EMPTY".into(), "".into()),
                ("HASH".into(), "a # file".into()),
                ("ESCAPED_SPACE".into(), "prefix #file".into()),
                ("EQUAL_HASH".into(), "prefix=#file".into()),
                ("COMMENT".into(), "".into()),
            ]
        );
    }

    #[test]
    fn edits_multiline_without_touching_neighbors() {
        let before = b"# kept\nKEY=\"first\nsecond\"\nNEXT=x\n";
        let after = edit(before, "KEY", Some("new\n\"quoted\" $literal")).unwrap();
        assert!(after.starts_with(b"# kept\n"));
        assert!(after.ends_with(b"NEXT=x\n"));
        assert_eq!(
            read_vars_bytes(&after).unwrap()[0].1,
            "new\n\"quoted\" $literal"
        );
        assert_eq!(edit(&after, "KEY", None).unwrap(), b"# kept\nNEXT=x\n");
    }

    #[test]
    fn rejects_duplicate_key_without_exposing_value() {
        let error = edit(b"KEY=secret-one\nKEY=secret-two\n", "KEY", Some("new")).unwrap_err();
        assert!(error.contains("more than once"));
        assert!(!error.contains("secret"));
    }
}
