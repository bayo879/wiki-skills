//! Signature tests for the `builtin-asset` backend and `link_for`.
//!
//! The first three are the plan's original scenarios; the rest cover the
//! properties the plan leaves implicit but that the raw layer depends on: the
//! relative prefix must follow the key's depth (not a fixed `../../`), escaping
//! only applies when the destination needs it, and every image is an asset with
//! no text layer, empty files included.

use llm_wiki::config::Config;
use llm_wiki::init;
use llm_wiki::parse::asset::{link_for, AssetParser};
use llm_wiki::parse::{Parser, Source};
use tempfile::tempdir;

/// Every extension the backend must claim, plus the ones it must not.
const IMAGE_EXTENSIONS: [&str; 8] = ["png", "PNG", "jpg", "jpeg", "gif", "webp", "svg", "JPG"];

#[test]
fn image_becomes_link_only_markdown() {
    let out = AssetParser::new()
        .parse(&Source::inline("dir/a.png", b"\x89PNG"))
        .unwrap();
    assert!(out.asset);
    assert!(!out.no_text_layer);
    assert_eq!(out.markdown.trim(), "![](../../source/dir/a.png)");
}

#[test]
fn names_with_spaces_and_parens_are_angle_bracketed() {
    let link = link_for("dir/图 (1)#x.png");
    assert_eq!(link, "![](<../../source/dir/图 (1)#x.png>)");
}

#[test]
fn empty_image_file_is_not_a_crash() {
    let out = AssetParser::new()
        .parse(&Source::inline("a.png", b""))
        .unwrap();
    assert!(out.asset);
    assert!(out.warnings.iter().any(|w| w.code == "empty"));
    // The link is still emitted, and one component means one `../`.
    assert_eq!(out.markdown.trim(), "![](../source/a.png)");
}

#[test]
fn a_one_component_key_uses_a_single_parent() {
    assert_eq!(link_for("a.png"), "![](../source/a.png)");
    assert!(!link_for("a.png").contains('<'), "no escape is needed here");
}

#[test]
fn a_three_component_key_uses_three_parents() {
    assert_eq!(link_for("a/b/c.png"), "![](../../../source/a/b/c.png)");
}

#[test]
fn percent_and_angle_brackets_force_the_quoted_form() {
    assert_eq!(link_for("a%20b<c>.png"), "![](<../source/a%20b<c>.png>)");
}

#[test]
fn extensions_are_claimed_case_insensitively() {
    let parser = AssetParser::new();
    for ext in IMAGE_EXTENSIONS {
        assert!(parser.can_handle(ext), "{ext} must be claimed");
    }
    for ext in ["pdf", "html", "text", ""] {
        assert!(!parser.can_handle(ext), "{ext} must not be claimed");
    }
}

#[test]
fn every_image_is_an_asset_with_no_text_layer() {
    let parser = AssetParser::new();
    for ext in IMAGE_EXTENSIONS {
        let out = parser
            .parse(&Source::inline(&format!("x.{ext}"), b"\x89PNG"))
            .unwrap();
        assert!(out.asset, "{ext} must be an asset");
        assert!(
            !out.no_text_layer,
            "{ext} must not claim a missing text layer"
        );
        assert!(out.markdown.starts_with("![]("), "{ext}: {}", out.markdown);
    }
}

#[test]
fn default_config_allows_every_image_extension() {
    let cfg = Config::default();
    for ext in ["png", "jpg", "jpeg", "gif", "webp", "svg"] {
        assert!(
            cfg.formats.allow.iter().any(|allowed| allowed == ext),
            "formats.allow must contain {ext}: {:?}",
            cfg.formats.allow
        );
    }
}

#[test]
fn init_template_lists_images_and_loads_back() {
    let d = tempdir().unwrap();
    init::run(d.path(), false).unwrap();

    let template = std::fs::read_to_string(d.path().join("wiki.toml")).unwrap();
    let allow_line = template
        .lines()
        .find(|line| line.trim_start().starts_with("allow"))
        .expect("the template must spell out formats.allow");
    for ext in ["png", "jpg", "jpeg", "gif", "webp", "svg"] {
        assert!(
            allow_line.contains(&format!("\"{ext}\"")),
            "template allow line must list {ext}: {allow_line}"
        );
    }

    let cfg = Config::load(d.path()).unwrap();
    for ext in ["png", "jpg", "jpeg", "gif", "webp", "svg"] {
        assert!(
            cfg.formats.allow.iter().any(|allowed| allowed == ext),
            "the template must read back {ext}: {:?}",
            cfg.formats.allow
        );
    }
}
