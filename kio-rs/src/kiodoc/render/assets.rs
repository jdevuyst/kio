//! Static assets shipped inside the rendered HTML site.
//!
//! `kio doc build --html` writes a self-contained site: no external
//! CDN, no network dependency. The default stylesheet and the
//! sidebar-toggle script are baked into the binary as the two
//! constants here and written to `_assets/` under the output
//! directory.
//!
//! Authors can override the look by placing their own
//! `docs/_assets/` directory in the markdown source tree — the
//! renderer copies a present source `_assets/` over the defaults
//! (see [`super`]). The default is intentionally small; full
//! theming is a follow-up.

/// The default stylesheet — written to `_assets/style.css`.
pub const STYLE_CSS: &str = r#"/* kio doc — default stylesheet */
:root {
  --fg: #1a1a1a;
  --bg: #ffffff;
  --muted: #6a6a6a;
  --accent: #2563eb;
  --border: #e2e2e2;
  --code-bg: #f5f5f5;
  /* Token-theme colours for kio doc's highlighted Kio. One variable
     per .kio-… class (see kio-rs/src/tokens.rs TokenKind::css_class),
     bound by the .kio-… rules below. Light (One-Light) palette; the
     prefers-color-scheme: dark block at the end swaps them for the
     One-Dark hues the REPL truecolor palette uses. */
  --kio-tok-keyword-control: #a626a4;
  --kio-tok-keyword-declaration: #a626a4;
  --kio-tok-keyword-elaborator: #a626a4;
  --kio-tok-identifier: var(--fg);
  --kio-tok-operator-builtin: #0184bc;
  --kio-tok-operator-user: #0184bc;
  --kio-tok-literal-string: #50a14f;
  --kio-tok-literal-number: #986801;
  --kio-tok-literal-bool: #986801;
  --kio-tok-comment-line: #a0a1a7;
  --kio-tok-comment-doc: #a0a1a7;
  --kio-tok-punctuation-bracket: #6a737d;
  --kio-tok-punctuation-separator: #6a737d;
  --kio-tok-slot: #6a737d;
  --kio-tok-entity-name-module: #c18401;
  --kio-tok-entity-name-function: #4078f2;
  --kio-tok-entity-name-type: #c18401;
  --kio-tok-entity-name-label: #e45649;
  --kio-tok-variable-parameter: #e45649;
}
.kio-keyword-control { color: var(--kio-tok-keyword-control); }
.kio-keyword-declaration { color: var(--kio-tok-keyword-declaration); }
.kio-keyword-elaborator { color: var(--kio-tok-keyword-elaborator); }
.kio-identifier { color: var(--kio-tok-identifier); }
.kio-operator-builtin { color: var(--kio-tok-operator-builtin); }
.kio-operator-user { color: var(--kio-tok-operator-user); }
.kio-literal-string { color: var(--kio-tok-literal-string); }
.kio-literal-number { color: var(--kio-tok-literal-number); }
.kio-literal-bool { color: var(--kio-tok-literal-bool); }
.kio-comment-line { color: var(--kio-tok-comment-line); font-style: italic; }
.kio-comment-doc { color: var(--kio-tok-comment-doc); font-style: italic; }
.kio-punctuation-bracket { color: var(--kio-tok-punctuation-bracket); }
.kio-punctuation-separator { color: var(--kio-tok-punctuation-separator); }
.kio-slot { color: var(--kio-tok-slot); }
.kio-entity-name-module { color: var(--kio-tok-entity-name-module); }
.kio-entity-name-function { color: var(--kio-tok-entity-name-function); }
.kio-entity-name-type { color: var(--kio-tok-entity-name-type); }
.kio-entity-name-label { color: var(--kio-tok-entity-name-label); }
.kio-variable-parameter { color: var(--kio-tok-variable-parameter); font-style: italic; }
* { box-sizing: border-box; }
body {
  margin: 0;
  font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
  color: var(--fg);
  background: var(--bg);
  line-height: 1.6;
}
.layout { display: flex; min-height: 100vh; }
nav.sidebar {
  width: 16rem;
  flex-shrink: 0;
  border-right: 1px solid var(--border);
  padding: 1.5rem 1rem;
  overflow-y: auto;
}
nav.sidebar h2 {
  font-size: 0.75rem;
  text-transform: uppercase;
  letter-spacing: 0.05em;
  color: var(--muted);
  margin: 1.25rem 0 0.4rem;
}
nav.sidebar ul { list-style: none; margin: 0; padding: 0; }
nav.sidebar li { margin: 0.15rem 0; }
nav.sidebar a { color: var(--accent); text-decoration: none; font-size: 0.9rem; }
nav.sidebar a:hover { text-decoration: underline; }
main {
  flex: 1;
  padding: 2rem 3rem;
  max-width: 52rem;
}
h1, h2, h3, h4 { line-height: 1.25; }
code {
  background: var(--code-bg);
  padding: 0.1rem 0.3rem;
  border-radius: 3px;
  font-size: 0.9em;
}
pre {
  background: var(--code-bg);
  padding: 0.8rem 1rem;
  border-radius: 5px;
  overflow-x: auto;
}
pre code { background: none; padding: 0; }
a { color: var(--accent); }
section.item {
  border-top: 1px solid var(--border);
  padding-top: 1rem;
  margin-top: 1.5rem;
}
section.item .sig {
  background: var(--code-bg);
  padding: 0.6rem 0.9rem;
  border-radius: 5px;
  display: block;
  white-space: pre;
  overflow-x: auto;
}
.item-kind { color: var(--muted); font-size: 0.8rem; }
.toggle {
  display: none;
  background: none;
  border: 1px solid var(--border);
  border-radius: 4px;
  padding: 0.3rem 0.6rem;
  cursor: pointer;
}
@media (max-width: 48rem) {
  .layout { flex-direction: column; }
  nav.sidebar { width: 100%; border-right: none; border-bottom: 1px solid var(--border); }
  nav.sidebar.collapsed ul, nav.sidebar.collapsed h2 { display: none; }
  .toggle { display: inline-block; margin: 1rem; }
}
@media (prefers-color-scheme: dark) {
  :root {
    --fg: #abb2bf;
    --bg: #282c34;
    --muted: #828997;
    --accent: #61afef;
    --border: #3b414d;
    --code-bg: #21252b;
    /* One-Dark token hues, matching the REPL truecolor palette
       (kio-rs/src/repl/highlight.rs) so the website and the REPL
       read identically. */
    --kio-tok-keyword-control: #c678dd;
    --kio-tok-keyword-declaration: #e5c07b;
    --kio-tok-keyword-elaborator: #56b6c2;
    --kio-tok-identifier: var(--fg);
    --kio-tok-operator-builtin: #e06c75;
    --kio-tok-operator-user: #e06c75;
    --kio-tok-literal-string: #98c379;
    --kio-tok-literal-number: #d19a66;
    --kio-tok-literal-bool: #d19a66;
    --kio-tok-comment-line: #5c6370;
    --kio-tok-comment-doc: #5c6370;
    --kio-tok-punctuation-bracket: #828997;
    --kio-tok-punctuation-separator: #828997;
    --kio-tok-slot: #828997;
    --kio-tok-entity-name-module: #abb2bf;
    --kio-tok-entity-name-function: #61afef;
    --kio-tok-entity-name-type: #56b6c2;
    --kio-tok-entity-name-label: #e5c07b;
    --kio-tok-variable-parameter: #e06c75;
  }
}
"#;

/// The sidebar-toggle script — written to `_assets/nav.js`.
pub const NAV_JS: &str = r#"// kio doc — sidebar toggle for narrow viewports
document.addEventListener('DOMContentLoaded', function () {
  var btn = document.querySelector('.toggle');
  var nav = document.querySelector('nav.sidebar');
  if (btn && nav) {
    btn.addEventListener('click', function () {
      nav.classList.toggle('collapsed');
    });
  }
});
"#;
