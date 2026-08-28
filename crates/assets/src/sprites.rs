// crates/assets/src/sprites.rs

//! Pipeline `[sprites]` (Phase 4).
//!
//! Fusionne l'ensemble des fichiers `.svg` d'un répertoire source en un unique sprite
//! maître prêt à être référencé nativement par le client.
//!
//! ## Modèle d'Assemblage & Hachage
//!
//! - **Topologie (Dossier $\rightarrow$ Cible) :** Chaque fichier `.svg` d'un dossier est converti
//!   en une balise `<symbol>`. L'ensemble est encapsulé dans un `<svg>` parent masqué (`display:none`).
//! - **Usage Client :** Les icônes sont appelées unitairement via `<use href="...#id">`.
//! - **Génération d'Empreinte :** Le hachage BLAKE3 est calculé sur l'artefact cible final (le sprite assemblé),
//!   et non par composition des fichiers sources isolés.
//!
//! ## Sympathie Mécanique (Zero-DOM)
//!
//! L'implémentation repose sur `quick_xml::Reader`, un analyseur itératif (*PULL parser*) opérant sur `&str`.
//!
//! - **Mémoire $O(1)$ structurel :** Aucun DOM n'est alloué. La mémoire allouée est strictement
//!   proportionnelle au flux de sortie (le buffer cible), indépendamment de la profondeur ou du volume de l'arbre XML.
//! - **Logique de Pile Sans Allocation :** Le suivi de l'imbrication (nécessaire pour extraire exclusivement
//!   le contenu *intérieur* de la balise racine `<svg>`) s'effectue via un simple registre de comptage (entier).
//!   *(Applique la même discipline architecturale que `find_matching_brace` pour le CSS : aucune pile explicite).*
//! - **Garanties Fail-Fast :** Le compilateur délègue la vérification de la fermeture des balises à
//!   la machine à états de `quick_xml`. Toute malformation de l'arbre court-circuite le pipeline (`Err`)
//!   avant d'atteindre le compteur métier.

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use quick_xml::Reader;
use quick_xml::events::Event;

use crate::manifest::{AssetEntry, CanonicalAssetId, hash_content, join_slash, mime_for_extension};

/// Erreur survenant lors du parsing ou de la fusion itérative des SVG.
#[derive(Debug)]
pub(crate) struct SpriteError(String);

impl fmt::Display for SpriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sprites : {}", self.0)
    }
}

impl std::error::Error for SpriteError {}

/// Sérialise une balise ouvrante (`Start` ou `Empty`) en purgeant `fill`/
/// `stroke` codés en dur — sauf `currentColor`/`none`, qui doivent
/// survivre tels quels (comportement déjà correct, rien à réécrire :
/// `none` signifie explicitement "pas de remplissage", `currentColor` est
/// déjà la manipulabilité CSS visée, pas un codage en dur à éliminer).
/// Toute autre valeur (`#ff0000`, `rgb(...)`, un nom de couleur littéral)
/// fige la couleur au niveau du fichier source — exactement ce que la
/// mission demande de retirer pour laisser le CSS du thème piloter la
/// couleur au runtime.
pub(crate) fn serialize_start(
    e: &quick_xml::events::BytesStart,
    self_closing: bool,
) -> Result<String, SpriteError> {
    let mut out = String::new();
    out.push('<');
    out.push_str(&String::from_utf8_lossy(e.name().as_ref()));

    for attr in e.attributes() {
        let attr = attr.map_err(|err| SpriteError(format!("attribut XML invalide : {err}")))?;
        let key = attr.key.as_ref();
        if matches!(key, b"fill" | b"stroke") {
            let value = attr.value.as_ref();
            if value != b"currentColor" && value != b"none" {
                continue; // purgé — codé en dur, ni currentColor ni none.
            }
        }
        out.push(' ');
        out.push_str(&String::from_utf8_lossy(key));
        out.push_str("=\"");
        out.push_str(&String::from_utf8_lossy(attr.value.as_ref()));
        out.push('"');
    }

    out.push_str(if self_closing { " />" } else { ">" });
    Ok(out)
}

/// Extrait la valeur de l'attribut `viewBox` de la balise racine `<svg>`
/// source, si présent.
///
/// Correctif pipeline sprites : la racine `<svg>` est entièrement purgée
/// au profit de `<symbol>` (cf. doc de `svg_file_to_symbol`), mais son
/// `viewBox` porte le système de coordonnées interne du dessin — sans
/// lui, un `<use>` référençant ce symbole retombe sur les dimensions par
/// défaut du navigateur (300×150) au lieu du repère `0 0 W H` d'origine.
/// Seul cet attribut est reporté ; le reste de la racine (`width`,
/// `height`, `xmlns` du fragment, etc.) reste volontairement perdu — ces
/// dimensions-là sont dictées par le contexte d'utilisation du symbole
/// (CSS), pas par le fichier source.
///
/// Nom d'attribut sensible à la casse (XML) : uniquement `viewBox`
/// (camelCase exact) — `viewbox` ou `VIEWBOX` ne matcheraient pas, ce
/// qui est correct : ce ne serait de toute façon pas l'attribut SVG
/// standard.
pub(crate) fn extract_view_box(
    e: &quick_xml::events::BytesStart,
) -> Result<Option<String>, SpriteError> {
    for attr in e.attributes() {
        let attr = attr.map_err(|err| SpriteError(format!("attribut XML invalide : {err}")))?;
        if attr.key.as_ref() == b"viewBox" {
            return Ok(Some(
                String::from_utf8_lossy(attr.value.as_ref()).into_owned(),
            ));
        }
    }
    Ok(None)
}

/// Construit l'étiquette d'ouverture `<symbol id="...">`, avec `viewBox`
/// reporté si la racine source en portait un.
pub(crate) fn build_symbol_header(id: &str, view_box: Option<&str>) -> String {
    let mut out = String::new();
    out.push_str("<symbol id=\"");
    out.push_str(id);
    out.push('"');
    if let Some(vb) = view_box {
        out.push_str(" viewBox=\"");
        out.push_str(vb);
        out.push('"');
    }
    out.push('>');
    out
}

/// Transforme un fichier SVG source en `<symbol id="...">...</symbol>` —
/// entêtes `<?xml...?>`/`<!DOCTYPE...>` ignorés, seul le contenu À
/// L'INTÉRIEUR de la balise racine `<svg>` est conservé (plus son
/// `viewBox`, reporté sur `<symbol>` lui-même, cf. `extract_view_box`).
///
/// Suivi de profondeur : `depth` compte les éléments ouverts DEPUIS la
/// racine (elle-même exclue — jamais incrémentée pour son propre `Start`).
/// La balise fermante rencontrée avec `depth == 0` est donc nécessairement
/// celle de la racine elle-même : fin du contenu utile, sans qu'il soit
/// besoin de mémoriser son nom pour la reconnaître.
pub(crate) fn svg_file_to_symbol(id: &str, source: &str) -> Result<String, SpriteError> {
    let mut reader = Reader::from_str(source);
    let mut out = String::new();

    let mut root_found = false;
    let mut depth: u32 = 0;

    loop {
        let event = reader
            .read_event()
            .map_err(|e| SpriteError(format!("{id} : XML invalide : {e}")))?;

        match event {
            // Entêtes explicitement ignorés — spec §3 de la mission.
            Event::Decl(_) | Event::PI(_) | Event::DocType(_) | Event::Comment(_) => {}

            Event::Start(e) => {
                if !root_found {
                    if e.local_name().as_ref() == b"svg" {
                        root_found = true;
                        let view_box = extract_view_box(&e)?;
                        out.push_str(&build_symbol_header(id, view_box.as_deref()));
                    } else {
                        return Err(SpriteError(format!(
                            "{id} : balise racine <svg> attendue, trouvé <{}>",
                            String::from_utf8_lossy(e.name().as_ref())
                        )));
                    }
                } else {
                    depth += 1;
                    out.push_str(&serialize_start(&e, false)?);
                }
            }

            Event::Empty(e) => {
                if !root_found {
                    if e.local_name().as_ref() == b"svg" {
                        // <svg ... /> — racine explicitement vide, mais son
                        // éventuel viewBox reste sémantiquement valide : un
                        // symbole vide n'est pas la même chose qu'un symbole
                        // sans repère de coordonnées défini.
                        let view_box = extract_view_box(&e)?;
                        out.push_str(&build_symbol_header(id, view_box.as_deref()));
                        break;
                    }
                    return Err(SpriteError(format!(
                        "{id} : balise racine <svg> attendue, trouvé <{}>",
                        String::from_utf8_lossy(e.name().as_ref())
                    )));
                }
                out.push_str(&serialize_start(&e, true)?);
            }

            Event::End(e) => {
                if !root_found {
                    return Err(SpriteError(format!(
                        "{id} : balise fermante inattendue avant la racine <svg>"
                    )));
                }
                if depth == 0 {
                    // Fermeture de la racine elle-même : fin du contenu utile.
                    break;
                }
                depth -= 1;
                out.push_str("</");
                out.push_str(&String::from_utf8_lossy(e.name().as_ref()));
                out.push('>');
            }

            Event::Text(e) => {
                if root_found {
                    out.push_str(&String::from_utf8_lossy(&e));
                }
            }

            Event::CData(e) => {
                if root_found {
                    out.push_str("<![CDATA[");
                    out.push_str(&String::from_utf8_lossy(&e));
                    out.push_str("]]>");
                }
            }

            // Référence générale (`&entity;`) — grammaire non rencontrée
            // dans les SVG réels du thème à ce stade, ignorée sans erreur
            // plutôt que de bloquer tout le pipeline pour un cas marginal.
            Event::GeneralRef(_) => {}

            Event::Eof => {
                if !root_found {
                    return Err(SpriteError(format!("{id} : aucune balise <svg> trouvée")));
                }
                break;
            }
        }
    }

    out.push_str("</symbol>");
    Ok(out)
}

pub(crate) fn run_sprites_pipeline(
    theme_dir: &Path,
    build_root: &Path,
    build_root_rel: &str,
    sprites: &HashMap<String, String>,
    manifest: &mut HashMap<String, AssetEntry>,
) -> Result<(), Box<dyn std::error::Error>> {
    // `theme.toml` désérialise `[sprites]` en HashMap — ordre d'itération
    // non spécifié par le langage. Trier les clés n'est pas une question
    // de style : le manifeste (et son hash indirect via le contenu émis)
    // doit être reproductible d'un build à l'autre sur la même source.
    let mut sprite_names: Vec<&String> = sprites.keys().collect();
    sprite_names.sort();

    for sprite_name in sprite_names {
        let source_dir_rel = &sprites[sprite_name];
        let source_dir = theme_dir.join(source_dir_rel);

        let mut svg_files: Vec<PathBuf> = fs::read_dir(&source_dir)
            .map_err(|e| {
                format!(
                    "sprites : dossier introuvable {} : {e}",
                    source_dir.display()
                )
            })?
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("svg"))
            .collect();
        // Même raison que le tri des clés ci-dessus : `read_dir` ne
        // garantit lui non plus aucun ordre — la reproductibilité du
        // sprite maître (donc de son contenu et de son hash) en dépend.
        svg_files.sort();

        let mut symbols = String::new();
        for svg_path in &svg_files {
            let id = svg_path.file_stem().ok_or_else(|| {
                format!("sprites : nom de fichier invalide : {}", svg_path.display())
            })?;
            let id = id.to_string_lossy();

            let source = fs::read_to_string(svg_path).map_err(|e| {
                format!(
                    "sprites : lecture impossible de {} : {e}",
                    svg_path.display()
                )
            })?;

            let symbol = svg_file_to_symbol(&id, &source)
                .map_err(|e| format!("sprites : {} : {e}", svg_path.display()))?;

            symbols.push_str(&symbol);
        }

        let sprite_svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" style="display:none;">{symbols}</svg>"#
        );
        let bytes = sprite_svg.as_bytes();
        let (full_hash, short_hash) = hash_content(bytes);

        let hashed_filename = format!("{sprite_name}.{short_hash}.svg");
        let output_rel = join_slash("sprites", &hashed_filename);
        let output_abs = build_root.join(&output_rel);

        if let Some(dir) = output_abs.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(&output_abs, bytes)?;

        let logical_key = CanonicalAssetId::from_theme_relative_path(Path::new(&format!(
            "sprites/{sprite_name}.svg"
        )))
        .into_string();
        manifest.insert(
            logical_key,
            AssetEntry {
                url: format!("/{output_rel}"),
                path: join_slash(build_root_rel, &output_rel),
                mime: mime_for_extension("svg").to_string(),
                size: bytes.len() as u64,
                hash: full_hash,
                version: String::new(), // rempli par l'appelant (theme.version)
                // SVG assemblé — jamais consommé via `deps`, champ inerte.
                module: true,
            },
        );

        println!(
            "[marius-assets] sprites   {source_dir_rel} -> /{output_rel} ({} icônes)",
            svg_files.len()
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── svg_file_to_symbol / serialize_start (Phase 4, [sprites]) ───────────

    /// Le comportement central de la mission : `fill`/`stroke` codés en dur
    /// sont purgés, la racine `<svg>` disparaît au profit de `<symbol>`.
    #[test]
    fn svg_file_to_symbol_purges_hardcoded_fill_and_wraps_in_symbol() {
        let src =
            r##"<svg xmlns="http://www.w3.org/2000/svg"><path d="M0 0" fill="#ff0000"/></svg>"##;
        let out = svg_file_to_symbol("icon", src).unwrap();
        assert_eq!(out, r#"<symbol id="icon"><path d="M0 0" /></symbol>"#);
    }

    /// Exception explicite de la mission : `currentColor`/`none` doivent
    /// survivre intacts, ce ne sont pas des couleurs codées en dur.
    #[test]
    fn svg_file_to_symbol_keeps_current_color_and_none() {
        let src = r#"<svg><path fill="currentColor" stroke="none" d="M1 1"/></svg>"#;
        let out = svg_file_to_symbol("icon", src).unwrap();
        assert_eq!(
            out,
            r#"<symbol id="icon"><path fill="currentColor" stroke="none" d="M1 1" /></symbol>"#
        );
    }

    #[test]
    fn svg_file_to_symbol_handles_nested_non_empty_elements() {
        let src = r#"<svg><g><path d="M0 0"/></g></svg>"#;
        let out = svg_file_to_symbol("g1", src).unwrap();
        assert_eq!(out, r#"<symbol id="g1"><g><path d="M0 0" /></g></symbol>"#);
    }

    #[test]
    fn svg_file_to_symbol_ignores_decl_doctype_and_comments() {
        let src =
            "<?xml version=\"1.0\"?>\n<!DOCTYPE svg>\n<!-- c --><svg><rect width=\"1\"/></svg>";
        let out = svg_file_to_symbol("r", src).unwrap();
        assert_eq!(out, r#"<symbol id="r"><rect width="1" /></symbol>"#);
    }

    #[test]
    fn svg_file_to_symbol_empty_self_closing_root_yields_empty_symbol() {
        let src = r#"<svg xmlns="http://www.w3.org/2000/svg"/>"#;
        let out = svg_file_to_symbol("empty", src).unwrap();
        assert_eq!(out, r#"<symbol id="empty"></symbol>"#);
    }

    /// Fail-hard : un fichier sans balise racine `<svg>` doit échouer, pas
    /// produire un `<symbol>` vide silencieusement.
    #[test]
    fn svg_file_to_symbol_missing_svg_root_is_an_error() {
        let src = "<g><path/></g>";
        assert!(svg_file_to_symbol("icon", src).is_err());
    }

    /// Correctif pipeline sprites — le `viewBox` de la racine source doit
    /// être reporté sur `<symbol>`, sinon le repère de coordonnées du
    /// dessin est perdu et un `<use>` retombe sur les dimensions par
    /// défaut du navigateur.
    #[test]
    fn svg_file_to_symbol_reports_view_box_from_start_root() {
        let src = r#"<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024"><path d="M0 0"/></svg>"#;
        let out = svg_file_to_symbol("ampersand", src).unwrap();
        assert_eq!(
            out,
            r#"<symbol id="ampersand" viewBox="0 0 1024 1024"><path d="M0 0" /></symbol>"#
        );
    }

    /// Même correctif, racine auto-fermante (`<svg ... />`) : un symbole
    /// vide n'est pas la même chose qu'un symbole sans repère défini, le
    /// viewBox doit survivre même sans contenu.
    #[test]
    fn svg_file_to_symbol_reports_view_box_from_empty_root() {
        let src = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"/>"#;
        let out = svg_file_to_symbol("empty", src).unwrap();
        assert_eq!(out, r#"<symbol id="empty" viewBox="0 0 24 24"></symbol>"#);
    }

    /// Confirmation explicite de la non-régression : une racine sans
    /// `viewBox` ne doit toujours rien ajouter (comportement des tests
    /// existants ci-dessus, `xmlns` seul ne doit pas être confondu avec
    /// `viewBox`).
    #[test]
    fn svg_file_to_symbol_omits_view_box_attribute_when_absent_from_source() {
        let src = r#"<svg xmlns="http://www.w3.org/2000/svg"><path d="M0 0"/></svg>"#;
        let out = svg_file_to_symbol("icon", src).unwrap();
        assert!(!out.contains("viewBox"));
    }
}
