//! # marius-db-forge · crates/forge/db-forge/src/naming.rs
//!
//! Conventions de nommage pour les artefacts générés.

/// `content_core` → `ContentCore`
/// `commerce_product_core` → `CommerceProductCore`
pub fn to_pascal(s: &str) -> String {
    s.split('_')
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                None => String::new(),
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
            }
        })
        .collect()
}

/// `content_core` → `CONTENT_CORE`
pub fn to_screaming(s: &str) -> String {
    s.to_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pascal_simple() {
        assert_eq!(to_pascal("content_core"), "ContentCore");
    }

    #[test]
    fn pascal_compound() {
        assert_eq!(to_pascal("commerce_product_core"), "CommerceProductCore");
    }

    #[test]
    fn screaming_simple() {
        assert_eq!(to_screaming("content_core"), "CONTENT_CORE");
    }

    #[test]
    fn screaming_compound() {
        assert_eq!(
            to_screaming("commerce_product_core"),
            "COMMERCE_PRODUCT_CORE"
        );
    }
}
