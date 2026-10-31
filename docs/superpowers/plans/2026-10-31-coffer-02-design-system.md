# coffer plan 02 — Design system and theming Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every coffer page restyled in the approved "Mochi" direction with Nugget the hamster, on a theme mechanism where a theme is one token file, chosen per household and per member, with self-hosted fonts and content-hashed static assets, all under the existing strict CSP.

**Architecture:** A build script serves every file under `crates/server/assets/` at a content-hashed URL and generates one Rust constant per file. A theme is `assets/themes/<name>.css`: `@font-face` rules plus one `:root` block of custom properties, with colours written as `light-dark(<light>, <dark>)`. One theme-agnostic component stylesheet reads only those tokens. A registry in `coffer_server::themes` names each theme, its stylesheet constant and its illustrations. The server renders the chosen theme's `<link>` and, when the member forced light or dark, `data-scheme` on `<html>`; the `color-scheme` property then selects the `light-dark()` arm, so no JavaScript is involved. The household default theme and each member's theme and scheme are stored, changed through an audited `Appearance` service, and resolved on every page, including error pages.

**Tech Stack:** Rust 1.99 (edition 2024), axum 0.8, maud 0.27, sqlx 0.9 (SQLite), blake3 1.8 (now also a build-dependency, same version, already in `Cargo.lock`). No new crates. Asset tooling, outside the build: `uv` running fonttools 4.67.0 with brotli 1.2.1, and resvg 0.48.1.

**Spec:** `docs/superpowers/specs/2026-10-30-coffer-design.md` (§5 constraints, §6.3 role matrix "Household settings", §6.4 pages, §6.6 audit, §6.14 dev practices) and the owner's decision on issue #1 (direction Mochi, mascot Nugget, themes as data, household default plus member override, the Mochi negative-colour fix).

**Owner decisions (rulings of 2026-10-31):**
- D1: Mochi negatives are indigo, `#4B5BB5` light and `#9DB4FF` dark (Task 3 explains why).
- D2: Broadsheet is deferred. IBM Plex declares the Reserved Font Name "Plex", so a subset could not keep the name. Fjord proves the mechanism instead (Task 8).
- D3: Fjord keeps Nugget. Per-theme illustrations stay possible through `Theme.illustrations`.
- D4: this plan executes first and owns migration `0002_appearance.sql`, `Action::ManageHousehold` and `/admin/household`. Plan 2a renumbers its migrations after it and reuses that page and action.
- D5: the `light-dark()` browser floor (Safari 17.5, Chrome 123, Firefox 120) is accepted.

## Global Constraints

- `rust-toolchain.toml` `channel = "1.99"`; workspace `edition = "2024"`, `rust-version = "1.99"`.
- Workspace lints: `unsafe_code = "forbid"`; clippy `all` + `pedantic` at deny; `unwrap_used`, `expect_used`, `print_stdout`, `print_stderr`, `todo`, `dbg_macro` denied. No `#[allow]`/`#[expect]` without `reason = "..."`. The build script is linted too.
- No dead code, no TODO comments. Tests specify behaviour; none restate the implementation or exist for coverage.
- `cargo deny check` stays clean with `deny.toml` unchanged. This plan adds no crate. If one ever looks necessary, it needs a version published at least 7 days ago (`python3 -I .github/scripts/crate_age.py --base main`) and a licence already allowed by `deny.toml`.
- Build artifacts go to the configured `CARGO_TARGET_DIR` (external SSD); never set or override it.
- Full gate, all green before each commit: `cargo fmt --check && cargo clippy --workspace --all-targets --all-features --locked -- -D warnings && cargo test --workspace --all-features --locked && RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked && cargo deny check`.
- CSP stays exactly `default-src 'self'; script-src 'self'; style-src 'self'; frame-ancestors 'none'; form-action 'self'; base-uri 'none'`. No inline `<script>`, no `style=` attribute, no `<style>` element, no external URL, no `data:` URI in markup, CSS or illustrations.
- Components read only tokens. `assets/css/base.css` contains no colour literal and no font name. A theme file contains only `@font-face` rules and one `:root` block.
- WCAG AA: 4.5:1 for every declared text pair (large text is held to 4.5:1 too, since no pair is reserved for large text) and 3:1 for UI boundaries and the focus ring. Every interactive element shows a visible `:focus-visible` outline. Motion runs only under `prefers-reduced-motion: no-preference`.
- Hashed static URLs are served `Cache-Control: public, max-age=31536000, immutable`. HTML stays `no-store`. A URL whose hash does not match the current content is 404, never the current content.
- Fonts are SIL OFL 1.1, self-hosted as woff2, with each family's `OFL.txt` committed next to it and served. Never subset a font that declares a Reserved Font Name. A font declares one in its copyright header, the lines before `This Font Software is licensed under the SIL Open Font License, Version 1.1.`; every OFL text also defines the term further down, and that definition does not count. The Python tool and the Rust test apply this same header rule.
- Mutations go through a `StoreTx` together with their audit entry; handlers authorize through `coffer_domain::policy::authorize`.
- The repository is public and generic: nothing about the owner's hosts, network or households.
- Commits: conventional-commit subject, no `Co-Authored-By`, `Claude-Session` or "Generated with" trailers.

## Review Focus

1. **A stored theme name that is no longer registered** (a theme renamed or removed in a later release, or a hand-edited row): pages render with the household default, or with Mochi if that name is unknown too. The account page still loads and shows "Household default" selected. → Task 4 `unknown_stored_theme_falls_back`, `invalid_theme_values_are_refused_by_the_schema`, `unparseable_stored_theme_reads_as_none` and `member_with_a_corrupt_theme_still_signs_in`; Task 7 `account_page_survives_a_retired_theme`.
2. **A browser holding a hashed URL from before an upgrade**: the old URL is 404, never today's file under a year-long immutable cache. Changed content always changes the URL. → Task 1 `stale_or_unknown_static_paths_are_404` and `hashed_url_embeds_a_content_hash`.
3. **Forged appearance form values** (`theme="x\"><script>"`, an unregistered name, a 10,000-character string, `color_scheme=purple`): 422, nothing stored, no audit event, and never echoed into `<link href>` or `data-scheme`. → Task 7 `appearance_rejects_unknown_values`.
4. **An error page requested with a stale, garbage or disabled member's session cookie**: renders the themed error page with the household theme. No 500, no redirect loop. → Task 6 `error_page_with_a_stale_cookie_still_renders`.
5. **A 360 px viewport with the longest real values** (a 50-character agent token shown once, a ~100-character invite link, a 64-character display name, the members page action row): no horizontal scrolling and every control at least 44 px tall. → Task 8 Step 7 (manual browser check at 360 px, light and dark).

---

### Task 1: Content-hashed static assets

**Files:**
- Create: `crates/server/build.rs`, `crates/server/build_support/pipeline.rs`, `crates/server/src/assets.rs`, `crates/server/src/http/static_files.rs`, `crates/server/src/testkit/browser.rs`, `crates/server/tests/assets.rs`
- Move: `crates/server/src/http/pages/coffer.js` → `crates/server/assets/js/coffer.js` (unchanged); `crates/server/src/http/pages/coffer.css` → `crates/server/assets/css/base.css` (unchanged here; rewritten in Task 5)
- Modify: `crates/server/Cargo.toml` (`[build-dependencies] blake3.workspace = true`; `tower` and `http-body-util` become optional dependencies enabled by `testkit`), `crates/server/src/lib.rs` (`pub mod assets;`), `crates/server/src/testkit.rs` → `crates/server/src/testkit/mod.rs` (adds `pub mod browser;`), `crates/server/src/http/mod.rs` (merge `static_files::router()`; the `security_headers` layer moves here), `crates/server/src/http/pages/mod.rs` (drop `SCRIPT`, `STYLE` and the `/static/coffer.*` routes; `security_headers` becomes `pub(super)` and leaves `pages::router()`), `crates/server/src/http/pages/layout.rs` (link the generated URLs), `crates/server/tests/pages.rs` (use `testkit::browser`; drop `static_assets_are_served`)

**Interfaces:**
- Produces (`build_support/pipeline.rs`, compiled into `build.rs` with `#[path = "build_support/pipeline.rs"] mod pipeline;` and into the library's tests with `#[cfg(test)] #[path = "../build_support/pipeline.rs"] mod pipeline;` in `src/assets.rs`):
  - `pub fn content_type(logical: &str) -> Option<&'static str>`: `css` → `text/css; charset=utf-8`, `js` → `text/javascript; charset=utf-8`, `woff2` → `font/woff2`, `svg` → `image/svg+xml`, `png` → `image/png`, `ico` → `image/x-icon`, `webmanifest` → `application/manifest+json`, `txt` → `text/plain; charset=utf-8`; anything else `None`.
  - `pub fn rewrites_references(logical: &str) -> bool`: true for `.css` and `.webmanifest`.
  - `pub fn hashed_url(logical: &str, body: &[u8]) -> String`: `/static/<dir>/<stem>.<h>.<ext>`, where `<h>` is the first 16 lowercase hex characters of `blake3::hash(body)` and `<dir>/` is omitted for top-level files.
  - `pub fn const_name(logical: &str) -> String`: ASCII uppercase, every non-alphanumeric character becomes `_` (`fonts/nunito-400-800.woff2` → `FONTS_NUNITO_400_800_WOFF2`).
  - `pub fn rewrite_references(text: &str, url_of: &dyn Fn(&str) -> Option<String>) -> Result<String, String>`: replaces each `asset:<path>` (path characters `[a-z0-9._/-]`) with `url_of(path)`. An unknown path is `Err` naming it.
- Produces (`build.rs`): walks `assets/` recursively in sorted order and skips every file or directory whose name starts with `.` (`.DS_Store`, editor swap files). Any other file without a content type is a build error whose message names the file and says to remove it or add its extension to `pipeline::content_type`, and so are two files with the same `const_name`. Files that don't rewrite references are hashed first, then CSS and manifests are rewritten (a reference from one rewritable file to another is a build error) and hashed. Every processed body is written to `$OUT_DIR/assets/<logical>`, and `$OUT_DIR/assets.rs` gets one `pub const <NAME>: Asset = Asset { url, content_type, body: include_bytes!(concat!(env!("OUT_DIR"), "/assets/<logical>")) };` per file plus `pub const ALL: &[Asset] = &[...];`. It emits `cargo::rerun-if-changed=assets` and `cargo::rerun-if-changed=build_support`. `main` returns `Result<(), Box<dyn std::error::Error>>`. The function that prints the directives carries `#[expect(clippy::print_stdout, reason = "cargo reads build-script directives from stdout")]`.
- Produces (`src/assets.rs`): `#[derive(Debug, Clone, Copy)] pub struct Asset { pub url: &'static str, pub content_type: &'static str, pub body: &'static [u8] }`, then `include!(concat!(env!("OUT_DIR"), "/assets.rs"));`. Code refers to an asset only by its constant (`assets::CSS_BASE_CSS.url`), never by a literal URL.
- Produces (`http::router` layer order, innermost first): routes → `require_same_origin` → `map_response(pages::security_headers)` → `api::problem_bodies`. Static files, health and API responses get the CSP, `nosniff` and `Referrer-Policy` too; `no-store` stays limited to `text/html`. Task 6 inserts one layer into this order.
- Produces (`http/static_files.rs`): `pub(super) fn router() -> Router<AppState>` with `GET /static/{*path}`: look up `"/static/" + path` in `assets::ALL`; 200 with `Content-Type` and `Cache-Control: public, max-age=31536000, immutable`, otherwise a bare 404. HEAD comes from axum's `get`.
- Produces (`testkit::browser`): `Browser` and `Reply`, moved from `tests/pages.rs` and made `pub`. The module is library code under the `testkit` feature, not `cfg(test)`, so the move adjusts it: `unwrap()` becomes `expect("…")` under a module-level `#![expect(clippy::expect_used, reason = "test harness: a setup step that fails must abort the test that called it")]`, and the public functions get `# Panics` docs and `#[must_use]` where clippy pedantic asks (`Browser::new(&TestServer)`, `get`, `post_form`, `post_form_from`, `post_json`, `post_json_with`, `send`, `enroll`, `login`; `Reply { status, headers, body }` with `json`, `header`, `csrf`, `between`), plus `pub async fn with_admin() -> (TestServer, Browser, SoftAuthenticator)`. Tasks 2 to 8 use these.

- [ ] **Step 1: Write the failing tests.** In `build_support/pipeline.rs` (`#[cfg(test)] mod tests`):
  - `hashed_url_embeds_a_content_hash`: `hashed_url("css/base.css", b"a") == format!("/static/css/base.{}.css", &blake3::hash(b"a").to_hex()[..16])`; `b"b"` gives a different URL; `hashed_url("manifest.webmanifest", b"a")` starts with `/static/manifest.` and ends with `.webmanifest`.
  - `references_are_rewritten_or_refused`: with `url_of` knowing only `fonts/a.woff2` → `/static/fonts/a.1111111111111111.woff2`, `url("asset:fonts/a.woff2")` becomes `url("/static/fonts/a.1111111111111111.woff2")`, and `url("asset:fonts/missing.woff2")` is `Err` containing `fonts/missing.woff2`.
  - `content_types_and_rewriting_follow_the_extension`: `content_type("fonts/a.woff2") == Some("font/woff2")`, `content_type("x.exe") == None`, `rewrites_references("themes/a.css")`, `!rewrites_references("js/coffer.js")`.
  - `const_names_are_derived_from_the_path`: the `FONTS_NUNITO_400_800_WOFF2` example above.

  In `src/assets.rs`: `every_url_matches_its_body`. For each of `ALL`, `url == pipeline::hashed_url(<logical recovered from url>, body)` (strip `/static/` and the `.<16 hex>` segment), and `content_type` agrees with `pipeline::content_type`. No CSS or manifest body contains `asset:`, and every `/static/` URL inside one is the `url` of an asset in `ALL`.

  In `tests/assets.rs`:
  - `hashed_assets_are_served_immutable`: for every asset in `ALL`, GET its URL gives 200, the exact `Content-Type`, `Cache-Control: public, max-age=31536000, immutable`, `X-Content-Type-Options: nosniff`, and a body equal to `asset.body`.
  - `stale_or_unknown_static_paths_are_404`: `/static/js/coffer.0000000000000000.js`, `/static/coffer.js`, `/static/coffer.css`, `/static/../Cargo.toml` and `/static/` are all 404.
  - `pages_reference_only_served_assets`: on `/login`, every `href="/static/…"` and `src="/static/…"` is in `ALL` and fetches with 200.
- [ ] **Step 2: Run** `cargo test -p coffer-server --all-features` → FAIL (no `pipeline`, no `assets`, old routes).
- [ ] **Step 3: Implement** the files above. `layout::page` links `assets::CSS_BASE_CSS.url` and loads `assets::JS_COFFER_JS.url` with `defer`.
- [ ] **Step 4: Run** the full gate → PASS. `cargo tree -p coffer-server -e build --depth 1` shows `blake3` as the only build-dependency, at the version already in `Cargo.lock`; `git diff Cargo.lock` is empty.
- [ ] **Step 5: Commit** `feat(server): serve static assets at content-hashed urls`.

### Task 2: Icons, favicon and web manifest

**Files:**
- Create: `tools/regenerate_assets.py` (subcommand `icons` here, `fonts` in Task 3), `tools/test_regenerate_assets.py`, `tools/ruff.toml`, `tools/icons/app-icon.svg`, `tools/icons/maskable.svg`
- Create (from the tool): `crates/server/assets/icons/favicon.svg`, `favicon.ico`, `apple-touch-icon.png`, `icon-192.png`, `icon-512.png`, `icon-maskable-512.png`; by hand: `crates/server/assets/manifest.webmanifest`
- Modify: `crates/server/src/http/static_files.rs` (`GET /favicon.ico`), `crates/server/src/http/pages/layout.rs` (head links), `.github/workflows/ci.yml` (crate-age job lint step also covers `tools`), `CONTRIBUTING.md` ("Static assets" section), `crates/server/tests/assets.rs`

**Interfaces:**
- Consumes: Task 1 `assets::ALL`, generated constants, `testkit::browser`.
- Produces: `GET /favicon.ico` → `assets::ICONS_FAVICON_ICO` with `Cache-Control: public, max-age=86400` (the URL is unhashed, so the cache is short).
- Produces (`tools/regenerate_assets.py`, standard library only): `def ico_bytes(pngs: list[tuple[int, bytes]]) -> bytes` (ICONDIR `reserved=0, type=1, count`, then one 16-byte ICONDIRENTRY per PNG: width and height bytes, `0` meaning 256, colour count 0, planes 1, bpp 32, size, offset, followed by the PNG bytes in order). `icons` checks that `resvg --version` prints `0.48.1`, then renders `favicon.svg` at 16, 32 and 48 px into `favicon.ico`; `tools/icons/app-icon.svg` at 180 (`apple-touch-icon.png`), 192 and 512; and `tools/icons/maskable.svg` at 512 (`icon-maskable-512.png`).

- [ ] **Step 1: Source files** (exact copy).
  - `crates/server/assets/icons/favicon.svg`: `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32">` + the **glyph** fragment (Appendix A) + `</svg>`.
  - `tools/icons/app-icon.svg`: `<svg xmlns="http://www.w3.org/2000/svg" viewBox="14 24 132 132"><rect x="14" y="24" width="132" height="132" fill="#FBE4EC"/>` + the **welcome** fragment + `</svg>`.
  - `tools/icons/maskable.svg`: the same, with `viewBox="-8.5 1.5 177 177"` and the rect at `x="-8.5" y="1.5" width="177" height="177"`. The centre is (80, 90) and the maskable safe zone is a circle of radius 0.4 × 177 = 70.8. The farthest point of Nugget, the outer edge of an ear (centre (46, 46), r 15), is 70.6 from the centre, so all of Nugget stays inside.
  - `crates/server/assets/manifest.webmanifest`:
    ```json
    {
      "name": "coffer",
      "short_name": "coffer",
      "start_url": "/",
      "scope": "/",
      "display": "standalone",
      "background_color": "#F3F6F1",
      "theme_color": "#C2386A",
      "icons": [
        { "src": "asset:icons/icon-192.png", "sizes": "192x192", "type": "image/png", "purpose": "any" },
        { "src": "asset:icons/icon-512.png", "sizes": "512x512", "type": "image/png", "purpose": "any" },
        { "src": "asset:icons/icon-maskable-512.png", "sizes": "512x512", "type": "image/png", "purpose": "maskable" }
      ]
    }
    ```
- [ ] **Step 2: Write the failing tests.** `tools/test_regenerate_assets.py` (unittest): `test_ico_header_and_directory` (three fake PNGs of 16, 32 and 48 px: bytes 0..6 are `00 00 01 00 03 00`; each entry's width byte, size and offset match, and offsets follow one another). In `tests/assets.rs`:
  - `favicon_is_served_at_the_root`: `/favicon.ico` gives 200, `image/x-icon`, `public, max-age=86400`, a body equal to `ICONS_FAVICON_ICO.body` and starting with `[0, 0, 1, 0]`.
  - `manifest_lists_served_icons`: parse `MANIFEST_WEBMANIFEST.body` as JSON. `start_url == "/"`, and each `icons[].src` fetches with 200 and `image/png`.
  - Extend `pages_reference_only_served_assets`: `/login` has `rel="icon"` with `type="image/svg+xml"` → `ICONS_FAVICON_SVG.url`, `rel="icon"` → `/favicon.ico` with `sizes="48x48"`, `rel="apple-touch-icon"` → `ICONS_APPLE_TOUCH_ICON_PNG.url`, and `rel="manifest"` → `MANIFEST_WEBMANIFEST.url`.
- [ ] **Step 3: Run** `python3 -I -m unittest discover -s tools -p 'test_*.py'` and `cargo test -p coffer-server --all-features --test assets` → FAIL.
- [ ] **Step 4: Implement** the tool and the route. Generate the PNGs and the ICO with `cargo install resvg --version 0.48.1 --locked` (a release more than 7 days old), then `python3 -I tools/regenerate_assets.py icons`. `tools/ruff.toml` holds `extend = "../.github/ruff.toml"`, and its comment says the inherited `S603`/`S607`/`T201` ignores hold here because the tool runs a fixed argv of `uvx`/`resvg` from PATH and prints progress. Any other ignore needs its own one-line justification. In ci.yml's crate-age lint step, run `ruff check .github/scripts tools`, `ruff format --check .github/scripts tools` and `python3 -I -m unittest discover -s tools -p 'test_*.py'` next to the existing commands. Add a "Static assets" section to CONTRIBUTING.md: files under `crates/server/assets` are served at hashed URLs; fonts and icons come from `tools/regenerate_assets.py` (needs `uv` and `resvg 0.48.1`) and are never hand-edited.
- [ ] **Step 5: Run** the tool tests, `ruff check tools` and `ruff format --check tools` (with the pinned ruff from `.github/requirements-lint.txt`), and the full gate → PASS. Then `GH_TOKEN=$(gh auth token) uvx zizmor .` → no findings.
- [ ] **Step 6: Commit** `feat(server): nugget favicon, touch icon and web manifest`.

### Task 3: Theme contract, registry, Mochi tokens and fonts

**Files:**
- Create: `crates/server/src/themes/mod.rs`, `themes/illustrations.rs`, `themes/contract.rs` (`#[cfg(test)]`), `themes/tests.rs` (`#[cfg(test)]`), `themes/nugget/welcome.svg`, `themes/nugget/resting.svg`, `themes/nugget/glyph.svg` (Appendix A)
- Create: `crates/server/assets/themes/mochi.css` (exact copy below)
- Create (from the tool): `crates/server/assets/fonts/fredoka-600.woff2`, `nunito-400-800.woff2`, `ofl-fredoka.txt`, `ofl-nunito.txt`
- Modify: `tools/regenerate_assets.py` (`fonts` subcommand, `verified`, `declares_reserved_font_name`), `tools/test_regenerate_assets.py`, `crates/server/src/lib.rs` (`pub mod themes;`), `crates/domain/src/identity.rs`, `crates/server/src/http/pages/layout.rs` (links the default theme before `base.css`), `AGENTS.md`, `README.md`

**Interfaces:**
- Produces (`coffer_domain::identity`):
  - `#[derive(Debug, Clone, PartialEq, Eq, Hash)] ThemeName(String)`: `ThemeName::parse(&str) -> Result<ThemeName, IdentityError>`, 1..=32 characters of `[a-z0-9-]`; `as_str()`; `Display`.
  - `#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)] enum ColorScheme { #[default] System, Light, Dark }`: `as_str()` → `"system" | "light" | "dark"`; `FromStr`; serde lowercase.
  - New `IdentityError` variants: `InvalidThemeName` ("theme name must be 1 to 32 characters of a-z, 0-9 or '-'"), `UnknownColorScheme` ("unknown colour scheme"), `UnknownTheme` ("unknown theme").
- Produces (`coffer_server::themes`):
  - `#[derive(Debug)] pub struct Theme { pub name: &'static str, pub label: &'static str, pub stylesheet: Asset, pub illustrations: &'static Illustrations }`
  - `pub const THEMES: &[Theme]` (the registry: `[MOCHI]` here; Task 8 adds Fjord); `pub const DEFAULT: &Theme` (Mochi); `pub fn find(name: &str) -> Option<&'static Theme>`. Themes are compared by `name` (tests assert `look.theme.name == DEFAULT.name`); `Theme` has no `PartialEq`.
  - `#[derive(Debug, Clone, Copy)] pub struct Look { pub theme: &'static Theme, pub scheme: ColorScheme }` with `impl Default` (`DEFAULT`, `System`).
  - `illustrations.rs`: `#[derive(Debug)] pub struct Illustration { pub view_box: &'static str, pub body: &'static str }`; `#[derive(Debug)] pub struct Illustrations { pub welcome: Illustration, pub empty: Illustration, pub error: Illustration, pub glyph: Illustration }`; `pub const NUGGET: Illustrations`. `welcome` = `nugget/welcome.svg`, `empty` and `error` = `nugget/resting.svg` (`view_box` `0 0 160 160`), `glyph` = `nugget/glyph.svg` (`0 0 32 32`), all via `include_str!`. A theme overrides the mascot by pointing `illustrations` at its own `Illustrations`.
- Produces (`themes/contract.rs`, test-only; Task 5 adds `declarations` for the base-stylesheet tests and reuses the token lists):
  - `pub(crate) enum Scheme { Light, Dark }`; `pub(crate) struct Tokens` (token name without `--` → raw value).
  - `pub(crate) fn parse_theme(css: &str) -> Result<Tokens, String>`: strips comments. `Err` on any top-level rule other than `:root` and `@font-face`, on a second `:root`, on a duplicate token, or on unbalanced braces.
  - `Tokens::resolve(&self, name: &str, scheme: Scheme) -> Result<String, String>`: substitutes `var(--x)` recursively (a cycle is `Err`) and picks the matching `light-dark(a, b)` arm (split at the top-level comma, so `rgba(…)` arms survive). `Tokens::references(&self) -> Vec<String>` lists every `var()` name used.
  - `pub(crate) fn contrast_ratio(a: Rgb, b: Rgb) -> f64`, using WCAG 2.x relative luminance with the sRGB linearisation threshold 0.04045; `pub(crate) fn oklab_distance(a: Rgb, b: Rgb) -> f64` (Euclidean distance in Björn Ottosson's OKLab); `Rgb::parse(&str) -> Option<Rgb>` for `#RRGGBB`.
  - `pub(crate) const COLOUR_TOKENS: &[&str]`, values that must resolve to a colour (`#RRGGBB`, `rgba(…)` or `transparent`) in both schemes: `bg surface text muted accent on-accent accent-soft positive negative warning warn-soft border control-border shadow card-bg card-border-color input-bg input-border-color button-primary-bg button-primary-fg button-secondary-bg button-secondary-fg button-secondary-border nav-bg nav-border-color nav-active-bg nav-active-fg`.
  - `pub(crate) const OTHER_TOKENS: &[&str]`, any non-empty value: `font-head font-body font-num font-mono weight-head weight-button tracking-head style-display radius radius-sm radius-btn radius-chip icon-stroke card-border-width card-shadow input-border-width button-primary-shadow nav-active-shadow illustration-animation`.
  - `pub(crate) enum Min { Body /* 4.5 */, Ui /* 3.0 */ }` and `pub(crate) const PAIRS: &[(&str, &str, Min)]` (foreground, background). `Body`: text/bg, text/surface, muted/bg, muted/surface, accent/bg, accent/surface, on-accent/accent, positive/bg, positive/surface, positive/card-bg, negative/bg, negative/surface, warning/surface, warning/warn-soft, text/warn-soft, text/accent-soft, muted/accent-soft, text/card-bg, muted/card-bg, accent/card-bg, text/input-bg, button-primary-fg/button-primary-bg, button-secondary-fg/button-secondary-bg, text/nav-bg, muted/nav-bg, nav-active-fg/nav-active-bg. `Ui`: control-border/surface, control-border/bg, input-border-color/card-bg, accent/nav-bg.
  - `pub(crate) const NEGATIVE_ACCENT_MIN_DISTANCE: f64 = 0.15`.

**The Mochi negative fix.** The mockup's `#B3410C` / `#FFA071` sit 0.113 / 0.111 from the raspberry accent in OKLab, and only 0.038 under tritan simulation. The warm hues between raspberry (hue 3°) and the ochre warning (80°) leave no colour clearly apart from both, so negatives move to indigo. **Light `#4B5BB5`** (6.06:1 on surface, 5.56:1 on bg, 0.234 from the accent) and **dark `#9DB4FF`** (7.84:1, 8.80:1, 0.176). They stay apart under protan, deutan and tritan simulation (minimum 0.105), and negatives always carry a minus sign as well. **Control borders:** the mockup's `--border` (`#E3E7E0` on white, 1.2:1) fails the 3:1 boundary rule for inputs, so `control-border` is added: light `#898790` (3.54 on surface, 3.25 on bg), dark `#746F7B` (3.24, 3.64). `--border` stays for decorative separators.

`crates/server/assets/themes/mochi.css` (exact):

```css
/* Mochi: soft and friendly. Pistachio paper, raspberry accent, round shapes.
   A theme is tokens only (see AGENTS.md, "Themes and assets"). */
@font-face {
  font-family: "Fredoka";
  font-style: normal;
  font-weight: 600;
  font-display: swap;
  src: url("asset:fonts/fredoka-600.woff2") format("woff2");
  unicode-range: U+0000-024F, U+0259, U+02BB-02BC, U+02C6, U+02DA, U+02DC, U+0300-0304, U+0308, U+0329, U+1E00-1EFF, U+2000-206F, U+20A0-20C0, U+2113, U+2122, U+2190-2193, U+2212, U+2215, U+FEFF, U+FFFD;
}
@font-face {
  font-family: "Nunito";
  font-style: normal;
  font-weight: 400 800;
  font-display: swap;
  src: url("asset:fonts/nunito-400-800.woff2") format("woff2");
  unicode-range: U+0000-024F, U+0259, U+02BB-02BC, U+02C6, U+02DA, U+02DC, U+0300-0304, U+0308, U+0329, U+1E00-1EFF, U+2000-206F, U+20A0-20C0, U+2113, U+2122, U+2190-2193, U+2212, U+2215, U+FEFF, U+FFFD;
}
:root {
  --bg: light-dark(#F3F6F1, #19171E);
  --surface: light-dark(#FFFFFF, #24212B);
  --text: light-dark(#2A2833, #F3EFF5);
  --muted: light-dark(#605D69, #ABA5B3);
  --accent: light-dark(#C2386A, #FF8FB6);
  --on-accent: light-dark(#FFFFFF, #2E0C1B);
  --accent-soft: light-dark(#FBE4EC, #3D2230);
  --accent-press: light-dark(#8F2049, #C2386A);
  --positive: light-dark(#2A7653, #82D8AB);
  --negative: light-dark(#4B5BB5, #9DB4FF);
  --warning: light-dark(#835C00, #F2C75E);
  --warn-soft: light-dark(#FDF2D3, #3A3018);
  --border: light-dark(#E3E7E0, #353140);
  --control-border: light-dark(#898790, #746F7B);
  --shadow: light-dark(rgba(194, 56, 106, .22), rgba(0, 0, 0, .5));

  --font-head: "Fredoka", ui-rounded, "SF Pro Rounded", system-ui, sans-serif;
  --font-body: "Nunito", ui-rounded, "SF Pro Rounded", system-ui, -apple-system, "Segoe UI", sans-serif;
  --font-num: "Nunito", ui-rounded, system-ui, sans-serif;
  --font-mono: ui-monospace, "SF Mono", Menlo, Consolas, monospace;
  --weight-head: 600;
  --weight-button: 800;
  --tracking-head: 0;
  --style-display: normal;

  --radius: 26px;
  --radius-sm: 16px;
  --radius-btn: 999px;
  --radius-chip: 999px;
  --icon-stroke: 2.1;

  --card-bg: var(--surface);
  --card-border-width: 0;
  --card-border-color: var(--border);
  --card-shadow: 0 1px 2px rgba(0, 0, 0, .04), 0 14px 32px -20px var(--shadow);
  --input-bg: var(--bg);
  --input-border-width: 2px;
  --input-border-color: var(--control-border);
  --button-primary-bg: var(--accent);
  --button-primary-fg: var(--on-accent);
  --button-primary-shadow: 0 3px 0 var(--accent-press);
  --button-secondary-bg: var(--accent-soft);
  --button-secondary-fg: var(--text);
  --button-secondary-border: transparent;
  --nav-bg: var(--surface);
  --nav-border-color: var(--border);
  --nav-active-bg: var(--accent);
  --nav-active-fg: var(--on-accent);
  --nav-active-shadow: none;
  --illustration-animation: bob 3.6s ease-in-out infinite;
}
```

`--accent-press` is private to Mochi. A theme may define extra tokens, as long as every `var()` it uses is defined in the same file.

- [ ] **Step 1: Write the failing tests** in `themes/tests.rs`:
  - `contrast_maths_matches_wcag_references`: `#000000`/`#FFFFFF` → 21.0 ± 0.01; `#767676`/`#FFFFFF` → 4.54 ± 0.01; a colour against itself → 1.0.
  - `oklab_matches_the_reference`: the OKLab distance from `#FFFFFF` to `#000000` is 1.0 ± 1e-3, and `#FF0000` sits at (0.62796, 0.22486, 0.12585) ± 1e-3. Expose `Rgb::oklab(self) -> [f64; 3]` for this test.
  - `resolver_follows_var_and_light_dark`: `:root{--a: light-dark(#111111, #EEEEEE); --b: var(--a); --c: light-dark(rgba(0, 0, 0, .5), #000000)}` → `b` is `#111111` (Light) and `#EEEEEE` (Dark), `c` is `rgba(0, 0, 0, .5)` (Light); `:root{--x: var(--y); --y: var(--x)}` → `Err`.
  - `a_theme_file_holds_only_tokens_and_fonts`: `:root{--a:#000000} .btn{color:red}` → `Err`; two `:root` blocks → `Err`.
  - `every_theme_defines_every_token_in_both_schemes`: for each theme in `THEMES`, parse `std::str::from_utf8(theme.stylesheet.body)`. Every `COLOUR_TOKENS` entry resolves to a colour in Light and Dark, every `OTHER_TOKENS` entry is present and non-empty, and every `references()` name is defined. A failure names theme, token and scheme.
  - `every_theme_meets_wcag_aa`: every `PAIRS` entry in both schemes resolves to `#RRGGBB` and its ratio is ≥ the minimum. A failure lists every miss as `theme scheme fg/bg = ratio`.
  - `negative_amounts_stand_apart_from_the_accent`: `oklab_distance(negative, accent) >= NEGATIVE_ACCENT_MIN_DISTANCE` in both schemes for every theme. The mockup's `#B3410C`/`#C2386A` must fail it: add that pair as a second assertion.
  - `every_theme_file_is_registered`: the set of `ALL` URLs under `/static/themes/` equals the set of `THEMES[].stylesheet.url`. Names are unique and `ThemeName::parse` accepts each one; `DEFAULT.name == "mochi"`.
  - `illustrations_are_inert_svg_fragments`: for every theme's four illustrations, the body contains none of `<script`, `style`, `href`, `javascript:`, `<foreignObject`, ` on` followed by a letter and `=`, or `<svg`. Fragments only: the wrapper is rendered by the layout.
  - `every_font_ships_with_its_licence`: for each `/static/fonts/<family>-*.woff2` in `ALL`, a `/static/fonts/ofl-<family>.<hash>.txt` exists and its body starts with `Copyright` and contains `SIL Open Font License, Version 1.1`, and its copyright header does not contain `Reserved Font Name`. The header is every line before `This Font Software is licensed under the SIL Open Font License, Version 1.1.`; a licence without that line fails the test. The licence's own definition of the term, further down, is ignored.
  - `wcag_check_reports_a_broken_theme`: copy Mochi's stylesheet text, replace `--text: light-dark(#2A2833` with `--text: light-dark(#F3F6F1`, and parse it. The pair checker used by `every_theme_meets_wcag_aa` (`fn wcag_failures(name: &str, tokens: &Tokens) -> Vec<String>` in `themes/tests.rs`) reports at least `text/bg` in Light. This proves the AA test is not vacuous.

  In `crates/domain/src/identity.rs`: `theme_name_validates` (`"mochi"` and `"a-1"` accepted; `""`, 33 characters, `"Mochi"` and `"a b"` rejected) and `color_scheme_roundtrips` (`"dark".parse()` → `Dark`, `Dark.as_str() == "dark"`, `"purple"` → `Err(UnknownColorScheme)`).
- [ ] **Step 2: Run** `cargo test -p coffer-domain && cargo test -p coffer-server --all-features themes` and `python3 -I -m unittest discover -s tools -p 'test_*.py'` → FAIL.
- [ ] **Step 3: Fonts.** Add the `fonts` subcommand. Add `def verified(path: Path, sha256: str) -> Path` (raises `SystemExit` naming the file on a mismatch) and `def declares_reserved_font_name(licence: str) -> bool` (true when the copyright header contains `Reserved Font Name`; the header is the lines before `This Font Software is licensed under the SIL Open Font License, Version 1.1.`, and a licence without that line also counts as true, since it can't be checked). Test them first in `tools/test_regenerate_assets.py`: `test_checksum_mismatch_is_refused` (a temp file with the wrong digest → `SystemExit`); `test_plain_ofl_has_no_reserved_name` (a copyright line, the licensed-under line, then the OFL definitions paragraph that mentions `"Reserved Font Name" refers to…` → false); `test_reserved_name_in_header_is_found` (`Copyright © 2017 IBM Corp. with Reserved Font Name "Plex"` before the licensed-under line → true). The subcommand downloads each source from `https://raw.githubusercontent.com/google/fonts/<commit>/ofl/<dir>/<file>` into a fresh temporary directory, runs `verified()` on it, then, with `uvx --from fonttools==4.67.0 --with brotli==1.2.1` and `SOURCE_DATE_EPOCH=0` in the environment (fontTools then writes a fixed `head.modified`, so rerunning on unchanged inputs leaves `git diff` empty), runs `fonttools varLib.instancer <src> <axes> -o <tmp>.ttf` and `pyftsubset <tmp>.ttf --unicodes=<the unicode-range above, comma-joined without spaces> --layout-features=kern,liga,calt,tnum,lnum,case,ccmp,locl,mark,mkmk --flavor=woff2 --name-IDs=* --name-languages=* --output-file=crates/server/assets/fonts/<out>`. The tool runs a fixed argv with no shell, so those arguments carry no quotes. It refuses (`SystemExit`) a licence for which `declares_reserved_font_name` is true, before any subsetting, and copies each verified `OFL.txt` to `crates/server/assets/fonts/ofl-<family>.txt`. Before committing, run the subcommand twice and confirm that the second run changes nothing. Sources:

  | Family | Commit | File | sha256 | Axes | Output |
  |---|---|---|---|---|---|
  | Fredoka | `a60a77e14f28abd4ef243a1b5dfc48df0cec5205` | `fredoka/Fredoka[wdth,wght].ttf` | `2ba02e68b152868aef9ba28e24b3648c7d457fe6f25c761f2c2c53fb61a73fc8` | `wdth=100 wght=600` | `fredoka-600.woff2` |
  | Fredoka licence | same | `fredoka/OFL.txt` | `5c9e7eee5c6b25f4b05b8d53b2e470ea4962f9ced742d044a98f7d95d1375bab` | | `ofl-fredoka.txt` |
  | Nunito | `8b0a1d0f5983c89bc2b93f1b5fb55f9e252744b5` | `nunito/Nunito[wght].ttf` | `bb55a5ca5c2042335b3991af27c4d0705d0ef41cac6164ac737fd8f2a1e85207` | `wght=400:800` | `nunito-400-800.woff2` |
  | Nunito licence | same | `nunito/OFL.txt` | `580df76c95a1ec5ab878ceb25bb3d85c6a076804e9c970c8c6972aea775fdf65` | | `ofl-nunito.txt` |

  Expected sizes are about 17 KB (Fredoka) and 55 KB (Nunito). Nunito has no `tnum` feature, but its default figures all have the same 600-unit advance, so amounts line up.
- [ ] **Step 4: Implement** the domain types, `themes/`, `contract.rs` and `mochi.css`, and link `DEFAULT.stylesheet.url` before `base.css` in the layout. AGENTS.md gets a "Themes and assets" section (exact):
  ```markdown
  ## Themes and assets

  - Static files live in `crates/server/assets/`. `build.rs` serves each at a
    content-hashed URL (`/static/<dir>/<name>.<hash>.<ext>`, cached for a year)
    and rewrites `asset:<path>` references in CSS and the web manifest. Rust
    code names an asset by its generated constant (`assets::CSS_BASE_CSS`).
  - A theme is one token file, `assets/themes/<name>.css`: `@font-face` rules
    and one `:root` block, colours as `light-dark(<light>, <dark>)`.
    Components (`assets/css/base.css`) read only tokens: no colour literal or
    font name outside a theme file.
  - Adding a theme: the token file (plus its fonts and `ofl-<family>.txt` under
    `assets/fonts/`), then one `Theme` entry in `crates/server/src/themes/mod.rs`.
    The theme tests check every token in both schemes and WCAG AA contrast.
  - Fonts and icons come from `tools/regenerate_assets.py` (pinned sources and
    tools); never edit them by hand. Fonts whose copyright header declares a
    Reserved Font Name are not subset.
  - The web manifest's `theme_color` and `background_color` are Mochi's
    values for every theme: the manifest is static and cached by the device.
  ```
  README gets a short "Fonts" section: coffer bundles Fredoka and Nunito under the SIL Open Font License 1.1; the licences are in `crates/server/assets/fonts/`.
- [ ] **Step 5: Run** the full gate → PASS.
- [ ] **Step 6: Commit** `feat(server): theme contract and the mochi theme`.

### Task 4: Appearance persistence and service

**Files:**
- Create: `crates/store/migrations/0002_appearance.sql`, `crates/server/src/appearance.rs`, `crates/server/tests/appearance.rs`
- Modify: `crates/domain/src/policy.rs`, `crates/store/Cargo.toml` (feature `testkit`, enabled by `coffer-server`'s `testkit`), `crates/store/src/members.rs`, `crates/store/src/lib.rs` (new `household` module and exports), create `crates/store/src/household.rs`, `crates/store/tests/identity.rs`, `crates/server/src/lib.rs` (`pub mod appearance;`), `crates/server/src/http/mod.rs` (`AppState.appearance: Arc<Appearance>`), `crates/server/src/cli.rs` and `crates/server/src/testkit/mod.rs` (build it; `TestServer.appearance`; `corrupt_member_theme`), `crates/server/Cargo.toml` (`testkit` also enables `coffer-store/testkit`)

**Interfaces:**
- Consumes: `ThemeName`, `ColorScheme`, `IdentityError::UnknownTheme`, `themes::{find, DEFAULT, Look, Theme}`.
- Schema (`0002_appearance.sql`):
  ```sql
  ALTER TABLE members ADD COLUMN theme TEXT
      CHECK (theme IS NULL OR (length(theme) BETWEEN 1 AND 32 AND theme NOT GLOB '*[^a-z0-9-]*'));
  ALTER TABLE members ADD COLUMN color_scheme TEXT NOT NULL DEFAULT 'system'
      CHECK (color_scheme IN ('system', 'light', 'dark'));

  CREATE TABLE household_appearance (
      id    INTEGER PRIMARY KEY CHECK (id = 1),
      theme TEXT NOT NULL
          CHECK (length(theme) BETWEEN 1 AND 32 AND theme NOT GLOB '*[^a-z0-9-]*')
  ) STRICT;
  ```
  `members.theme` NULL means "household default". No `household_appearance` row means the registry default. The CHECKs mirror `ThemeName::parse`.
- Reading a stored theme never fails a member lookup. `member_from_row` and `household_theme` map a value that `ThemeName::parse` rejects to `None`. The CHECKs make that unreachable unless constraint checks were disabled, and the store crate has no logging, so the doc comment says why there is no warning. Otherwise one bad row would turn every request by that member, sign-in included, into a 500. An unknown but well-formed name is the service's concern (`look` falls back).
- Produces (store): `Member` gains `pub theme: Option<ThemeName>, pub color_scheme: ColorScheme` (`NewMember` unchanged; `insert_member` returns `None`/`System`). `StoreTx::set_member_appearance(&mut self, id: MemberId, theme: Option<&ThemeName>, scheme: ColorScheme) -> Result<(), StoreError>` (`NotFound` if no row). `Store::household_theme(&self) -> Result<Option<ThemeName>, StoreError>` and the same on `StoreTx`. `StoreTx::set_household_theme(&mut self, theme: &ThemeName) -> Result<(), StoreError>` (upsert on `id = 1`).
- Produces (policy): `Action::ManageOwnPreferences` → `Scope(Write)`; `Action::ManageHousehold` → `Scope(Admin)` (spec §6.3 "Household settings").
- Produces (`coffer_server::appearance::Appearance`):
  - `pub fn new(store: Store, clock: Arc<dyn Clock>) -> Self`.
  - `pub async fn look(&self, member: Option<&Member>) -> Look`. Never fails: the member's theme if registered, else the household theme if registered, else `DEFAULT`; the scheme is the member's, or `System` without a member. A storage error is logged at `warn` without member data, and resolution falls back.
  - `pub async fn household_theme(&self) -> &'static Theme`, with the same fallback.
  - `pub async fn set_member(&self, actor: &Actor, theme: Option<ThemeName>, scheme: ColorScheme) -> Result<(), AuthError>`: authorize `ManageOwnPreferences`; `Invalid(UnknownTheme)` if `theme` is `Some` and not registered. One `StoreTx` reads the member, writes only on change and audits `member.appearance` (entity `member`, id = member). Changes hold only the fields that changed: `{"theme": {"from": <name|null>, "to": <name|null>}, "color_scheme": {"from": "system", "to": "dark"}}`. A no-op writes nothing.
  - `pub async fn set_household_theme(&self, actor: &Actor, theme: ThemeName) -> Result<(), AuthError>`: authorize `ManageHousehold`; `Invalid(UnknownTheme)` if not registered. One `StoreTx` audits `household.theme` (entity `household`, id `household`) with `{"from": <name|null>, "to": <name>}`. A no-op writes nothing.

- [ ] **Step 1: Write the failing tests.** `crates/store/tests/identity.rs`:
  - `member_appearance_defaults_and_updates`: a new member is `None`/`System`; after `set_member_appearance(Some(mochi), Dark)` and commit, `get_member` returns them; on an unknown id it is `NotFound`.
  - `household_theme_upserts`: `None` at first, `Some("a")` after the first set and `Some("b")` after the second.
  - `invalid_theme_values_are_refused_by_the_schema` (unit test in `crates/store/src/members.rs`, which can reach the pool, like `audit_is_append_only`): raw `UPDATE members SET theme = 'Mochi'`, `'a b'` and 33 × `'a'`, and `INSERT INTO household_appearance (id, theme) VALUES (1, 'X')`, all return an error; `'mochi'` and `'a-1'` succeed.
  - `unparseable_stored_theme_reads_as_none` (same module): on one connection, `PRAGMA ignore_check_constraints = ON`, write `theme = 'Mochi'` for a member, then `PRAGMA ignore_check_constraints = OFF`. `get_member` and `list_members` succeed with `theme == None`; the same holds for the household row through `household_theme`.

  `crates/server/tests/appearance.rs` (service, with `TestServer` and enrolled members):
  - `new_member_sees_the_household_default`: `look(Some(&bob))` → `DEFAULT`, `System`; `look(None)` → `DEFAULT`.
  - `member_choice_is_stored_and_audited`: `set_member(bob, None, Dark)` → stored; exactly one `member.appearance` event, actor bob, changes `{"color_scheme": {"from": "system", "to": "dark"}}`. Repeating the call adds no event.
  - `unregistered_theme_is_rejected`: `ThemeName::parse("no-such-theme")` → `AuthError::Invalid(IdentityError::UnknownTheme)`; nothing stored, no event.
  - `household_theme_requires_admin`: a member session gets `Denied`; an admin setting `mochi` → event `household.theme` with `{"from": null, "to": "mochi"}`.
  - `read_token_cannot_change_preferences`: an `Actor` via `Agent { scope: Read }` gets `Denied`.
  - `member_with_a_corrupt_theme_still_signs_in` (page level, in `tests/appearance.rs`): after the raw write above, made through a helper `TestServer::corrupt_member_theme(&self, member: MemberId)` added to `testkit` (it uses the store's test-only hook `Store::execute_unchecked_for_tests(&self, sql: &str, bind: &str)`, behind the store's own `testkit` feature), `Browser::login` for that member returns 200, and `/account` renders.
  - `unknown_stored_theme_falls_back`: with `retired` stored through the store API for both the household and bob, `look(Some(&bob))` is `DEFAULT`. With the household set to `mochi` and bob still on `retired`, it is Mochi.
  
  In `policy.rs`: `preferences_need_write_and_household_needs_admin` (Member+Session may `ManageOwnPreferences` but not `ManageHousehold`; Admin+Agent(Write) may not `ManageHousehold`).
- [ ] **Step 2: Run** `cargo test --workspace --all-features` → FAIL.
- [ ] **Step 3: Implement.** Extend the `columns!` macro and `member_from_row` (use `text_opt` + `ThemeName::parse`, and `parsed` for the scheme). The audit helper follows the pattern of `auth::identity::audit`.
- [ ] **Step 4: Run** the full gate → PASS. Run the `security-review` skill on the diff (policy change) and fix the findings.
- [ ] **Step 5: Commit** `feat: household and member appearance settings`.

### Task 5: Component stylesheet and every page restyled

**Files:**
- Modify: `crates/server/assets/css/base.css` (replaced, exact copy below), `crates/server/src/themes/contract.rs` (`declarations`), `crates/server/src/http/pages/layout.rs`, `pages/mod.rs` (`PageSession` loads the member), `pages/login.rs`, `pages/setup.rs`, `pages/account.rs`, `pages/admin.rs`, `crates/server/src/themes/tests.rs`, `crates/server/tests/pages.rs`

**Interfaces:**
- Consumes: `Look`, `Appearance::look`, `Illustrations`, generated asset constants, `contract::{COLOUR_TOKENS, OTHER_TOKENS}`.
- Produces (`themes/contract.rs`, test-only): `pub(crate) fn declarations(css: &str) -> Vec<Declaration>` with `pub(crate) struct Declaration { context: String /* enclosing @media prelude, or "" */, property: String, value: String /* trimmed, with any trailing `!important` removed */ }`.
- Produces (`pages::PageSession`): also holds `member: Member`, loaded in `from_request_parts`. `pub(super) async fn look(&self, state: &AppState) -> Look` = `state.appearance.look(Some(&self.member))`. `account::viewer` becomes `pub fn viewer(state: &AppState, session: &PageSession) -> Viewer` (no second member query).
- Produces (`layout.rs`):
  - `pub enum Nav { Account, Members, Invites }` (Task 7 adds `Household`).
  - `pub struct Frame<'a> { pub look: Look, pub viewer: Option<&'a Viewer>, pub current: Option<Nav>, pub narrow: bool }` (`narrow` adds `page-narrow` to `main`; login, setup and error pages set it).
  - `pub fn page(frame: &Frame<'_>, title: &str, content: &Markup) -> Markup`. The head holds, in order: `meta charset`; `meta viewport` (`width=device-width, initial-scale=1`); `meta name="color-scheme"` (`light dark`, `light` or `dark`); `title` (`<title> · coffer`); the icon, apple-touch-icon and manifest links from Task 2; `link rel=stylesheet` to `look.theme.stylesheet.url`, then `assets::CSS_BASE_CSS.url`; `script defer` from `assets::JS_COFFER_JS.url`. `<html lang="en">` carries `data-scheme="light|dark"` only when `look.scheme` is not `System`. The body holds `a.skip href="#main"` ("Skip to content"), then, with a viewer, `header.topbar` with `a.brand href="/account"` (glyph at 28 px plus "coffer"), `nav.nav aria-label="Main"` with `aria-current="page"` on `current`, `span.who` (display name) and the sign-out form (`button.btn`). Then `main#main.page` (plus `.page-narrow` when `frame.narrow`).
  - `pub fn auth_card(look: Look, heading: &str, content: &Markup) -> Markup`: `section.card.welcome` with the welcome illustration (96 px, slot `welcome`), `p.wordmark` "coffer", `h1`, then `content`.
  - `pub fn empty_state(look: Look, message: &str) -> Markup`: `div.empty` with the empty illustration (72 px) and `p`.
  - `pub fn illustration(art: &Illustration, size: u32, slot: &str) -> Markup`: `svg class="illustration illustration-<slot>" viewBox=… width=… height=… aria-hidden="true" focusable="false"` wrapping `PreEscaped(art.body)` (a compile-time constant).
  - `pub fn key_icon() -> Markup`: `svg.icon viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"` containing `<circle cx="8" cy="15" r="4.5"/><path d="M11.2 11.8L20 3M16.5 6.5l2.5 2.5M14 9l2 2"/>`.
  - `once`, `csrf_field` keep their signatures. `error_page(title, detail)` renders with `Look::default()` until Task 6.

`crates/server/assets/css/base.css` (exact):

```css
/* coffer components. Read theme tokens only: no colour literal, no font name. */
*, *::before, *::after { box-sizing: border-box; }
:root { color-scheme: light dark; }
:root[data-scheme="light"] { color-scheme: light; }
:root[data-scheme="dark"] { color-scheme: dark; }
html { -webkit-text-size-adjust: 100%; text-size-adjust: 100%; }
body { margin: 0; background: var(--bg); color: var(--text); font: 500 16px/1.55 var(--font-body); overflow-wrap: anywhere; }
h1, h2, h3 { font-family: var(--font-head); font-weight: var(--weight-head); letter-spacing: var(--tracking-head); line-height: 1.1; margin: 0 0 .5em; }
h1 { font-size: clamp(1.75rem, 6vw, 2.25rem); font-style: var(--style-display); }
h2 { font-size: 1.375rem; }
h3 { font-size: 1.0625rem; margin-top: 1.25rem; }
p { margin: 0 0 .75em; }
a { color: var(--accent); text-underline-offset: 3px; }
code { font: 500 .9em/1.4 var(--font-mono); overflow-wrap: anywhere; }
:focus-visible { outline: 3px solid var(--accent); outline-offset: 2px; }
.skip { position: absolute; left: 1rem; top: -4rem; padding: .6rem .9rem; border-radius: var(--radius-sm); background: var(--text); color: var(--bg); z-index: 1; }
.skip:focus { top: .75rem; }
.icon { stroke-width: var(--icon-stroke); flex: none; }

.topbar { display: flex; flex-wrap: wrap; align-items: center; gap: .5rem 1rem; padding: .75rem 1rem; background: var(--nav-bg); border-bottom: 1px solid var(--nav-border-color); }
.brand { display: inline-flex; align-items: center; gap: .5rem; font: var(--weight-head) 1.25rem/1 var(--font-head); color: var(--text); text-decoration: none; }
.who { margin-left: auto; display: flex; flex-wrap: wrap; align-items: center; gap: .5rem; color: var(--muted); font-weight: 700; font-size: .875rem; }
.who form { margin: 0; }
.nav { order: 3; flex-basis: 100%; display: flex; gap: .25rem; overflow-x: auto; }
.nav a { display: inline-flex; align-items: center; min-height: 44px; padding: 0 .9rem; border-radius: var(--radius-btn); color: var(--muted); font-weight: 700; text-decoration: none; white-space: nowrap; }
.nav a[aria-current="page"] { background: var(--nav-active-bg); color: var(--nav-active-fg); box-shadow: var(--nav-active-shadow); text-decoration: underline; text-decoration-thickness: 2px; text-underline-offset: 6px; }

.page { max-width: 52rem; margin: 0 auto; padding: 1.25rem 1rem 3rem; }
.page-narrow { max-width: 28rem; }
.card { background: var(--card-bg); border-style: solid; border-width: var(--card-border-width); border-color: var(--card-border-color); border-radius: var(--radius); box-shadow: var(--card-shadow); padding: 1.25rem; margin: 0 0 1.25rem; }
.card > h2:first-child { margin-top: 0; }

.items { list-style: none; margin: 0 0 1rem; padding: 0; }
.item { display: flex; flex-wrap: wrap; align-items: center; gap: .5rem 1rem; padding: .875rem 0; border-top: 1px solid var(--border); }
.item:first-child { border-top: 0; }
.item-main { flex: 1 1 14rem; min-width: 0; }
.item-title { font-weight: 700; }
.item-meta { display: block; color: var(--muted); font-size: .8125rem; }
.item-actions { display: flex; flex-wrap: wrap; gap: .5rem; }
.item-actions form { display: flex; flex-wrap: wrap; gap: .5rem; margin: 0; }
.chip { display: inline-flex; align-items: center; gap: .3rem; font: var(--weight-button) .78rem/1 var(--font-body); padding: .4rem .6rem; border-radius: var(--radius-chip); background: var(--accent-soft); color: var(--text); white-space: nowrap; }
.chip-quiet { background: transparent; border: 1px dashed var(--muted); color: var(--muted); }

button, .btn { display: inline-flex; align-items: center; justify-content: center; gap: .5rem; min-height: 44px; padding: 0 1rem; font: var(--weight-button) .9375rem/1.1 var(--font-body); border-radius: var(--radius-btn); border: 1px solid var(--button-secondary-border); background: var(--button-secondary-bg); color: var(--button-secondary-fg); cursor: pointer; text-decoration: none; }
.btn-primary { background: var(--button-primary-bg); color: var(--button-primary-fg); border-color: var(--button-primary-bg); box-shadow: var(--button-primary-shadow); }
.btn-primary:active { box-shadow: none; transform: translateY(2px); }
.btn-block { width: 100%; }
button:hover, .btn:hover { filter: brightness(1.06); }
button:disabled { opacity: .6; cursor: progress; }

.field { display: flex; flex-direction: column; gap: .375rem; margin: 0 0 1rem; font-size: .8125rem; font-weight: 700; color: var(--muted); }
.input { font: 500 1rem/1.2 var(--font-body); color: var(--text); background: var(--input-bg); border-style: solid; border-width: var(--input-border-width); border-color: var(--input-border-color); border-radius: var(--radius-sm); padding: 0 .75rem; min-height: 44px; width: 100%; }
.input:focus-visible { outline-offset: 1px; }
.choices { border: 0; margin: 0 0 1rem; padding: 0; }
.choices legend { font-size: .8125rem; font-weight: 700; color: var(--muted); margin-bottom: .375rem; padding: 0; }
.choice { display: flex; align-items: center; gap: .6rem; min-height: 44px; font-weight: 600; color: var(--text); }
.choice input { accent-color: var(--accent); width: 1.25rem; height: 1.25rem; margin: 0; }

#result { min-height: 1.5em; font-weight: 700; }
#result.ok { color: var(--positive); }
#result.error { color: var(--warning); background: var(--warn-soft); padding: .6rem .8rem; border-radius: var(--radius-sm); }
.once { background: var(--warn-soft); color: var(--text); border-radius: var(--radius-sm); padding: 1rem; margin: 0 0 1.25rem; }
.once code { display: block; margin: .5rem 0; padding: .6rem .75rem; background: var(--surface); border-radius: var(--radius-sm); }
.empty { display: flex; flex-direction: column; align-items: center; gap: .5rem; padding: 1rem 0; text-align: center; color: var(--muted); }
.illustration { display: block; flex: none; }
.welcome { text-align: center; }
.welcome .illustration { margin: 0 auto .5rem; }
.welcome .field, .welcome #result { text-align: left; }
.wordmark { font: var(--weight-head) .95rem/1 var(--font-head); letter-spacing: .02em; color: var(--muted); margin: 0 0 .5rem; }
.fine { font-size: .8125rem; color: var(--muted); }

@media (min-width: 40rem) {
  .topbar { padding: .75rem 1.5rem; }
  .nav { order: 0; flex-basis: auto; }
  .page { padding: 2rem 1.5rem 4rem; }
  .card { padding: 1.5rem 1.75rem; }
}
@keyframes bob { 50% { transform: translateY(-5px); } }
@media (prefers-reduced-motion: no-preference) {
  .illustration-welcome { animation: var(--illustration-animation); }
}
@media (prefers-reduced-motion: reduce) {
  *, *::before, *::after { animation: none !important; transition: none !important; scroll-behavior: auto !important; }
}
```

Page content (the copy is fixed here; element ids that `coffer.js` and the tests rely on are unchanged):
- **Login:** `auth_card(look, "Welcome home", …)` holding `label.field` "Username" with `input#username.input` (attributes unchanged), `button#login.btn.btn-primary.btn-block` = `key_icon()` + "Sign in with a passkey", `p#result role=status`, `p.fine` "No passwords. Face ID, Touch ID, Windows Hello or a security key."
- **Setup / invite / re-enroll:** `auth_card(look, "Create your passkey", …)`. The intro is "Choose how you sign in to coffer." (new member) or "Add a new passkey to your existing account." (re-enroll), followed by the existing fields as `label.field` + `.input`, `button#enroll.btn.btn-primary.btn-block` = `key_icon()` + "Create passkey", and `#result`.
- **Account** (`current: Account`): `h1` "Account"; the `once` box; `#result`; then cards. `h2` "Passkeys": `ul.items`, one `li.item` per passkey with `.item-title` label, `.item-meta` "Added <created_at> · Last used <last_used_at|never>", and a Remove form (`button.btn`); then the new-passkey `label.field` and `button#add-passkey.btn` (unchanged `data-csrf`). `h2` "Agent tokens": items with `.item-title` name, chips for scope and status (`chip-quiet` when revoked or expired), `.item-meta` with `code` prefix "…" and "Expires <date|never>", and a Revoke form; or `empty_state(look, "No agent tokens yet. Create one below to let an agent use coffer on your behalf.")`. `h3` "Create a token": the existing form with `label.field`/`.input` and `button.btn.btn-primary`.
- **Members** (`current: Members`): `h1` "Members"; the `once` box; card `h2` "People", one item per member with `.item-title` username, `.item-meta` display name, role and status chips, and in `.item-actions` the role `select.input` with `aria-label="Role for <username>"` + "Set role", "Re-enroll link" and "Disable" forms. Card `h2` "Agent tokens": the existing columns as item title, chips and meta, or `empty_state(look, "No agent tokens in this household.")`.
- **Invites** (`current: Invites`): `h1` "Invites"; the `once` box; card `h2` "Invite someone" (existing form restyled); card `h2` "Pending invites": items, or `empty_state(look, "No pending invites.")`.

- [ ] **Step 1: Write the failing tests.** In `themes/tests.rs`, using `contract::declarations(CSS_BASE_CSS.body)`:
  - `components_read_only_tokens`: every `var(--x)` names a `COLOUR_TOKENS`/`OTHER_TOKENS` entry. No value contains `#` followed by a hex digit, `rgb(`, `rgba(`, `hsl(`, `hwb(`, `lab(`, `lch(`, `oklch(`, `color(` or `light-dark(`. `font`/`font-family` values contain no `"`. For colour-bearing properties (`color`, `background`, `background-color`, `border`, `border-top`, `border-bottom`, `border-color`, `outline`, `box-shadow`, `accent-color`), the value with `var(…)` calls removed holds only numbers, lengths, `,`, `!important` and the keywords `transparent`, `currentColor`, `none`, `inherit`, `solid`, `dashed`, `inset`.
  - `motion_runs_only_when_allowed`: every `animation` declaration whose `value` (already stripped of `!important`) is not `none` has context `@media (prefers-reduced-motion: no-preference)`. The reduced-motion rule's `animation: none !important` therefore passes.

  In `tests/pages.rs`:
  - `every_page_is_themed_and_inline_free`: for `/login`, a bootstrap `/setup/…` link, a new-member `/invite/…` link, `/account`, `/admin/members` and `/admin/invites` (signed-in admin where needed), the response is 200. The body contains `href="<DEFAULT.stylesheet.url>"`, then `href="<CSS_BASE_CSS.url>"`, `meta name="color-scheme" content="light dark"`, `href="#main"` and `<main id="main"`, and contains neither `style=` nor `<style` nor `<script>` (an inline opening tag without `src`).
  - `sign_in_page_greets_with_the_mascot`: `/login` contains `illustration-welcome`, `id="login"` and "Sign in with a passkey".
  - `empty_states_show_the_resting_mascot`: a fresh admin's `/account` contains "No agent tokens yet" and `illustration-empty`; `/admin/invites` contains "No pending invites."
  - `every_form_control_has_an_accessible_name`: on the same pages, every `<select` and every `<input` whose `type` is not `hidden` has an accessible name, through one of:
    - an `aria-label`;
    - an `id` matched by a `for="<id>"` on a `<label`;
    - an enclosing label: the last `<label` before the control has no `</label>` between it and the control.
    The failure message names the page and the control's `name` or `id`. On `/admin/members`, the role select carries `aria-label="Role for bob"`.
  - `nav_marks_the_current_page`: `/account` has `aria-current="page"` on the Account link only; `/admin/invites` on Invites only.
- [ ] **Step 2: Run** `cargo test -p coffer-server --all-features` → FAIL.
- [ ] **Step 3: Implement** the stylesheet, layout components and pages. Existing tests in `tests/pages.rs` keep their assertions, with one exception: `members_page_manages_members_and_reenroll_links` slices the page at `"<td>bob</td>"`, which no longer exists. Change that locator to `"class=\"item-title\">bob<"`, so the slice starts at bob's item and the following `between("action=\"/admin/members/", "/role\"")` still finds bob's id. No other assertion changes. `full_journey` locates `id="new-token"` and `id="new-link"`, so keep `once` rendering `code id=…`.
- [ ] **Step 4: Run** the full gate → PASS. Then serve locally (`COFFER_PUBLIC_URL=http://localhost:8080 COFFER_DATA_DIR=$(mktemp -d) cargo run -p coffer-server -- serve`), open every page in light and dark (the browser's emulated `prefers-color-scheme`) and confirm that the browser console reports no CSP violation.
- [ ] **Step 5: Commit** `feat(server): mochi components and restyled pages`.

### Task 6: Themed error and not-found pages

**Files:**
- Modify: `crates/server/src/http/mod.rs` (fallback and layer order), `crates/server/src/http/pages/mod.rs` (`PageError::into_response`, `ErrorPage`, `render_error_pages`, `fallback`), `crates/server/src/http/pages/layout.rs` (`error_body`), `crates/server/tests/pages.rs`

**Interfaces:**
- Consumes: `Appearance::look`, `Sessions::resolve`, `session_cookie_value`, `layout::{page, illustration}`.
- Produces:
  - `#[derive(Debug, Clone)] pub(super) struct ErrorPage { title: String, detail: Option<String> }`. `PageError::into_response` keeps the 401 → `/login` redirect, and otherwise returns `(status, Extension(ErrorPage { … }))` with an empty body.
  - `pub(super) async fn render_error_pages(State(state): State<AppState>, request: Request, next: Next) -> Response`. It takes the session cookie value before `next.run`. If the response carries an `ErrorPage`, it resolves the look: the cookie → `sessions.resolve` → `store.get_member` → `appearance.look(Some(&member))`; any failure → `appearance.look(None)`. It then renders `page(&Frame { look, viewer: None, current: None, narrow: true }, &title, &layout::error_body(look, &title, detail.as_deref()))`, keeping status and headers and setting `Content-Type: text/html; charset=utf-8`.
  - `pub fn error_body(look: Look, title: &str, detail: Option<&str>) -> Markup`: `section.card.welcome` with the error illustration (120 px, slot `error`), `h1` title, `p` detail, `a.btn.btn-primary href="/account"` "Back to coffer".
  - `pub(super) async fn fallback(uri: Uri) -> Response`: a path of `/api` or under `/api/` → bare `StatusCode::NOT_FOUND` (`api::problem_bodies` turns it into a problem); anything else → `PageError::not_found()`.
  - Router layers, innermost first: routes and `.fallback(pages::fallback)` → `require_same_origin` → `render_error_pages` (new, inserted into Task 1's order) → `map_response(security_headers)` → `api::problem_bodies`. The same-origin rejection is a `PageError` too, so it gets the themed page. Delete the temporary `layout::error_page`.
  - Resolving the cookie on an error response goes through `Sessions::resolve`, which may slide the session's idle expiry (at most one write a minute), exactly as any page load does. That is accepted and documented in the function's doc comment.

- [ ] **Step 1: Write the failing tests** in `tests/pages.rs`:
  - `unknown_page_renders_a_themed_not_found`: `GET /nope` → 404, `text/html`, `no-store`, the CSP header as in `security_headers_present`, the body contains `DEFAULT.stylesheet.url`, `illustration-error`, `Not found` and `href="/account"`. `HEAD /nope` → 404.
  - `error_page_with_a_stale_cookie_still_renders`: browsers carrying `coffer_session=garbage`, or the cookie of a member disabled since, get the themed 404 for `GET /nope` (not a redirect, not 500). `GET /account` with the same cookies still redirects to `/login`.
  - `conflict_pages_are_themed`: the existing last-passkey removal (409) body contains `last passkey`, the theme link and `illustration-error`.
  - `unknown_api_route_problem_json` in `tests/api.rs` must stay green unchanged. Run it explicitly.
- [ ] **Step 2: Run** `cargo test -p coffer-server --all-features --test pages --test api` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the full gate → PASS. Run the `security-review` skill on the diff (it reads session cookies on error paths) and fix the findings.
- [ ] **Step 5: Commit** `feat(server): themed error and not-found pages`.

### Task 7: Choosing a theme and a colour scheme

**Files:**
- Create: `crates/server/src/http/pages/household.rs`
- Modify: `crates/server/src/http/pages/account.rs` (Appearance card, `POST /account/appearance`), `pages/mod.rs` (merge `household::routes()`), `pages/layout.rs` (`Nav::Household`; admins see the link), `crates/server/tests/appearance.rs`

**Interfaces:**
- Consumes: `Appearance::{set_member, set_household_theme, household_theme}`, `THEMES`, `PageForm`, `PageSession::verify`.
- Produces:
  - Account page card `h2` "Appearance" holds `form method=post action="/account/appearance"` with `csrf_field`. It has `fieldset.choices` legend "Theme": `label.choice` + `input type=radio name="theme"`, value `""` labelled "Household default (<household theme label>)", then one radio per `THEMES` entry (value `name`, label `label`). `checked` goes to the member's stored theme when it is registered, and to the household default radio otherwise. A second `fieldset.choices` legend "Light or dark" holds `name="color_scheme"` radios: `system` "Match this device", `light` "Light", `dark` "Dark". Then `button.btn.btn-primary` "Save appearance".
  - `POST /account/appearance` form `{ csrf, theme, color_scheme }`: `verify` CSRF; empty `theme` → `None`, otherwise `ThemeName::parse`; `ColorScheme::from_str`; `set_member`; parse errors → `AuthError::Invalid` (422); success → 303 `/account`.
  - `GET /admin/household` (`current: Household`; `authorize(ManageHousehold)`, otherwise the 403 page): `h1` "Household"; card `h2` "Theme", with `p` "The theme everyone sees unless they choose their own on their account page." and a radio per theme, the current household theme checked; `button.btn.btn-primary` "Save household theme". `POST /admin/household` `{ csrf, theme }` → `set_household_theme` → 303 `/admin/household`.

- [ ] **Step 1: Write the failing tests** in `tests/appearance.rs` (page section, `testkit::browser`):
  - `account_page_offers_every_theme_and_scheme`: `/account` has `name="theme" value=""` checked, `value="mochi"`, and `system` checked, `light`, `dark`.
  - `forcing_a_scheme_is_rendered_without_script`: posting `theme=""`, `color_scheme=dark` → 303 `/account`; the reloaded `/account` contains `<html lang="en" data-scheme="dark">` and `content="dark"`; the dark radio is checked. Posting `system` → no `data-scheme`, `content="light dark"`.
  - `appearance_rejects_unknown_values`: `theme="x\"><script>"`, `theme="nope"`, a 10,000-character `theme`, `color_scheme=purple` → 422 `text/html` each. Afterwards bob is still `None`/`System`, there is no `member.appearance` event, and no response body contains `<script>"`. A request without `csrf` → 403.
  - `household_page_is_admin_only`: bob gets 403 for `GET` and `POST /admin/household`. An admin gets 200 with `value="mochi"` checked; posting `theme=mochi` → 303 and a `household.theme` event (actor = admin, `{"from": null, "to": "mochi"}`); the admin nav shows the Household link and bob's does not.
  - `household_form_rejects_forged_values`: as admin, `POST /admin/household` with `theme="x\"><script>"`, `theme=nope` and an empty `theme` → 422 each; without `csrf` → 403. Afterwards `household_theme()` is still `None`, there is no `household.theme` event, and no response body contains `<script>"`.
  - `account_page_survives_a_retired_theme`: store `retired` for bob through the store API; `/account` → 200 with the household default radio checked and the Mochi stylesheet linked.
- [ ] **Step 2: Run** `cargo test -p coffer-server --all-features --test appearance` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the full gate → PASS. Security review of the new form routes (CSRF, policy, no echo of input) with the `security-review` skill; fix the findings.
- [ ] **Step 5: Commit** `feat(server): household and member theme choice`.

### Task 8: Fjord, the second theme, proves the mechanism

Adding Fjord touches no template, component or route: one token file, its font and one registry entry. Broadsheet is deferred by owner ruling D2 (IBM Plex's Reserved Font Name).

**Files:**
- Create: `crates/server/assets/themes/fjord.css` (exact copy below); from the tool: `crates/server/assets/fonts/manrope-400-800.woff2`, `ofl-manrope.txt`
- Modify: `tools/regenerate_assets.py` (Manrope source), `crates/server/src/themes/mod.rs` (`FJORD` in `THEMES`, after Mochi), `crates/server/tests/appearance.rs`, `README.md` (Fonts: add Manrope)

**Interfaces:**
- Consumes: everything above, unchanged. `FJORD = Theme { name: "fjord", label: "Fjord", stylesheet: assets::THEMES_FJORD_CSS, illustrations: &NUGGET }`.

Manrope source: commit `b31870aff700ab7a1d74fa0c6887d95beb9e0037`, `manrope/Manrope[wght].ttf` sha256 `3ae11c49db0455a3cc33e37d380f20fdb8c7f8b41dc07625c177e3d87a9d6ae6`, axes `wght=400:800`, output `manrope-400-800.woff2` (about 35 KB); `manrope/OFL.txt` sha256 `58172e0c0fac2cda8a37b348164bb55e44b0e69051e557e92b1d3f6910141f7b` → `ofl-manrope.txt`.

`crates/server/assets/themes/fjord.css` (exact; colours from the approved palette, plus a `control-border` derived the same way as Mochi's):

```css
/* Fjord: calm and Scandinavian. Cool air, slate blue, generous room. */
@font-face {
  font-family: "Manrope";
  font-style: normal;
  font-weight: 400 800;
  font-display: swap;
  src: url("asset:fonts/manrope-400-800.woff2") format("woff2");
  unicode-range: U+0000-024F, U+0259, U+02BB-02BC, U+02C6, U+02DA, U+02DC, U+0300-0304, U+0308, U+0329, U+1E00-1EFF, U+2000-206F, U+20A0-20C0, U+2113, U+2122, U+2190-2193, U+2212, U+2215, U+FEFF, U+FFFD;
}
:root {
  --bg: light-dark(#F2F5F6, #0F161B);
  --surface: light-dark(#FFFFFF, #172128);
  --text: light-dark(#182730, #E5ECEF);
  --muted: light-dark(#52626C, #93A4AE);
  --accent: light-dark(#2C6480, #86BEDA);
  --on-accent: light-dark(#FFFFFF, #0A1A23);
  --accent-soft: light-dark(#E1EDF2, #1B3341);
  --positive: light-dark(#2A7150, #6FC69A);
  --negative: light-dark(#B23A2F, #F2897D);
  --warning: light-dark(#865700, #E2B451);
  --warn-soft: light-dark(#FBF1DC, #33280F);
  --border: light-dark(#D8E0E4, #26343D);
  --control-border: light-dark(#7D8991, #63717A);
  --shadow: light-dark(rgba(24, 39, 48, .10), rgba(0, 0, 0, .4));

  --font-head: "Manrope", ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto, sans-serif;
  --font-body: "Manrope", ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto, sans-serif;
  --font-num: "Manrope", ui-sans-serif, system-ui, sans-serif;
  --font-mono: ui-monospace, "SF Mono", Menlo, Consolas, monospace;
  --weight-head: 700;
  --weight-button: 700;
  --tracking-head: -.02em;
  --style-display: normal;

  --radius: 14px;
  --radius-sm: 10px;
  --radius-btn: 10px;
  --radius-chip: 8px;
  --icon-stroke: 1.7;

  --card-bg: var(--surface);
  --card-border-width: 1px;
  --card-border-color: var(--border);
  --card-shadow: none;
  --input-bg: var(--bg);
  --input-border-width: 1px;
  --input-border-color: var(--control-border);
  --button-primary-bg: var(--accent);
  --button-primary-fg: var(--on-accent);
  --button-primary-shadow: none;
  --button-secondary-bg: var(--surface);
  --button-secondary-fg: var(--text);
  --button-secondary-border: var(--border);
  --nav-bg: var(--surface);
  --nav-border-color: var(--border);
  --nav-active-bg: var(--accent-soft);
  --nav-active-fg: var(--text);
  --nav-active-shadow: none;
  --illustration-animation: none;
}
```

These values pass every pair; the tightest are control-border on bg at 3.27 (light) and on surface at 3.25 (dark). Negative sits 0.231 / 0.200 from the accent.

- [ ] **Step 1: Write the failing tests** in `tests/appearance.rs`:
  - `household_default_applies_to_everyone_without_a_choice`: the admin sets `fjord` → anonymous `/login` and `/nope` link `THEMES_FJORD_CSS.url` and not Mochi's; bob (no choice) gets Fjord on `/account`, whose default radio reads "Household default (Fjord)".
  - `member_override_beats_the_household_default`: with the household on `fjord`, bob picks `mochi` → bob's `/account` and his `/nope` link Mochi, while the admin's pages link Fjord.
  - `fjord_is_registered_after_the_default` (in `themes/tests.rs`): `THEMES` names are exactly `["mochi", "fjord"]` and `DEFAULT.name == "mochi"`. The contract tests of Task 3 iterate `THEMES`, and `wcag_check_reports_a_broken_theme` proves the checker is not vacuous, so Fjord is checked with no further test.
- [ ] **Step 2: Run** `cargo test -p coffer-server --all-features --test appearance` → FAIL (`fjord` unknown).
- [ ] **Step 3: Implement.** Add Manrope to the tool and run `python3 -I tools/regenerate_assets.py fonts`. Add `fjord.css` and the `FJORD` entry. `git diff --stat` must show no change under `src/http/` or `assets/css/`.
- [ ] **Step 4: Run** the full gate → PASS.
- [ ] **Step 5: Commit** `feat(server): fjord theme`.
- [ ] **Step 6: Whole-branch security review** with the `security-review` skill; fix the findings and commit them.
- [ ] **Step 7: Manual check at 360 px** (Review Focus 5). Serve locally and open every page with the browser at 360 × 740 in light and dark, under Mochi and Fjord, with: a 64-character display name, a token created and shown once, a re-enroll link and an invite link shown once, and three members on the members page. Pass: no horizontal scrollbar on any page; every button and field at least 44 px tall; Tab reaches every control in order with a visible focus ring, starting with "Skip to content"; with `prefers-reduced-motion: reduce` emulated, Nugget on the sign-in page stays still. Then at 1280 px: content centred at 52 rem, the topbar on one row. Record the result in the PR description.

---

## Appendix A: Nugget fragments (exact)

Each is the inner markup of an SVG. Store each file as the elements below concatenated without newlines.

`crates/server/src/themes/nugget/welcome.svg` (viewBox `0 0 160 160`):

```svg
<circle cx="46" cy="46" r="15" fill="#E7A06A"/>
<circle cx="46" cy="47.5" r="8.5" fill="#F6B7B4"/>
<circle cx="114" cy="46" r="15" fill="#E7A06A"/>
<circle cx="114" cy="47.5" r="8.5" fill="#F6B7B4"/>
<ellipse cx="80" cy="94" rx="54" ry="50" fill="#F3BB84"/>
<ellipse cx="80" cy="51" rx="8" ry="4.5" fill="#E7A06A"/>
<ellipse cx="80" cy="111" rx="34" ry="29" fill="#FFEBD3"/>
<ellipse cx="37" cy="101" rx="22" ry="19" fill="#FFEBD3"/>
<ellipse cx="123" cy="101" rx="22" ry="19" fill="#FFEBD3"/>
<ellipse cx="60" cy="143" rx="12" ry="6" fill="#EFA27A"/>
<ellipse cx="100" cy="143" rx="12" ry="6" fill="#EFA27A"/>
<circle cx="60" cy="82" r="8.5" fill="#2B2230"/>
<circle cx="62.7" cy="78.9" r="3.2" fill="#fff"/>
<circle cx="57.1" cy="85.2" r="1.4" fill="#fff"/>
<circle cx="100" cy="82" r="8.5" fill="#2B2230"/>
<circle cx="102.7" cy="78.9" r="3.2" fill="#fff"/>
<circle cx="97.1" cy="85.2" r="1.4" fill="#fff"/>
<ellipse cx="80" cy="95" rx="4.5" ry="3.2" fill="#E9798C"/>
<path d="M75 100q2.5 4 5 0q2.5 4 5 0" fill="none" stroke="#2B2230" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"/>
<ellipse cx="38" cy="102" rx="9" ry="5.5" fill="#F7A1B5" opacity=".75"/>
<ellipse cx="122" cy="102" rx="9" ry="5.5" fill="#F7A1B5" opacity=".75"/>
<circle cx="80" cy="124" r="10" fill="#F6C945"/>
<circle cx="80" cy="124" r="6.6" fill="none" stroke="#DCA22B" stroke-width="1.6"/>
<path d="M75.8 123.2A4.4 4.4 0 0 1 79.4 119.8" fill="none" stroke="#FFF7DA" stroke-width="1.6" stroke-linecap="round"/>
<ellipse cx="69" cy="126.5" rx="7.5" ry="6" fill="#F6CDA5"/>
<ellipse cx="91" cy="126.5" rx="7.5" ry="6" fill="#F6CDA5"/>
```

`crates/server/src/themes/nugget/resting.svg` (viewBox `0 0 160 160`; empty states and errors):

```svg
<circle cx="46" cy="46" r="15" fill="#E7A06A"/>
<circle cx="46" cy="47.5" r="8.5" fill="#F6B7B4"/>
<circle cx="114" cy="46" r="15" fill="#E7A06A"/>
<circle cx="114" cy="47.5" r="8.5" fill="#F6B7B4"/>
<ellipse cx="80" cy="94" rx="54" ry="50" fill="#F3BB84"/>
<ellipse cx="80" cy="51" rx="8" ry="4.5" fill="#E7A06A"/>
<ellipse cx="80" cy="111" rx="34" ry="29" fill="#FFEBD3"/>
<ellipse cx="43" cy="103" rx="14" ry="12" fill="#FFEBD3"/>
<ellipse cx="117" cy="103" rx="14" ry="12" fill="#FFEBD3"/>
<ellipse cx="60" cy="143" rx="12" ry="6" fill="#EFA27A"/>
<ellipse cx="100" cy="143" rx="12" ry="6" fill="#EFA27A"/>
<path d="M51.5 81.2Q60 90.5 68.5 81.2" fill="none" stroke="#2B2230" stroke-width="3.8" stroke-linecap="round"/>
<path d="M91.5 81.2Q100 90.5 108.5 81.2" fill="none" stroke="#2B2230" stroke-width="3.8" stroke-linecap="round"/>
<ellipse cx="80" cy="95" rx="4.5" ry="3.2" fill="#E9798C"/>
<ellipse cx="80" cy="101.5" rx="2" ry="2.5" fill="#2B2230" opacity=".85"/>
<ellipse cx="45" cy="102" rx="7.5" ry="4.5" fill="#F7A1B5" opacity=".75"/>
<ellipse cx="115" cy="102" rx="7.5" ry="4.5" fill="#F7A1B5" opacity=".75"/>
<ellipse cx="73" cy="127" rx="7.5" ry="6" fill="#F6CDA5"/>
<ellipse cx="87" cy="127" rx="7.5" ry="6" fill="#F6CDA5"/>
<text x="120" y="44" font-size="20" font-family="ui-rounded,system-ui,sans-serif" font-weight="800" fill="#8E97AB">z</text>
<text x="134" y="30" font-size="14" font-family="ui-rounded,system-ui,sans-serif" font-weight="800" fill="#8E97AB">z</text>
```

`crates/server/src/themes/nugget/glyph.svg` (viewBox `0 0 32 32`; topbar brand and favicon):

```svg
<circle cx="7" cy="8" r="4.5" fill="#E7A06A"/>
<circle cx="25" cy="8" r="4.5" fill="#E7A06A"/>
<ellipse cx="16" cy="18" rx="14" ry="12.5" fill="#F3BB84"/>
<ellipse cx="16" cy="23.5" rx="8" ry="6.5" fill="#FFEBD3"/>
<ellipse cx="4.8" cy="21" rx="4.6" ry="4.2" fill="#FFEBD3"/>
<ellipse cx="27.2" cy="21" rx="4.6" ry="4.2" fill="#FFEBD3"/>
<circle cx="10.8" cy="15.5" r="2.7" fill="#2B2230"/>
<circle cx="21.2" cy="15.5" r="2.7" fill="#2B2230"/>
<ellipse cx="16" cy="19.6" rx="1.7" ry="1.2" fill="#E9798C"/>
```
