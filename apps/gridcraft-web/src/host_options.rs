//! Options the embedding page sets before GridCraft starts (KoAssist), and the host-only
//! control methods it may call afterwards through `window.koassistSheet.request`.
//!
//! The page assigns `window.koassistSheetOptions` before it loads the bundle:
//!
//! ```js
//! window.koassistSheetOptions = {
//!   embedded: true,              // no title bar, no File tab (the host owns the file)
//!   read_only: true,             // a viewer: no ribbon/formula bar/editing (Chrome::read_only)
//!   hidden_tabs: ["Automate"],   // ribbon tabs left undrawn (ids from ribbon::TABS)
//!   language: "de",              // i18n::Language code
//!   theme: "light" | "dark" | "system",   // view.theme
//!   author: "Erika Mustermann",  // signs notes and comments
//!   product_name: "KoAssist",    // replaces "GridCraft" in every label
//!   tokens: { light: { accent: "#0f4f4a", … }, dark: { … } },   // theme::Tokens colours
//! };
//! ```
//!
//! Applied once after the stored preferences and before the first frame. Missing or malformed
//! options leave the app as it is; a wrong value is logged and skipped, never fatal.
//!
//! Host methods (answered like control methods): `host.tokens {light?, dark?}`,
//! `host.chrome {title_bar?, file_tab?, hidden_tabs?, read_only?}`, `host.author {name}`,
//! `host.product {name}`, `host.highlight {sheet?, range?}` (mark a cited range on a sheet:
//! `sheet` = index | name activates it first, `range` = A1 is drawn, selected and scrolled into
//! view; without `range` the mark is cleared).

use gridcraft_ui_egui::control::Outcome;
use gridcraft_ui_egui::i18n::Language;
use gridcraft_ui_egui::{Chrome, SheetApp, theme};
use serde_json::{Value, json};

/// The longest user name taken from the options (as `host.rs` bounds `?author=`).
const MAX_AUTHOR_CHARS: usize = 200;

#[derive(Debug, Default)]
pub struct HostOptions {
    pub embedded: bool,
    pub read_only: bool,
    pub hidden_tabs: Option<Vec<String>>,
    pub language: Option<String>,
    pub theme: Option<String>,
    pub author: Option<String>,
    pub product_name: Option<String>,
    pub tokens: Option<Value>,
}

/// `window.koassistSheetOptions`, if the page set it.
pub fn read() -> HostOptions {
    let Some(window) = web_sys::window() else { return HostOptions::default() };
    let Ok(raw) = js_sys::Reflect::get(&window, &"koassistSheetOptions".into()) else { return HostOptions::default() };
    if raw.is_undefined() || raw.is_null() {
        return HostOptions::default();
    }
    let text = js_sys::JSON::stringify(&raw).ok().and_then(|s| s.as_string());
    let value: Value = match text.as_deref().map(serde_json::from_str::<Value>) {
        Some(Ok(v)) => v,
        _ => {
            log::warn!("koassistSheetOptions: not JSON; ignored");
            return HostOptions::default();
        }
    };
    from_value(&value)
}

/// The options from their JSON form (also what `read` parses).
pub fn from_value(value: &Value) -> HostOptions {
    let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    HostOptions {
        embedded: value.get("embedded").and_then(Value::as_bool).unwrap_or(false),
        read_only: value.get("read_only").and_then(Value::as_bool).unwrap_or(false),
        hidden_tabs: tab_list(value.get("hidden_tabs")),
        language: text("language"),
        theme: text("theme"),
        author: text("author"),
        product_name: text("product_name"),
        tokens: value.get("tokens").filter(|t| t.is_object()).cloned(),
    }
}

/// Apply the options to the app (before its first frame).
pub fn apply(app: &mut SheetApp, options: &HostOptions) {
    if options.embedded {
        app.chrome = Chrome { title_bar: false, file_tab: false, ..Default::default() };
    }
    if let Some(tabs) = &options.hidden_tabs {
        app.chrome.hidden_tabs = tabs.clone();
    }
    if options.read_only {
        app.set_read_only(true);
    }
    if let Some(name) = &options.product_name {
        gridcraft_ui_egui::i18n::set_product_name(name);
    }
    if let Some(author) = &options.author {
        set_author(app, author);
    }
    if let Some(code) = &options.language {
        match Language::parse(code) {
            Some(language) => app.ui.language = language,
            None => log::warn!("koassistSheetOptions.language: unknown `{code}`"),
        }
    }
    if let Some(mode) = &options.theme
        && let Err(e) = app.run("view.theme", json!({"mode": mode}))
    {
        log::warn!("koassistSheetOptions.theme: {e}");
    }
    if let Some(tokens) = &options.tokens
        && let Err(e) = apply_tokens(tokens)
    {
        log::warn!("koassistSheetOptions.tokens: {e}");
    }
}

/// A JSON array of tab ids (`["Automate"]`), trimmed, without empties; `None` when absent.
fn tab_list(value: Option<&Value>) -> Option<Vec<String>> {
    let items = value?.as_array()?;
    Some(items.iter().filter_map(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect())
}

fn set_author(app: &mut SheetApp, author: &str) {
    let author: String = author.trim().chars().take(MAX_AUTHOR_CHARS).collect();
    if !author.is_empty() {
        app.session.prefs.user_name = author;
    }
}

/// `{"light": {…}, "dark": {…}}`: a theme given as an empty object drops its overlay, a theme
/// not given keeps what it has.
fn apply_tokens(tokens: &Value) -> Result<(), String> {
    for (dark, key) in [(false, "light"), (true, "dark")] {
        if let Some(map) = tokens.get(key) {
            let parsed = theme::HostTokens::from_json(map).map_err(|e| format!("{key}: {e}"))?;
            theme::set_host_tokens(dark, parsed);
        }
    }
    Ok(())
}

fn ok(v: Value) -> Outcome {
    Outcome::Done(json!({"ok": true, "result": v}))
}

fn err(e: impl std::fmt::Display) -> Outcome {
    Outcome::Done(json!({"ok": false, "error": e.to_string()}))
}

/// Host-only methods the page may call besides the control layer; `None` for anything else.
pub fn handle(app: &mut SheetApp, method: &str, p: &Value) -> Option<Outcome> {
    Some(match method {
        "host.tokens" => match apply_tokens(p) {
            Ok(()) => ok(json!({"light": theme::host_token_count(false), "dark": theme::host_token_count(true)})),
            Err(e) => err(e),
        },
        "host.chrome" => {
            if let Some(on) = p.get("title_bar").and_then(Value::as_bool) {
                app.chrome.title_bar = on;
            }
            if let Some(on) = p.get("file_tab").and_then(Value::as_bool) {
                app.chrome.file_tab = on;
            }
            if let Some(tabs) = tab_list(p.get("hidden_tabs")) {
                app.chrome.hidden_tabs = tabs;
            }
            if let Some(on) = p.get("read_only").and_then(Value::as_bool) {
                app.set_read_only(on);
            }
            ok(json!({"title_bar": app.chrome.title_bar, "file_tab": app.chrome.file_tab, "hidden_tabs": app.chrome.hidden_tabs, "read_only": app.chrome.read_only}))
        }
        "host.highlight" => {
            if let Some(sheet) = p.get("sheet")
                && let Err(e) = app.run("sheet.activate", json!({"sheet": sheet}))
            {
                return Some(err(e));
            }
            let range = p.get("range").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty());
            match app.set_host_highlight(range) {
                Ok(()) => {
                    let sheet = app.session.active().map(|d| d.wb.active_sheet());
                    ok(json!({"sheet": sheet, "range": range}))
                }
                Err(e) => err(e),
            }
        }
        "host.author" => {
            if let Some(name) = p.get("name").and_then(Value::as_str) {
                set_author(app, name);
            }
            ok(json!({"author": app.session.prefs.user_name}))
        }
        "host.product" => {
            if let Some(name) = p.get("name").and_then(Value::as_str) {
                gridcraft_ui_egui::i18n::set_product_name(name);
            }
            ok(json!({"product": gridcraft_ui_egui::i18n::product_name()}))
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_is_read_from_the_options_and_off_by_default() {
        assert!(from_value(&json!({"embedded": true, "read_only": true})).read_only);
        assert!(!from_value(&json!({"embedded": true})).read_only);
        assert!(!from_value(&json!({"read_only": "yes"})).read_only);
    }
}
