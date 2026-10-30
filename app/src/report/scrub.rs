//! What leaves the machine from a log, the report's private attachment and the copy buttons: the
//! machine's own names ([`super::identity`]), then the shared outgoing policy of
//! `decanter_core::redact` (secrets, account ids, other players' NPIDs, addresses).
//!
//! Line by line, since no match spans lines and the identity guard folds its whole input: a log of
//! megabytes is one pass of short lines, never one fold of the whole text. Best effort for names the
//! identity does not list, which is why the attachment stays private.

use decanter_core::redact::{self, Hidden};
use decanter_report::{Guard, Identity};

use crate::i18n::Text;

/// What a scrub replaced, by kind; never the values themselves.
#[derive(Clone, Copy, Default)]
pub struct Cleaned {
    pub names: usize,
    pub values: usize,
    pub addresses: usize,
}

impl Cleaned {
    /// `key` with the three counts as `names`, `values` and `addresses`.
    pub fn text(self, key: &'static str) -> Text {
        Text::new(key)
            .arg("names", self.names)
            .arg("values", self.values)
            .arg("addresses", self.addresses)
    }
}

pub struct Scrubber {
    guard: Guard,
}

impl Scrubber {
    pub fn new(identity: &Identity) -> Self {
        Self {
            guard: Guard::new(identity),
        }
    }

    /// With the machine's names collected now: blocking file and database I/O, off the UI thread.
    pub fn collect() -> Self {
        Self::new(&super::identity::collect())
    }

    pub fn log(&self, log: &str) -> (String, Cleaned) {
        let mut out = String::with_capacity(log.len());
        let mut names = 0;
        let mut hidden = Hidden::default();
        let mut in_dump = false;
        for line in log.split_inclusive('\n') {
            // A `PS_FAULT_DUMP` block is raw guest memory, which no identity list can scrub: it is
            // dropped whole.
            if line.starts_with("Fault dump ") {
                in_dump = true;
                continue;
            }
            in_dump = in_dump && is_dump_row(line);
            if in_dump {
                continue;
            }
            let (line, found) = self.guard.scrub_counted(line);
            names += found;
            out.push_str(&redact::outgoing(&line, &mut hidden));
        }
        let cleaned = Cleaned {
            names: names + hidden.ids,
            values: hidden.values,
            addresses: hidden.addresses,
        };
        (out, cleaned)
    }
}

/// A row of a fault dump: two spaces, a 16-digit address and a colon, then the bytes or `unreadable`.
fn is_dump_row(line: &str) -> bool {
    line.strip_prefix("  ")
        .and_then(|row| row.split_once(':'))
        .is_some_and(|(address, _)| address.len() == 16 && address.bytes().all(|byte| byte.is_ascii_hexdigit()))
}
