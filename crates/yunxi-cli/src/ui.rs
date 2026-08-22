//! Compact terminal styling that degrades cleanly without ANSI support.

use std::borrow::Cow;

pub(crate) struct Palette {
    enabled: bool,
}

impl Palette {
    pub(crate) fn new(enabled: bool) -> Self {
        Self { enabled }
    }

    pub(crate) fn title<'a>(&self, value: &'a str) -> Cow<'a, str> {
        self.paint(value, "\x1b[1;36m")
    }

    pub(crate) fn prompt<'a>(&self, value: &'a str) -> Cow<'a, str> {
        self.paint(value, "\x1b[1;32m")
    }

    pub(crate) fn assistant<'a>(&self, value: &'a str) -> Cow<'a, str> {
        self.paint(value, "\x1b[1;36m")
    }

    pub(crate) fn muted<'a>(&self, value: &'a str) -> Cow<'a, str> {
        self.paint(value, "\x1b[2m")
    }

    pub(crate) fn error<'a>(&self, value: &'a str) -> Cow<'a, str> {
        self.paint(value, "\x1b[1;31m")
    }

    fn paint<'a>(&self, value: &'a str, prefix: &str) -> Cow<'a, str> {
        if self.enabled {
            Cow::Owned(format!("{prefix}{value}\x1b[0m"))
        } else {
            Cow::Borrowed(value)
        }
    }
}
