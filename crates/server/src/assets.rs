//! Static assets: every file under `assets/`, served at a URL that carries a
//! hash of its content.
//!
//! `build.rs` generates one constant per file (`CSS_BASE_CSS`) and the list
//! `ALL`. Code names an asset by its constant, never by a literal URL.

/// A static file and the URL it is served at.
#[derive(Debug, Clone, Copy)]
pub struct Asset {
    pub url: &'static str,
    pub content_type: &'static str,
    pub body: &'static [u8],
}

include!(concat!(env!("OUT_DIR"), "/assets.rs"));

#[cfg(test)]
#[path = "../build_support/pipeline.rs"]
mod pipeline;

#[cfg(test)]
mod tests {
    use super::{ALL, pipeline};

    #[test]
    fn every_url_matches_its_body() {
        for asset in ALL {
            let logical = logical_of(asset.url);
            assert_eq!(asset.url, pipeline::hashed_url(&logical, asset.body));
            assert_eq!(
                Some(asset.content_type),
                pipeline::content_type(&logical),
                "{logical}"
            );
            if pipeline::rewrites_references(&logical) {
                let text = std::str::from_utf8(asset.body).expect("UTF-8 text");
                assert!(!text.contains("asset:"), "{logical} keeps a reference");
                for url in static_urls(text) {
                    assert!(ALL.iter().any(|a| a.url == url), "{logical}: {url}");
                }
            }
        }
    }

    /// `/static/css/base.0123456789abcdef.css` -> `css/base.css`.
    fn logical_of(url: &str) -> String {
        let rest = url.strip_prefix("/static/").expect("static url");
        let (stem, ext) = rest.rsplit_once('.').expect("extension");
        let (stem, hash) = stem.rsplit_once('.').expect("hash segment");
        assert_eq!(hash.len(), 16, "{url}");
        assert!(hash.bytes().all(|b| b.is_ascii_hexdigit()), "{url}");
        format!("{stem}.{ext}")
    }

    fn static_urls(text: &str) -> Vec<String> {
        text.match_indices("/static/")
            .map(|(at, _)| {
                text[at..]
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || "/._-".contains(*c))
                    .collect()
            })
            .collect()
    }
}
