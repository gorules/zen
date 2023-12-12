use std::cell::Cell;

#[derive(Debug)]
pub(super) struct Cursor<'a> {
    chars: &'a [u8],
    current: Cell<usize>,
}

pub(super) type CursorItem = (usize, char);

impl<'a> From<&'a str> for Cursor<'a> {
    fn from(source: &'a str) -> Self {
        Self {
            chars: source.as_bytes(),
            current: Cell::new(0),
        }
    }
}

impl Cursor<'_> {
    pub fn next(&self) -> Option<CursorItem> {
        self.advance_by(1)
    }

    pub fn next_if<F>(&self, f: F) -> Option<CursorItem>
    where
        F: Fn(char) -> bool,
    {
        let (_, c) = self.peek()?;
        if f(c) {
            return self.next();
        }

        None
    }

    pub fn next_if_is(&self, s: &str) -> bool {
        let current = self.current.get();
        let is_valid = s.chars().all(|c| self.next_if(|ca| ca == c).is_some());
        if !is_valid {
            self.current.set(current);
        }

        is_valid
    }

    pub fn back(&self) -> Option<CursorItem> {
        self.back_by(1)
    }

    pub fn peek(&self) -> Option<CursorItem> {
        self.peek_by(1)
    }

    #[allow(dead_code)]
    pub fn peek_back(&self) -> Option<CursorItem> {
        self.peek_back_by(1)
    }

    pub fn peek_by(&self, n: usize) -> Option<CursorItem> {
        self.nth(self.current.get() + n)
    }

    #[allow(dead_code)]
    pub fn peek_back_by(&self, n: usize) -> Option<CursorItem> {
        self.nth(self.current.get() - n)
    }

    pub fn position(&self) -> usize {
        self.current.get()
    }

    pub fn advance_by(&self, n: usize) -> Option<CursorItem> {
        self.current.set(self.current.get() + n);
        self.current()
    }

    pub fn back_by(&self, n: usize) -> Option<CursorItem> {
        self.current.set(self.current.get() - n);
        self.current()
    }

    pub fn current(&self) -> Option<CursorItem> {
        self.nth(self.current.get())
    }

    pub fn nth(&self, n: usize) -> Option<CursorItem> {
        let &c = self.chars.get(n - 1)?;
        Some((n - 1, c as char))
    }
}
