// crates/core/schema/build/asset_lookup.rs

//! Résolution d'un `{% asset key %}` contre le manifeste chargé, avec
//! diagnostic de clé absente (suggestion par distance de Levenshtein).
//!
//! `fragment-forge` ne possède pas les clés du manifeste (aucun I/O dans
//! ce crate) : le calcul de la suggestion diagnostique vit exclusivement ici.

use std::collections::HashMap;

use marius_fragment_forge::AssetLookup;

use crate::manifest::AssetEntry;

// =============================================================================
// Résolution d'asset avec diagnostic — remplace les anciennes closures
// `|key| assets.get(key).map(|a| a.url.len())` (Option<usize>) câblées
// directement à `resolve_and_measure`. `fragment-forge` ne possède pas les
// clés du manifeste (aucun I/O dans ce crate — invariant inchangé) : le
// calcul de la suggestion diagnostique doit donc vivre ici, seul endroit
// où les clés candidates existent réellement en mémoire.
// =============================================================================

/// Résout un `{% asset key %}` contre le manifeste chargé — clé absente :
/// suggestion calculée ici, jamais dans `fragment-forge` (voir doc
/// d'`AssetLookup`, marius-fragment-forge/src/lib.rs).
pub(crate) fn resolve_asset_lookup(assets: &HashMap<String, AssetEntry>, key: &str) -> AssetLookup {
    match assets.get(key) {
        Some(entry) => AssetLookup::Found(entry.url.len()),
        None => AssetLookup::NotFound {
            suggestion: suggest_asset_key(key, assets),
        },
    }
}

/// Duplication délibérée de `suggest_variable`/`levenshtein` (`marius-assets`,
/// pipeline `[styles]` Phase 3) — même algorithme, aucune dépendance
/// partagée : la Roadmap `marius-assets` (§2.1) interdit explicitement tout
/// couplage de types Rust entre `marius-assets` et les crates de la Forge ;
/// `build.rs` n'a aucune raison d'y déroger pour emprunter dix lignes de
/// calcul de distance d'édition.
///
/// Même hiérarchie de confiance que côté `marius-assets`, jamais mélangée
/// dans un seul message : casse différente (quasi certaine) avant distance
/// de Levenshtein (une piste, pas une certitude), bornée à 2 pour éviter
/// une suggestion trompeuse sur une clé sans rapport réel.
fn suggest_asset_key(key: &str, assets: &HashMap<String, AssetEntry>) -> Option<String> {
    if let Some(exact_ci) = assets.keys().find(|k| k.eq_ignore_ascii_case(key)) {
        return Some(format!(
            "la casse ne correspond pas : le manifeste contient '{exact_ci}', pas '{key}'"
        ));
    }

    assets
        .keys()
        .map(|k| (k, levenshtein(key, k)))
        .filter(|(_, dist)| *dist <= 2)
        .min_by_key(|(_, dist)| *dist)
        .map(|(k, _)| format!("vouliez-vous dire '{k}' ?"))
}

/// Distance de Levenshtein — deux lignes de tableau roulées (`prev`/`curr`),
/// pas de matrice complète : un manifeste de thème compte au plus quelques
/// centaines de clés, seule l'empreinte mémoire par comparaison justifie ce
/// choix, pas la complexité (O(n·m) par paire est hors de propos ici).
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr: Vec<usize> = vec![0; b.len() + 1];

    for i in 1..=a.len() {
        curr[0] = i;
        for j in 1..=b.len() {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

// =============================================================================
// Tests — résolution d'asset avec diagnostic.
//
// Trois responsabilités testées séparément, sans se chevaucher :
//   - `levenshtein` : la fonction de distance elle-même, cas classiques.
//   - `suggest_asset_key` : la hiérarchie de confiance (casse avant
//     Levenshtein, aucune suggestion trompeuse au-delà du seuil).
//   - `resolve_asset_lookup` : le câblage — clé trouvée → `Found`, clé
//     absente → `NotFound` portant exactement la suggestion calculée.
// =============================================================================

#[cfg(test)]
mod tests_asset_lookup {
    use super::{AssetEntry, AssetLookup, levenshtein, resolve_asset_lookup, suggest_asset_key};
    use std::collections::HashMap;

    /// Entrée de manifeste minimale — seul `url` (et sa longueur) est
    /// consommé par `resolve_asset_lookup`, le reste n'a besoin d'être
    /// que syntaxiquement présent (voir `#[allow(dead_code)]` sur
    /// `AssetEntry` : ces champs sont pour le Shell au runtime, pas pour
    /// ce build.rs — même remarque que la doc de la struct).
    fn make_entry(url: &str) -> AssetEntry {
        AssetEntry {
            url: url.to_string(),
            path: String::new(),
            mime: String::new(),
            size: 0,
            hash: String::new(),
            version: String::new(),
        }
    }

    fn sample_manifest() -> HashMap<String, AssetEntry> {
        let mut m = HashMap::new();
        m.insert(
            "utils.svg".to_string(),
            make_entry("/sprites/utils.4c4e9.svg"),
        );
        m.insert(
            "players.svg".to_string(),
            make_entry("/sprites/players.76165.svg"),
        );
        m
    }

    // ── levenshtein ──────────────────────────────────────────────────────────

    #[test]
    fn levenshtein_identical_strings_is_zero() {
        assert_eq!(levenshtein("utils.svg", "utils.svg"), 0);
    }

    #[test]
    fn levenshtein_classic_kitten_sitting_is_three() {
        // Exemple canonique de la littérature — sert de garde-fou contre
        // une régression silencieuse de l'algorithme (ex. coût de
        // substitution mal posé, tableau non roulé correctement).
        assert_eq!(levenshtein("kitten", "sitting"), 3);
    }

    #[test]
    fn levenshtein_against_empty_string_is_the_other_length() {
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("abc", ""), 3);
    }

    #[test]
    fn levenshtein_single_typo_is_one() {
        // Le cas réel qui a motivé cette fonctionnalité : "util.svg" saisi
        // pour "utils.svg" — une lettre manquante, distance 1.
        assert_eq!(levenshtein("util.svg", "utils.svg"), 1);
    }

    // ── suggest_asset_key ────────────────────────────────────────────────────

    /// Priorité 1 : une correspondance insensible à la casse doit produire
    /// le message "la casse ne correspond pas", jamais un "vouliez-vous
    /// dire" générique — même clé, seule la casse diffère, la confiance
    /// est maximale et le message doit le refléter.
    #[test]
    fn suggest_asset_key_case_mismatch_takes_priority() {
        let manifest = sample_manifest();
        let suggestion = suggest_asset_key("UTILS.SVG", &manifest)
            .expect("une entrée ne différant que par la casse doit produire une suggestion");
        assert!(
            suggestion.contains("la casse ne correspond pas"),
            "message inattendu : {suggestion:?}"
        );
        assert!(
            suggestion.contains("utils.svg"),
            "message inattendu : {suggestion:?}"
        );
    }

    /// Priorité 2 : à défaut de correspondance de casse, une clé à
    /// distance ≤ 2 doit être proposée comme "vouliez-vous dire".
    /// C'est le cas exact rencontré en session : "util.svg" pour
    /// "utils.svg" (distance 1).
    #[test]
    fn suggest_asset_key_close_typo_suggests_nearest_key() {
        let manifest = sample_manifest();
        let suggestion = suggest_asset_key("util.svg", &manifest)
            .expect("distance 1 : une suggestion est attendue");
        assert_eq!(suggestion, "vouliez-vous dire 'utils.svg' ?");
    }

    /// Au-delà du seuil (distance > 2) et sans correspondance de casse :
    /// aucune suggestion — mieux vaut se taire qu'orienter vers une clé
    /// sans rapport réel. Cas réel : "silos/195v.svg", dont la présence
    /// d'un `/` seul suffit à l'éloigner de toute clé plate du manifeste.
    #[test]
    fn suggest_asset_key_no_close_match_returns_none() {
        let manifest = sample_manifest();
        assert_eq!(suggest_asset_key("silos/195v.svg", &manifest), None);
    }

    /// Manifeste vide : aucune candidate à proposer, `None` — pas de panique
    /// sur un registre sans entrée (`.min_by_key` sur un itérateur vide).
    #[test]
    fn suggest_asset_key_empty_manifest_returns_none() {
        let manifest: HashMap<String, AssetEntry> = HashMap::new();
        assert_eq!(suggest_asset_key("anything.svg", &manifest), None);
    }

    // ── resolve_asset_lookup ─────────────────────────────────────────────────

    #[test]
    fn resolve_asset_lookup_found_returns_url_length() {
        let manifest = sample_manifest();
        let result = resolve_asset_lookup(&manifest, "utils.svg");
        assert_eq!(result, AssetLookup::Found("/sprites/utils.4c4e9.svg".len()));
    }

    #[test]
    fn resolve_asset_lookup_missing_key_carries_the_computed_suggestion() {
        let manifest = sample_manifest();
        match resolve_asset_lookup(&manifest, "util.svg") {
            AssetLookup::NotFound { suggestion } => {
                assert_eq!(
                    suggestion,
                    Some("vouliez-vous dire 'utils.svg' ?".to_string())
                );
            }
            other => panic!("NotFound attendu, obtenu : {other:?}"),
        }
    }

    #[test]
    fn resolve_asset_lookup_missing_key_without_candidate_carries_none() {
        let manifest = sample_manifest();
        match resolve_asset_lookup(&manifest, "silos/195v.svg") {
            AssetLookup::NotFound { suggestion } => {
                assert_eq!(suggestion, None);
            }
            other => panic!("NotFound attendu, obtenu : {other:?}"),
        }
    }
}
