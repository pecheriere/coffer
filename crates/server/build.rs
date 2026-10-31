//! Builds the static-asset table: every file under `assets/` gets a
//! content-hashed URL (see `build_support/pipeline.rs`) and a constant in
//! `$OUT_DIR/assets.rs`, which `src/assets.rs` includes.

#[path = "build_support/pipeline.rs"]
mod pipeline;

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

/// A processed asset: its served URL and final body.
struct Built {
    url: String,
    body: Vec<u8>,
}

fn main() -> Result<(), Box<dyn Error>> {
    emit_rerun_directives();
    let out_dir = PathBuf::from(std::env::var("OUT_DIR")?);
    let root = Path::new("assets");

    let mut logicals = Vec::new();
    collect(root, "", &mut logicals)?;
    pipeline::check_names(&logicals)?;

    // Files that name no other asset are hashed first, so a stylesheet or
    // manifest can refer to them by their final URL.
    let mut built: BTreeMap<String, Built> = BTreeMap::new();
    for logical in logicals
        .iter()
        .filter(|logical| !pipeline::rewrites_references(logical))
    {
        let body = fs::read(root.join(logical))?;
        let url = pipeline::hashed_url(logical, &body);
        built.insert(logical.clone(), Built { url, body });
    }
    let plain_urls: BTreeMap<String, String> = built
        .iter()
        .map(|(logical, asset)| (logical.clone(), asset.url.clone()))
        .collect();
    for logical in logicals
        .iter()
        .filter(|logical| pipeline::rewrites_references(logical))
    {
        let text = fs::read_to_string(root.join(logical))
            .map_err(|err| format!("assets/{logical}: {err}"))?;
        let text = pipeline::rewrite_references(&text, &|path| plain_urls.get(path).cloned())
            .map_err(|err| {
                format!(
                    "assets/{logical}: {err} (a reference to another stylesheet or manifest \
                     is not allowed)"
                )
            })?;
        let body = text.into_bytes();
        let url = pipeline::hashed_url(logical, &body);
        built.insert(logical.clone(), Built { url, body });
    }

    let mut table = String::new();
    let mut names = Vec::new();
    for (logical, asset) in &built {
        let target = out_dir.join("assets").join(logical);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&target, &asset.body)?;
        let name = pipeline::const_name(logical);
        let content_type = pipeline::content_type(logical)
            .ok_or_else(|| format!("assets/{logical}: no content type"))?;
        writeln!(
            table,
            "pub const {name}: Asset = Asset {{ url: {url:?}, content_type: {content_type:?}, \
             body: include_bytes!(concat!(env!(\"OUT_DIR\"), \"/assets/{logical}\")) }};",
            url = asset.url,
        )?;
        names.push(name);
    }
    writeln!(table, "pub const ALL: &[Asset] = &[{}];", names.join(", "))?;
    fs::write(out_dir.join("assets.rs"), table)?;
    Ok(())
}

fn emit_rerun_directives() {
    println!("cargo::rerun-if-changed=assets");
    println!("cargo::rerun-if-changed=build_support");
}

/// Lists the logical path (`css/base.css`) of every file under `dir`, in
/// sorted order, skipping names that start with `.`.
fn collect(dir: &Path, prefix: &str, found: &mut Vec<String>) -> Result<(), Box<dyn Error>> {
    let mut entries = fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let name = entry
            .file_name()
            .into_string()
            .map_err(|name| format!("assets: a file name is not UTF-8: {}", name.display()))?;
        if name.starts_with('.') {
            continue;
        }
        let logical = format!("{prefix}{name}");
        if entry.file_type()?.is_dir() {
            collect(&entry.path(), &format!("{logical}/"), found)?;
        } else {
            found.push(logical);
        }
    }
    Ok(())
}
