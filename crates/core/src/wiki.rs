//! Wiki page fields besides the text ([`crate::markup`]): other names.
//!
//! Other names are what a tag is called elsewhere: its Japanese name,
//! romanisations, a pixiv tag. Like tag names they use underscores for
//! spaces, but keep their case and script.

/// Other names a page can have.
pub const MAX_OTHER_NAMES: usize = 50;
/// Characters in one other name.
pub const OTHER_NAME_MAX_LEN: usize = 170;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OtherNamesError {
    #[error("A page can have at most {MAX_OTHER_NAMES} other names.")]
    TooMany,
    #[error("Other names can be at most {OTHER_NAME_MAX_LEN} characters long: `{0}` is longer.")]
    TooLong(String),
}

/// `raw` with whitespace as single underscores, trimmed of underscores.
pub fn normalize_other_name(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        if c.is_control() && !c.is_whitespace() {
            continue;
        }
        let c = if c.is_whitespace() { '_' } else { c };
        if c == '_' && (out.is_empty() || out.ends_with('_')) {
            continue;
        }
        out.push(c);
    }
    while out.ends_with('_') {
        out.pop();
    }
    out
}

/// Normalises other names, dropping empty ones and repeats (the first
/// of each stays, in order).
pub fn other_names<'a>(
    names: impl IntoIterator<Item = &'a str>,
) -> Result<Vec<String>, OtherNamesError> {
    let mut out: Vec<String> = Vec::new();
    for name in names {
        let name = normalize_other_name(name);
        if name.is_empty() || out.contains(&name) {
            continue;
        }
        if name.chars().count() > OTHER_NAME_MAX_LEN {
            return Err(OtherNamesError::TooLong(name));
        }
        out.push(name);
        // As soon as there are too many, so a long list stays quick.
        if out.len() > MAX_OTHER_NAMES {
            return Err(OtherNamesError::TooMany);
        }
    }
    Ok(out)
}

/// Other names from a form's box: separated by spaces or new lines.
pub fn parse_other_names(input: &str) -> Result<Vec<String>, OtherNamesError> {
    other_names(input.split_whitespace())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_other_names() {
        assert_eq!(
            parse_other_names("  猫  Neko_Chan\nneko__chan_ 猫 ").unwrap(),
            ["猫", "Neko_Chan", "neko_chan"]
        );
        assert_eq!(
            other_names(["long  hair ", "", "_"]).unwrap(),
            ["long_hair"]
        );
        let many: String = (0..=MAX_OTHER_NAMES).map(|i| format!("n{i} ")).collect();
        assert_eq!(parse_other_names(&many), Err(OtherNamesError::TooMany));
        // However long the list, and repeats don't count.
        let huge: String = (0..200_000).map(|i| format!("n{i} ")).collect();
        assert_eq!(parse_other_names(&huge), Err(OtherNamesError::TooMany));
        let repeated: String = (0..200_000)
            .map(|i| format!("n{} ", i % MAX_OTHER_NAMES))
            .collect();
        assert_eq!(parse_other_names(&repeated).unwrap().len(), MAX_OTHER_NAMES);
        let long = "a".repeat(OTHER_NAME_MAX_LEN + 1);
        assert!(matches!(
            parse_other_names(&long),
            Err(OtherNamesError::TooLong(_))
        ));
    }
}
