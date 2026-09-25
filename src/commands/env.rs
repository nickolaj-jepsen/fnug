//! Variable expansion in config `env` values.

/// Expand `$NAME` and `${NAME}` in `value` with `lookup`, and `$$` to a literal `$`. A `$` that
/// starts none of these stays as it is.
///
/// Returns the expanded value and the names `lookup` didn't find, which expand to nothing.
pub(crate) fn expand(
    value: &str,
    lookup: impl Fn(&str) -> Option<String>,
) -> (String, Vec<String>) {
    let mut out = String::with_capacity(value.len());
    let mut undefined = Vec::new();
    let mut rest = value;
    while let Some(dollar) = rest.find('$') {
        out.push_str(&rest[..dollar]);
        let after = &rest[dollar + 1..];
        let (name, len) = if let Some(escaped) = after.strip_prefix('$') {
            out.push('$');
            rest = escaped;
            continue;
        } else if let Some(braced) = after.strip_prefix('{') {
            match braced.find('}') {
                Some(end) if name_len(&braced[..end]) == end && end > 0 => {
                    (&braced[..end], end + 2)
                }
                _ => ("", 0),
            }
        } else {
            let end = name_len(after);
            (&after[..end], end)
        };
        if name.is_empty() {
            out.push('$');
            rest = after;
            continue;
        }
        match lookup(name) {
            Some(found) => out.push_str(&found),
            None => undefined.push(name.to_string()),
        }
        rest = &after[len..];
    }
    out.push_str(rest);
    (out, undefined)
}

/// Length of the variable name (`[A-Za-z_][A-Za-z0-9_]*`) at the start of `s`.
fn name_len(s: &str) -> usize {
    if !s.starts_with(|c: char| c == '_' || c.is_ascii_alphabetic()) {
        return 0;
    }
    s.find(|c: char| c != '_' && !c.is_ascii_alphanumeric())
        .unwrap_or(s.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup(name: &str) -> Option<String> {
        match name {
            "A" => Some("1".into()),
            "LONG_name2" => Some("x".into()),
            _ => None,
        }
    }

    #[test]
    fn expands_names_escapes_and_leaves_stray_dollars() {
        let cases = [
            ("$A", "1", vec![]),
            ("${A}b", "1b", vec![]),
            ("$Ab", "", vec!["Ab"]),
            ("$LONG_name2.txt", "x.txt", vec![]),
            ("pre-$A-post", "pre-1-post", vec![]),
            ("$$A", "$A", vec![]),
            ("$$$A", "$1", vec![]),
            ("$1 $ ${ ${} ${A", "$1 $ ${ ${} ${A", vec![]),
            ("trailing $", "trailing $", vec![]),
            ("${MISSING}!", "!", vec!["MISSING"]),
            ("ünï$A", "ünï1", vec![]),
        ];
        for (value, expected, missing) in cases {
            let (out, undefined) = expand(value, lookup);
            assert_eq!(out, expected, "{value}");
            assert_eq!(undefined, missing, "{value}");
        }
    }
}
