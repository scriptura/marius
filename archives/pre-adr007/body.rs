use std::collections::{HashMap, HashSet};
use std::fmt::Write;

use crate::FlatPageToken;

// ── Types supposés définis dans crate::types — reproduits pour cohérence ──────

/// Champ de largeur fixe : valeur scalaire accessible sur `record`.
pub struct FieldSpec<'a> {
    pub name: &'a str,
}

/// Champ varlena : stocké hors-ligne, accès via `as_deref()`.
/// "Varlena" (variable-length array) : terme PostgreSQL désignant un champ
/// dont la largeur n'est pas connue statiquement.
pub struct VarlenField<'a> {
    pub name: &'a str,
}

// ── Index Fixed/Varlena — O(1) lookup ─────────────────────────────────────────

pub struct FieldIndex<'a> {
    varlena: HashSet<&'a str>,
}

impl<'a> FieldIndex<'a> {
    #[inline]
    pub fn is_varlena(&self, field: &str) -> bool {
        self.varlena.contains(field)
    }
}

/// Construit l'index de classification depuis les slices de métadonnées.
/// Coût : O(n) construction, O(1) lookup par `is_varlena`.
pub fn build_field_index<'a>(
    _fields: &[FieldSpec<'a>],
    varlena: &[VarlenField<'a>],
) -> FieldIndex<'a> {
    FieldIndex {
        varlena: varlena.iter().map(|v| v.name).collect(),
    }
}

/// Renvoie la représentation littérale Rust de `s` : guillemets inclus,
/// contenu escapé. Délègue au Display Debug de &str — décision reconduite
/// de Phase 2.2.
pub fn rust_raw_str_lit(s: &str) -> String {
    format!("{s:?}")
}

// ── Générateur du corps séquentiel ────────────────────────────────────────────

/// Génère les instructions du corps de `render_page`, hors signature et
/// accolade fermante (périmètre Phase 3.3).
///
/// Structure de sortie :
///   1. Déclarations `_ref` Varlena (déduplicées, triées — déterminisme build).
///   2. Séquence plate d'instructions d'émission.
///
/// Deux niveaux d'indentation max (INV-2). Toute allocation est build-time.
pub fn generate_sequential_body(
    flat: &[FlatPageToken<'_>],
    fields: &[FieldSpec<'_>],
    varlena: &[VarlenField<'_>],
    static_idents: &HashMap<String, String>,
) -> String {
    let index = build_field_index(fields, varlena);

    // ── Pass 1 : collecte des champs Varlena référencés ───────────────────────
    let mut varlena_used: Vec<&str> = flat
        .iter()
        .filter_map(|t| match t {
            FlatPageToken::Field { field, .. } if index.is_varlena(field) => Some(*field),
            _ => None,
        })
        .collect();
    // Tri alphabétique : déterminisme du fichier généré indépendant de l'ordre
    // de déclaration dans le template.
    varlena_used.sort_unstable();
    varlena_used.dedup();

    let mut out = String::with_capacity(flat.len() * 64 + varlena_used.len() * 52);

    // ── Déclarations de références Varlena ────────────────────────────────────
    for name in &varlena_used {
        writeln!(
            out,
            "    let {name}_ref: Option<&str> = varlena.{name}.as_deref();"
        )
        .unwrap();
    }

    // ── Pass 2 : émission séquentielle ────────────────────────────────────────
    // `indent` : deux valeurs statiques uniquement.
    // INV-2 garantit qu'un IfBool ne peut être actif quand un autre est rencontré.
    let mut indent: &'static str = "    ";

    for token in flat {
        match token {
            FlatPageToken::Static(s) => {
                let lit = rust_raw_str_lit(s);
                writeln!(out, "{indent}buf.push_str({lit});").unwrap();
            }

            FlatPageToken::StaticInclude { original_path, .. } => {
                let ident = static_idents
                    .get(*original_path)
                    .expect("static_idents: clé absente — pipeline AOT incohérent");
                writeln!(out, "{indent}buf.push_str(static_partials::{ident});").unwrap();
            }

            FlatPageToken::Field { field, .. } => {
                if index.is_varlena(field) {
                    writeln!(
                        out,
                        "{indent}if let Some(s) = {field}_ref {{ marius_html_escape(s, buf); }}"
                    )
                    .unwrap();
                } else {
                    writeln!(
                        out,
                        "{indent}::std::fmt::Write::write_fmt(buf, format_args!(\"{{}}\", record.{field})).ok();"
                    )
                    .unwrap();
                }
            }

            FlatPageToken::IfBool { field, .. } => {
                // IfBool est invariablement au niveau base (INV-2) :
                // on n'utilise pas `indent` pour cet émis, on fixe la prochaine.
                writeln!(out, "    if record.{field} {{").unwrap();
                indent = "        ";
            }

            FlatPageToken::EndIf => {
                indent = "    ";
                writeln!(out, "    }}").unwrap();
            }
        }
    }

    out
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_sequential_body() {
        // ── Fixtures ─────────────────────────────────────────────────────────
        let tokens: &[FlatPageToken<'_>] = &[
            FlatPageToken::Static("<div class=\"root\">"),
            FlatPageToken::StaticInclude {
                original_path: "partials/nav.html",
                rel_from_manifest: "partials/nav.html",
                len: 128,
            },
            FlatPageToken::Field { entity: "article", field: "title" },
            FlatPageToken::Field { entity: "article", field: "body" },
            FlatPageToken::IfBool { entity: "article", field: "is_published" },
            FlatPageToken::Static("<span>published</span>"),
            FlatPageToken::EndIf,
        ];

        let fields = &[
            FieldSpec { name: "title" },
            FieldSpec { name: "is_published" },
        ];
        let varlena = &[VarlenField { name: "body" }];

        let mut static_idents = HashMap::new();
        static_idents.insert(
            "partials/nav.html".to_string(),
            "NAV_PARTIAL".to_string(),
        );

        // ── Exécution ────────────────────────────────────────────────────────
        let output = generate_sequential_body(tokens, fields, varlena, &static_idents);

        // ── Assertion verbatim ────────────────────────────────────────────────
        // concat! : segments disjoints pour lisibilité des niveaux d'indentation.
        let expected = concat!(
            "    let body_ref: Option<&str> = varlena.body.as_deref();\n",
            "    buf.push_str(\"<div class=\\\"root\\\">\");\n",
            "    buf.push_str(static_partials::NAV_PARTIAL);\n",
            "    ::std::fmt::Write::write_fmt(buf, format_args!(\"{}\", record.title)).ok();\n",
            "    if let Some(s) = body_ref { marius_html_escape(s, buf); }\n",
            "    if record.is_published {\n",
            "        buf.push_str(\"<span>published</span>\");\n",
            "    }\n",
        );

        assert_eq!(output, expected);
    }
}
