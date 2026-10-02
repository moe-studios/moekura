//! Google's image servers (googleusercontent.com, ggpht.com), behind
//! YouTube, Google Photos and others: `=s0`-style samples of `=d`
//! originals.

use super::parts::Parts;
use super::{GOOGLE, SourceUrl};

pub(super) fn parse(p: &Parts) -> Option<SourceUrl> {
    let lh =
        p.sub.contains("lh") && matches!(p.domain.as_str(), "ggpht.com" | "googleusercontent.com");
    if !lh && !matches!(p.domain.as_str(), "goo.gl" | "forms.gle") {
        return None;
    }
    let found = SourceUrl::of(&GOOGLE);
    if !lh {
        return Some(found);
    }
    let path = p.path();
    Some(match (p.domain.as_str(), path.as_slice()) {
        ("googleusercontent.com", [dirs @ .., id]) => {
            let id = id.split('=').next().unwrap_or(id);
            let mut parts = vec![p.origin()];
            parts.extend(dirs.iter().map(|d| (*d).to_owned()));
            parts.push(format!("{id}=d"));
            found.file(parts.join("/"))
        }
        ("ggpht.com", [a, b, c, d, file]) if p.ext().is_some() => {
            found.file(format!("{}/{a}/{b}/{c}/{d}/d/{file}", p.origin()))
        }
        ("ggpht.com", [a, b, c, d, _, file, ..]) => {
            found.file(format!("{}/{a}/{b}/{c}/{d}/d/{file}", p.origin()))
        }
        _ => found.file(None),
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::file;
    use super::*;

    #[test]
    fn images() {
        file(
            &GOOGLE,
            "https://lh3.googleusercontent.com/qAhRBhfciCcosUoYHPJr5WtNYSJ81vpSqcQwbQitZtsR3mB2aCUj7J5LvhJOCfWn=s0",
            Some(
                "https://lh3.googleusercontent.com/qAhRBhfciCcosUoYHPJr5WtNYSJ81vpSqcQwbQitZtsR3mB2aCUj7J5LvhJOCfWn=d",
            ),
        );
        file(
            &GOOGLE,
            "http://lh3.ggpht.com/_0qYlQ9JkXnE/Ryz9b1yXRDI/AAAAAAAAAu4/Iv0WPaT7uWY/016.jpg",
            Some(
                "https://lh3.ggpht.com/_0qYlQ9JkXnE/Ryz9b1yXRDI/AAAAAAAAAu4/Iv0WPaT7uWY/d/016.jpg",
            ),
        );
    }
}
