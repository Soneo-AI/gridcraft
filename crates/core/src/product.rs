//! The product name that ends up in files and formula results: `docProps/app.xml`
//! (`<Application>`), pivot caches (`refreshedBy`), the comments on hidden defined names, solver
//! reports, `INFO("osversion")` and the default comment author.
//!
//! An embedding host (KoAssist) sets its own name once at start; nothing it saves then names
//! GridCraft. Unset, everything reads "GridCraft" exactly as before.

use std::sync::OnceLock;

const DEFAULT: &str = "GridCraft";

static PRODUCT_NAME: OnceLock<String> = OnceLock::new();

/// Name the product once (the first non-empty name wins; later calls are ignored).
pub fn set_product_name(name: &str) {
    let name = name.trim();
    if !name.is_empty() {
        let _ = PRODUCT_NAME.set(name.to_string());
    }
}

/// The name from the environment (`KOASSIST_PRODUCT_NAME`), read once — the headless CLI has no
/// host that calls [`set_product_name`]; a sandbox sets the variable per run. Absent on wasm.
static ENV_NAME: OnceLock<Option<String>> = OnceLock::new();

/// What files and formulas call the application: the host's name, else `KOASSIST_PRODUCT_NAME`, else
/// "GridCraft".
pub fn product_name() -> &'static str {
    if let Some(name) = PRODUCT_NAME.get() {
        return name;
    }
    ENV_NAME
        .get_or_init(|| std::env::var("KOASSIST_PRODUCT_NAME").ok().map(|n| n.trim().to_string()).filter(|n| !n.is_empty()))
        .as_deref()
        .unwrap_or(DEFAULT)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pure: the process-wide name stays unset here, other tests look for GridCraft's own strings.
    #[test]
    fn unset_name_is_gridcraft() {
        assert_eq!(product_name(), DEFAULT);
    }
}
