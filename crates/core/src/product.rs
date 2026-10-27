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

/// What files and formulas call the application: "GridCraft", or the host's name.
pub fn product_name() -> &'static str {
    PRODUCT_NAME.get().map(String::as_str).unwrap_or(DEFAULT)
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
