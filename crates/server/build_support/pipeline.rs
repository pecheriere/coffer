//! The asset pipeline: how a file under `assets/` becomes a content-hashed URL.
//!
//! Compiled into `build.rs`, which generates the asset table, and into the
//! library's tests, which check the generated table against these rules.

/// The `Content-Type` served for a file, from its extension; `None` for an
/// extension the server does not serve.
pub fn content_type(logical: &str) -> Option<&'static str> {
    let (_, extension) = logical.rsplit_once('.')?;
    Some(match extension {
        "css" => "text/css; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "woff2" => "font/woff2",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "webmanifest" => "application/manifest+json",
        "txt" => "text/plain; charset=utf-8",
        _ => return None,
    })
}

/// Whether the file may name other assets as `asset:<path>`.
pub fn rewrites_references(logical: &str) -> bool {
    matches!(logical.rsplit_once('.'), Some((_, "css" | "webmanifest")))
}

/// The URL a file is served at: `/static/<dir>/<stem>.<hash>.<ext>`, with the
/// first 16 hex characters of the BLAKE3 hash of its (final) body.
pub fn hashed_url(logical: &str, body: &[u8]) -> String {
    let hash = blake3::hash(body).to_hex();
    let hash = &hash.as_str()[..16];
    let (dir, file) = logical
        .rsplit_once('/')
        .map_or(("", logical), |(dir, file)| (dir, file));
    let dir = if dir.is_empty() {
        String::new()
    } else {
        format!("{dir}/")
    };
    match file.rsplit_once('.') {
        Some((stem, extension)) => format!("/static/{dir}{stem}.{hash}.{extension}"),
        None => format!("/static/{dir}{file}.{hash}"),
    }
}

/// The Rust constant for a file: upper case, every other character `_`.
pub fn const_name(logical: &str) -> String {
    logical
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

/// Replaces each `asset:<path>` in `text` with `url_of(path)`. A path that
/// names no asset is an error naming it.
pub fn rewrite_references(
    text: &str,
    url_of: &dyn Fn(&str) -> Option<String>,
) -> Result<String, String> {
    const MARKER: &str = "asset:";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(MARKER) {
        out.push_str(&rest[..at]);
        let after = &rest[at + MARKER.len()..];
        let len = after
            .find(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit() || "._/-".contains(c)))
            .unwrap_or(after.len());
        let path = &after[..len];
        let url = url_of(path).ok_or_else(|| format!("`asset:{path}` names no asset"))?;
        out.push_str(&url);
        rest = &after[len..];
    }
    out.push_str(rest);
    Ok(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn hashed_url_embeds_a_content_hash() {
        assert_eq!(
            super::hashed_url("css/base.css", b"a"),
            format!(
                "/static/css/base.{}.css",
                &blake3::hash(b"a").to_hex()[..16]
            )
        );
        assert_ne!(
            super::hashed_url("css/base.css", b"a"),
            super::hashed_url("css/base.css", b"b")
        );
        let manifest = super::hashed_url("manifest.webmanifest", b"a");
        assert!(manifest.starts_with("/static/manifest."));
        assert!(manifest.ends_with(".webmanifest"));
    }

    #[test]
    fn references_are_rewritten_or_refused() {
        let url_of = |path: &str| {
            (path == "fonts/a.woff2").then(|| "/static/fonts/a.1111111111111111.woff2".to_owned())
        };
        assert_eq!(
            super::rewrite_references(r#"url("asset:fonts/a.woff2")"#, &url_of),
            Ok(r#"url("/static/fonts/a.1111111111111111.woff2")"#.to_owned())
        );
        let err = super::rewrite_references(r#"url("asset:fonts/missing.woff2")"#, &url_of)
            .expect_err("unknown asset");
        assert!(err.contains("fonts/missing.woff2"), "{err}");
    }

    #[test]
    fn content_types_and_rewriting_follow_the_extension() {
        assert_eq!(super::content_type("fonts/a.woff2"), Some("font/woff2"));
        assert_eq!(super::content_type("x.exe"), None);
        assert!(super::rewrites_references("themes/a.css"));
        assert!(!super::rewrites_references("js/coffer.js"));
    }

    #[test]
    fn const_names_are_derived_from_the_path() {
        assert_eq!(
            super::const_name("fonts/nunito-400-800.woff2"),
            "FONTS_NUNITO_400_800_WOFF2"
        );
    }
}
