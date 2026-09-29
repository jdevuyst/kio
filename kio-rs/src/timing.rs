#[cfg(feature = "cli")]
pub(crate) fn frontend_enabled() -> bool {
    enabled("frontend", "KIO_DEBUG_FRONTEND_TIMING")
}

#[cfg(feature = "cli")]
pub(crate) fn build_enabled() -> bool {
    enabled("build", "KIO_DEBUG_BUILD_TIMING")
}

#[cfg(feature = "surface")]
pub(crate) fn eval_enabled() -> bool {
    enabled("eval", "KIO_DEBUG_EVAL_TIMING")
}

#[cfg(feature = "surface")]
pub(crate) fn user_elaborator_frontend_enabled() -> bool {
    #[cfg(feature = "cli")]
    {
        frontend_enabled()
    }
    #[cfg(not(feature = "cli"))]
    {
        false
    }
}

#[cfg(feature = "lsp")]
pub(crate) fn lsp_enabled() -> bool {
    enabled("lsp", "KIO_DEBUG_LSP_TIMING")
}

#[cfg(any(feature = "cli", feature = "surface", feature = "lsp"))]
fn enabled(category_name: &str, category_var: &str) -> bool {
    enabled_from_env(category_name) || category_var_enabled(category_var)
}

#[cfg(any(feature = "cli", feature = "surface", feature = "lsp"))]
fn enabled_from_env(category_name: &str) -> bool {
    let Ok(value) = std::env::var("KIO_DEBUG_TIMING") else {
        return false;
    };
    enabled_from_value(category_name, &value)
}

#[cfg(any(feature = "cli", feature = "surface", feature = "lsp", test))]
fn enabled_from_value(category_name: &str, value: &str) -> bool {
    let value = value.trim();
    if value.is_empty() {
        return false;
    }
    for token in value.split([',', ' ', '\t', '\n', ';']) {
        let token = token.trim().to_ascii_lowercase();
        match token.as_str() {
            "" | "0" | "false" | "off" | "none" => {}
            "1" | "true" | "yes" | "all" => return true,
            other if other == category_name => return true,
            _ => {}
        }
    }
    false
}

#[cfg(any(feature = "cli", feature = "surface", feature = "lsp"))]
fn category_var_enabled(var: &str) -> bool {
    std::env::var(var)
        .map(|v| {
            let v = v.trim();
            !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false")
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    #[test]
    fn enabled_from_value_accepts_all_aliases() {
        for value in ["1", "true", "yes", "all"] {
            assert!(super::enabled_from_value("frontend", value));
            assert!(super::enabled_from_value("build", value));
            assert!(super::enabled_from_value("derive", value));
            assert!(super::enabled_from_value("eval", value));
            assert!(super::enabled_from_value("lsp", value));
        }
    }

    #[test]
    fn enabled_from_value_accepts_category_lists() {
        assert!(super::enabled_from_value("frontend", "frontend"));
        assert!(super::enabled_from_value("frontend", "derive,frontend"));
        assert!(super::enabled_from_value("build", "frontend,build"));
        assert!(super::enabled_from_value("derive", "frontend derive"));
        assert!(super::enabled_from_value("eval", "frontend eval"));
        assert!(super::enabled_from_value("lsp", "frontend lsp"));
        assert!(!super::enabled_from_value("build", "frontend"));
        assert!(!super::enabled_from_value("derive", "frontend"));
        assert!(!super::enabled_from_value("eval", "frontend"));
        assert!(!super::enabled_from_value("lsp", "frontend"));
    }

    #[test]
    fn enabled_from_value_rejects_disabled_values() {
        for value in ["", "0", "false", "off", "none"] {
            assert!(!super::enabled_from_value("frontend", value));
            assert!(!super::enabled_from_value("derive", value));
        }
    }
}
