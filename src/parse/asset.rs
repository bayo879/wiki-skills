//! The `builtin-asset` backend: images become links, never text.
//!
//! An image has no text layer to extract, so this backend does not parse bytes
//! at all: it renders one markdown image link to the original file under
//! `source/` and marks the result as an asset. That keeps the raw layer complete
//! (the link is the raw content) while telling `ingest` that there is nothing to
//! summarise.
//!
//! The link is relative to the raw file, which lives at `raw/<key minus ext>.md`.
//! `raw/` mirrors `source/`, so a raw file's depth equals the number of path
//! components in the source key: `a.png` sits one level down and needs `../`,
//! while `dir/a.png` sits two levels down and needs `../../`.

use super::{Extracted, Parser, Result, Source, Warning};
use crate::paths;

/// Backend id recorded in the raw frontmatter and in command output.
pub const ID: &str = "builtin-asset";

/// Extensions this backend claims, in addition to `svg` (which is text but is
/// still only linked, because the wiki never inlines it).
const EXTENSIONS: [&str; 6] = ["png", "jpg", "jpeg", "gif", "webp", "svg"];

/// The `builtin-asset` backend.
pub struct AssetParser;

impl AssetParser {
    /// A stateless backend; construction exists so the registry reads uniformly.
    pub fn new() -> AssetParser {
        AssetParser
    }
}

impl Default for AssetParser {
    fn default() -> Self {
        AssetParser::new()
    }
}

impl Parser for AssetParser {
    fn id(&self) -> &'static str {
        ID
    }

    /// The image extensions, in any case.
    fn can_handle(&self, ext: &str) -> bool {
        EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str())
    }

    fn parse(&self, input: &Source) -> Result<Extracted> {
        let mut warnings = Vec::new();
        // An empty file is still a valid link target, so the link is emitted and
        // the degradation is only reported: dropping the reference would lose
        // the fact that the source exists.
        if input.bytes.is_empty() {
            warnings.push(Warning::new(
                "empty",
                format!(
                    "{} is an empty image file; the link is emitted anyway",
                    input.key
                ),
            ));
        }

        Ok(Extracted {
            markdown: link_for(&input.key),
            warnings,
            asset: true,
            no_text_layer: false,
        })
    }
}

/// The markdown image link for a source key, relative to the raw file.
///
/// `source_key` is the identity key without the `source/` prefix. The relative
/// prefix repeats `../` once per path component, because the raw file at
/// `raw/<key minus ext>.md` sits exactly that many levels below the repository
/// root and `source/<key>` sits the same number below it.
///
/// Public because the scanned-PDF path in a later task needs the same link (a
/// PDF with no text layer degrades to a link), and both must agree byte for
/// byte.
pub fn link_for(source_key: &str) -> String {
    let depth = source_key
        .split('/')
        .filter(|component| !component.is_empty())
        .count();
    let target = format!("{}source/{source_key}", "../".repeat(depth));
    if paths::needs_angle(&target) {
        format!("![](<{target}>)")
    } else {
        format!("![]({target})")
    }
}
