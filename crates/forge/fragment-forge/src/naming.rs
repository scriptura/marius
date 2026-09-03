//! Utilitaires transverses de nommage et de calcul de capacité —
//! conversion chemin → identifiant Rust, résolution de chemin `include_str!`,
//! calcul de capacité statique du balisage `<article><dl>` généré.
//! Utilisés par les deux pipelines (Fragment et Page).

use crate::schema::{FieldSpec, VarlenField};

// =============================================================================
// I.bis Utilitaires de chemin — préparation I/O disque (templates .marius)
// =============================================================================

/// Convertit un chemin de fichier en identifiant Rust valide pour une
/// constante statique (`static_partials::IDENT`).
///
/// Règles :
///   - Tout caractère non [A-Za-z0-9_] devient '_'.
///   - Le résultat est mis en SCREAMING_SNAKE_CASE.
///   - Un préfixe '_' est ajouté si le premier caractère est un chiffre
///     (un identifiant Rust ne peut pas commencer par un chiffre).
///
/// Exemples :
///   "partials/nav.html"      → "PARTIALS_NAV_HTML"
///   "fragments/2024/foo.htm" → "_FRAGMENTS_2024_FOO_HTM"
pub fn static_const_ident(path: &str) -> String {
    let mut ident: String = path
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect::<String>()
        .to_uppercase();

    if ident.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        ident.insert(0, '_');
    }

    ident
}

/// Calcule le chemin à utiliser dans `include_str!()` pour un template
/// référencé depuis le fichier généré (`{out_dir}/{table}_render.rs`).
///
/// `include_str!` résout ses chemins relativement au fichier source qui
/// contient la macro, pas relativement à `CARGO_MANIFEST_DIR`. Le plus
/// robuste est donc d'émettre un chemin absolu : `include_str!` l'accepte
/// aussi bien qu'un chemin relatif, et ça évite tout calcul de profondeur
/// fragile entre OUT_DIR (profond, sous target/debug/build/.../out) et
/// la racine du manifeste.
///
/// `manifest_dir`       : CARGO_MANIFEST_DIR du crate appelant build.rs.
/// `rel_from_manifest`  : chemin du template relatif au manifeste
///                        (ex: "templates/page.marius").
pub fn relative_path_for_include_str(manifest_dir: &str, rel_from_manifest: &str) -> String {
    use std::path::Path;
    Path::new(manifest_dir)
        .join(rel_from_manifest)
        .to_string_lossy()
        .into_owned()
}

// =============================================================================
// II. Calcul de capacité
// =============================================================================

/// Somme des octets HTML statiques entourant les valeurs dynamiques.
///
/// Calcul purement arithmétique — zéro allocation, zéro construction de String.
/// Appelé au build-time (build.rs), résultat émis comme constante dans le code généré.
///
/// ─── Structure HTML générée ──────────────────────────────────────────────────
///
///   <article class="{schema}-{table}" data-id="{pk}">
///     <dl>
///       <dt>{field.name}</dt><dd>{field.value}</dd>
///       ...  (champs fixed-length)
///       <dt>{varlena.name}</dt><dd>{varlena.value}</dd>
///       ...  (champs varlena)
///     </dl>
///   </article>
///
/// ─── Décomposition comptable ─────────────────────────────────────────────────
///
///   Tag ouvrant : `<article class="` (16) + schema + `-` (1) + table + `" data-id="` (11)
///   Après PK    : `"><dl>` (6)  [ferme data-id + ouvre dl]
///   Par champ   : `<dt>` (4) + nom + `</dt><dd>` (9) + `</dd>` (5)
///   Tag fermant : `</dl></article>` (15)
///
/// Note : les valeurs dynamiques (PK, champs) ne sont PAS comptées ici.
/// Leur capacité est désormais calculée par `resolve_and_measure` (Voie B),
/// pas par une fonction `dynamic_capacity` dédiée — supprimée avec ce patch
/// (ADR-007) : elle était un résidu Voie A sans appelant, et sa signature
/// (`.sum::<usize>()` sur `VarlenField::max_escaped_len()`) ne pouvait plus
/// compiler une fois cette méthode passée à `Option<usize>` (champs non
/// bornés). `TemplateMetrics::total_dynamic_bytes` est l'unique source de
/// vérité pour la capacité dynamique.
pub fn static_capacity(
    schema: &str,
    table: &str,
    fields: &[FieldSpec],
    varlena: &[VarlenField],
) -> usize {
    // `<article class="` = 16 octets
    // schema
    // `-` = 1 octet
    // table
    // `" data-id="` = 11 octets
    let open_tag = 16 + schema.len() + 1 + table.len() + 11;

    // `"><dl>` = 6 octets (ferme la valeur de data-id, ferme le tag article, ouvre dl)
    let after_id = 6;

    let mut cap = open_tag + after_id;

    // Par champ fixed-length : `<dt>` (4) + nom + `</dt><dd>` (9) + `</dd>` (5) = 18 + len(nom)
    for f in fields {
        cap += 4 + f.name.len() + 9 + 5;
    }

    // Par champ varlena : même structure de balisage.
    for v in varlena {
        cap += 4 + v.name.len() + 9 + 5;
    }

    // `</dl></article>` = 15 octets
    cap += 15;

    cap
}
