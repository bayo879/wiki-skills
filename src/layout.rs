//! The engine layout: one table, one owner per directory.
//!
//! `init` creates every entry. Commands may repair the derived ones, but the
//! authored product (`.wiki/pages`) is the user's decision: the gate refuses
//! when it is missing, and `provision` must never create it, not even through a
//! child directory such as `.wiki/pages/archive`.

/// One directory the engine owns.
#[derive(Debug, Clone, Copy)]
pub struct Entry {
    /// Path relative to the repository root, `/`-separated.
    pub path: &'static str,
    /// `true` for the product the user authored: commands must refuse rather
    /// than create it.
    pub authored: bool,
}

/// Parents come before children so each `create_dir_all` sees its parent.
pub const LAYOUT: [Entry; 10] = [
    Entry {
        path: ".wiki",
        authored: false,
    },
    Entry {
        path: ".wiki/cache",
        authored: false,
    },
    Entry {
        path: ".wiki/objects",
        authored: false,
    },
    Entry {
        path: ".wiki/pages",
        authored: true,
    },
    Entry {
        path: ".wiki/pages/archive",
        authored: false,
    },
    Entry {
        path: ".wiki/pages/audit",
        authored: false,
    },
    Entry {
        path: ".wiki/raw",
        authored: false,
    },
    Entry {
        path: ".wiki/sessions",
        authored: false,
    },
    Entry {
        path: ".wiki/staging",
        authored: false,
    },
    Entry {
        path: ".wiki/versions",
        authored: false,
    },
];

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::LAYOUT;

    #[test]
    fn the_only_authored_directory_is_the_page_tree() {
        let authored: Vec<_> = LAYOUT
            .iter()
            .filter(|entry| entry.authored)
            .map(|entry| entry.path)
            .collect();
        assert_eq!(authored, vec![".wiki/pages"]);
    }

    #[test]
    fn parents_come_before_their_children() {
        for (index, entry) in LAYOUT.iter().enumerate() {
            let Some((parent, _)) = entry.path.rsplit_once('/') else {
                continue;
            };
            let parent_index = LAYOUT
                .iter()
                .position(|candidate| candidate.path == parent)
                .unwrap_or_else(|| panic!("{} has no parent entry", entry.path));
            assert!(
                parent_index < index,
                "{} must come after its parent {parent}",
                entry.path
            );
        }
    }

    #[test]
    fn every_entry_is_a_distinct_path_under_the_engine() {
        let mut seen = BTreeSet::new();
        for entry in LAYOUT {
            assert!(
                entry.path == ".wiki" || entry.path.starts_with(".wiki/"),
                "{} is outside the engine",
                entry.path
            );
            assert!(seen.insert(entry.path), "duplicate path {}", entry.path);
        }
    }
}
