//! Notes: text in boxes on a post's image.

/// Longest note, in characters.
pub const MAX_LEN: usize = 10_000;

/// Most active notes on one post.
pub const MAX_PER_POST: usize = 500;

/// A note's box, in the original image's pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoteBox {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BoxError {
    #[error("The box is empty: it needs a width and height of at least 1 pixel.")]
    Empty,
    #[error("The box is outside the image.")]
    Outside,
}

impl NoteBox {
    /// Checks the box, cut to fit a `width` × `height` image. Boxes partly
    /// outside the image (dragged past its edge) are trimmed to it.
    pub fn fit(self, width: i32, height: i32) -> Result<Self, BoxError> {
        if self.width < 1 || self.height < 1 {
            return Err(BoxError::Empty);
        }
        let left = self.x.max(0);
        let top = self.y.max(0);
        let right = self.x.saturating_add(self.width).min(width);
        let bottom = self.y.saturating_add(self.height).min(height);
        if right <= left || bottom <= top {
            return Err(BoxError::Outside);
        }
        Ok(Self {
            x: left,
            y: top,
            width: right - left,
            height: bottom - top,
        })
    }
}

/// Tidies a note's text; `None` if it's empty.
pub fn clean_body(body: &str) -> Option<String> {
    let body = body.replace("\r\n", "\n").trim().to_owned();
    (!body.is_empty()).then_some(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(x: i32, y: i32, width: i32, height: i32) -> NoteBox {
        NoteBox {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn boxes_fit_the_image() {
        assert_eq!(b(10, 10, 20, 20).fit(100, 100), Ok(b(10, 10, 20, 20)));
        assert_eq!(b(-5, 90, 20, 20).fit(100, 100), Ok(b(0, 90, 15, 10)));
        assert_eq!(b(0, 0, 0, 5).fit(100, 100), Err(BoxError::Empty));
        assert_eq!(b(100, 0, 5, 5).fit(100, 100), Err(BoxError::Outside));
        assert_eq!(b(-10, 0, 5, 5).fit(100, 100), Err(BoxError::Outside));
    }

    #[test]
    fn bodies() {
        assert_eq!(clean_body(" Hi\r\nthere \n"), Some("Hi\nthere".into()));
        assert_eq!(clean_body("  "), None);
    }
}
