// crates/forge/fragment-forge/src/fragment/resolver.rs

//! Phase 2.1 — AOT Capacity Planner & I/O Resolver : résolution des
//! inclusions externes, mesure exacte de `STATIC_CAP`/`DYNAMIC_CAP`. Seule
//! phase du pipeline Fragment autorisée à faire de l'E/S disque.

use crate::fragment::token::FlatPageToken;
use crate::schema::SchemaIndex;
#[cfg(test)]
use crate::schema::{EscapePolicy, FieldKind, FieldSpec, VarlenField};

// =============================================================================
// Phase 2.1 — AOT Capacity Planner & I/O Resolver
// =============================================================================
//
// Responsabilité unique : résoudre les inclusions externes, muter l'AST en place,
// et calculer PAGE_STATIC_CAP (total_static_bytes).
//
// Frontières strictes :
//   - Mutation en place de &mut [FlatPageToken<'src>] : zéro nouvel arbre.
//   - Fail-slow : toutes les erreurs I/O sont accumulées avant de retourner Err.
//   - `get_file_size` est injecté : la phase est testable sans I/O réel.
//   - Field / IfBool / EndIf ne contribuent pas à PAGE_STATIC_CAP :
//     leur coût mémoire est runtime-dépendant (Phase 2.2).

/// Métriques calculées lors de la résolution AOT de l'AST.
///
/// `total_static_bytes` devient la constante `PAGE_STATIC_CAP` à la génération
/// de code. Elle représente la borne exacte des octets statiques, connue
/// analytiquement avant toute exécution du moteur de rendu.
#[derive(Debug, PartialEq, Eq)]
pub struct TemplateMetrics {
    /// Somme exacte en octets de tous les `Static` et `StaticInclude` résolus.
    pub total_static_bytes: usize,
    /// Somme des pires cas d'affichage de tous les champs Field du template.
    /// Fixed-length : FieldKind::max_display_width().
    /// Varlena : VarlenField::max_escaped_len() (facteur escape × max_len).
    pub total_dynamic_bytes: usize,
    /// Nombre de fichiers externes inclus résolus avec succès.
    pub include_count: usize,
}

/// Résultat de la résolution d'un `{% asset key %}` par la closure injectée
/// depuis `build.rs`. Pas un `Option<usize>` : le cas d'échec porte sa
/// propre suggestion diagnostique, calculée par l'appelant — seul à
/// posséder le registre d'assets (`fragment-forge` reste sans I/O, sans
/// connaissance du manifeste, invariant inchangé depuis la doc de
/// `resolve_asset_len` ci-dessous).
///
/// Test de substitution (retirer la fonctionnalité de suggestion) :
/// `Found(usize)` reste `Found(usize)` quoi qu'il arrive au diagnostic —
/// contrairement à un `Result<usize, String>` calqué sur `get_file_size`,
/// où le canal de succès aurait dû redevenir `Option<usize>` si un jour la
/// suggestion disparaissait. Le canal de succès n'est pas couplé au canal
/// diagnostique.
#[derive(Debug, PartialEq, Eq)]
pub enum AssetLookup {
    /// Clé trouvée dans le manifeste — longueur en octets de l'URL
    /// publique versionnée finale (voir doc de `resolve_asset_len`).
    Found(usize),
    /// Clé absente du manifeste. `suggestion`, si présente, est un message
    /// déjà formaté par l'appelant (casse différente ou plus proche
    /// voisin par distance d'édition) — `fragment-forge` ne la recalcule
    /// jamais, il n'a pas accès aux clés candidates pour le faire.
    NotFound { suggestion: Option<String> },
}

/// Erreur de résolution I/O produite par `resolve_and_measure`.
///
/// `path` emprunte `'src` depuis le token AST : zéro allocation pour
/// l'identifiant du fichier manquant. `details` est alloué : le message
/// d'erreur OS doit être formaté en `String` (build-time uniquement).
#[derive(Debug, PartialEq, Eq)]
pub enum ResolverError<'src> {
    /// Fichier inclus introuvable ou illisible.
    IoError { path: &'src str, details: String },
    /// Token Field ou IfBool référençant un champ absent du schéma.
    /// Erreur AOT fatale : cargo:error dans build.rs.
    UnknownField { entity: &'src str, field: &'src str },
    /// Champ varlena référencé par le template mais sans borne connue
    /// (`VarlenField.max_len == None`) — ADR-007, disjoncteur Hot/Cold/Erreur.
    ///
    /// Distinct de `UnknownField` : le champ existe bien dans le schéma
    /// PostgreSQL (visible dans `SchemaIndex.varlena`), mais aucune contrainte
    /// exploitable (VARCHAR(N) ou CHECK reconnu) ne borne sa longueur. Un champ
    /// non référencé avec la même absence de borne ne produit jamais cette
    /// erreur — il reste Cold, invisible au calcul de capacité.
    UnboundedField { entity: &'src str, field: &'src str },
    /// `{% asset key %}` référençant une clé absente du manifeste d'assets
    /// produit par `marius-assets` — spec §9 et §11 : `AssetNotFound` est un
    /// échec fatal de compilation, jamais un repli ou une résolution
    /// dynamique différée au runtime.
    ///
    /// `suggestion` transporte, sans le recalculer, le diagnostic déjà
    /// produit par `AssetLookup::NotFound` — `None` signifie qu'aucun
    /// candidat plausible n'a été trouvé dans le manifeste, pas qu'aucune
    /// tentative n'a eu lieu.
    AssetNotFound {
        key: &'src str,
        suggestion: Option<String>,
    },
}

/// Résout les inclusions, mute l'AST en place, calcule les métriques statiques.
///
/// # Stratégie fail-slow
/// Toutes les erreurs I/O sont accumulées avant de retourner `Err`.
/// Un build référençant N fichiers manquants remonte N erreurs en une seule passe.
///
/// # Mutation en place
/// `StaticInclude::len` passe de `0` (valeur provisoire Phase 1.3) à la taille
/// réelle — sans créer de nouvel arbre, sans déplacer l'AST.
///
/// # Allocation conditionnelle
/// `Vec::new()` n'alloue pas de bloc heap avant le premier `push`.
/// Un projet sans fichier manquant traverse cette fonction sans allocation.
///
/// # Résolution des assets
/// `resolve_asset_len` : injectée par `build.rs` après lecture du manifeste
/// d'assets — jamais d'I/O ici. Retourne la longueur en octets de l'URL
/// publique versionnée finale (celle qui sera effectivement écrite par
/// `generate_aot_snippet`), pas la taille du fichier lui-même : c'est ce
/// nombre d'octets, et lui seul, qui contribue à `PAGE_STATIC_CAP`.
/// `AssetLookup::NotFound` signifie clé absente du manifeste →
/// `ResolverError::AssetNotFound`, jamais une longueur par défaut devinée
/// — la suggestion diagnostique éventuelle est transportée telle quelle,
/// `fragment-forge` ne la recalcule jamais (il n'a pas les clés
/// candidates pour le faire, seul `build.rs` les possède).
pub fn resolve_and_measure<'src>(
    tokens: &mut [FlatPageToken<'src>],
    schema: &SchemaIndex<'_>,
    get_file_size: impl Fn(&str) -> Result<usize, String>,
    resolve_asset_len: impl Fn(&str) -> AssetLookup,
    // Contribution en octets de ModulesPlaceholder, calculée intégralement
    // par `build.rs` (voir doc du variant) — pire cas : somme des snippets
    // de TOUTES les capacités actives, jamais une hypothèse d'exclusivité
    // mutuelle entre bits du bitset `js_deps`. `0` pour tout appelant dont
    // le flux ne contient jamais ce token (Mode Fragment isolé,
    // STATIC_PAGES, tests sans capacité) — valeur alors simplement inerte.
    modules_static_bytes: usize,
) -> Result<TemplateMetrics, Vec<ResolverError<'src>>> {
    let mut metrics = TemplateMetrics {
        total_static_bytes: 0,
        total_dynamic_bytes: 0,
        include_count: 0,
    };
    let mut errors: Vec<ResolverError<'src>> = Vec::new();

    for token in tokens.iter_mut() {
        match token {
            // Octets HTML connus statiquement : contribution directe.
            FlatPageToken::Static(s) => {
                metrics.total_static_bytes += s.len();
            }

            // Inclusion externe : résolution I/O et mutation en place.
            FlatPageToken::StaticInclude {
                rel_from_manifest,
                len,
                ..
            } => {
                let path = *rel_from_manifest;
                match get_file_size(path) {
                    Ok(size) => {
                        *len = size;
                        metrics.total_static_bytes += size;
                        metrics.include_count += 1;
                    }
                    Err(details) => {
                        errors.push(ResolverError::IoError { path, details });
                    }
                }
            }

            // Champ dynamique : disjoncteur Hot / Cold / Erreur (ADR-007).
            //
            // Table de vérité (un champ varlena est visité ici uniquement s'il
            // est référencé par l'AST — un champ jamais référencé n'entre jamais
            // dans cette branche, il reste Cold par construction, sans code dédié) :
            //
            //   référencé + fixed-length            → Hot, max_display_width()
            //   référencé + varlena, max_len=Some(n) → Hot, max_escaped_len()
            //   référencé + varlena, max_len=None    → Erreur, UnboundedField
            //   absent du schéma (ni fixed ni varlena) → Erreur, UnknownField
            FlatPageToken::Field { entity, field } => {
                if let Some(f) = schema.find_fixed(field) {
                    metrics.total_dynamic_bytes += f.kind.max_display_width();
                } else if let Some(v) = schema.find_varlena(field) {
                    match v.max_escaped_len() {
                        Some(n) => metrics.total_dynamic_bytes += n,
                        None => errors.push(ResolverError::UnboundedField { entity, field }),
                    }
                } else {
                    errors.push(ResolverError::UnknownField { entity, field });
                }
            }

            // Bloc conditionnel : validation schéma uniquement.
            // Le champ sert de condition booléenne, il n'est pas affiché —
            // pas de contribution à total_dynamic_bytes.
            FlatPageToken::IfBool { entity, field } => {
                if schema.find_fixed(field).is_none() {
                    errors.push(ResolverError::UnknownField { entity, field });
                }
            }

            // EndIf : aucun effet sur les métriques.
            FlatPageToken::EndIf => {}

            // ScriptStart/ScriptEnd : marqueurs purs, aucune contribution
            // propre — comme EndIf. Le contenu CAPTURÉ entre les deux (ses
            // propres tokens Static/AssetRef) est mesuré normalement par
            // ses propres tokens, que la capture ait lieu ou non en aval
            // (hoisting Page vs passthrough Fragment isolé, décidé par
            // build.rs) — cette fonction n'a pas à le savoir.
            FlatPageToken::ScriptStart | FlatPageToken::ScriptEnd => {}

            // Asset : longueur de l'URL résolue, jamais celle du fichier
            // source — même famille que Static/StaticInclude (contribution
            // directe à total_static_bytes), aucune mutation en place (voir
            // doc de la variante : pas de champ provisoire à patcher ici).
            FlatPageToken::AssetRef(key) => match resolve_asset_len(key) {
                AssetLookup::Found(len) => metrics.total_static_bytes += len,
                AssetLookup::NotFound { suggestion } => {
                    errors.push(ResolverError::AssetNotFound { key, suggestion });
                }
            },

            // Contribution directe, valeur fournie par l'appelant — même
            // famille que Static/AssetRef, aucune mutation en place (le
            // token ne porte aucun champ propre à patcher).
            FlatPageToken::ModulesPlaceholder => {
                metrics.total_static_bytes += modules_static_bytes;
            }
        }
    }

    if errors.is_empty() {
        Ok(metrics)
    } else {
        Err(errors)
    }
}

// =============================================================================
// Tests — Phase 2.1
// =============================================================================

#[cfg(test)]
mod tests_phase_2_1 {
    use super::{
        AssetLookup, EscapePolicy, FieldKind, FieldSpec, FlatPageToken, ResolverError, SchemaIndex,
        TemplateMetrics, VarlenField, resolve_and_measure,
    };

    /// Construit un StaticInclude avec len = 0 (valeur provisoire Phase 1.3).
    /// Les deux paths sont identiques : l'orchestrateur n'a pas encore calculé
    /// le chemin relatif au manifest.
    fn make_include(path: &str) -> FlatPageToken<'_> {
        FlatPageToken::StaticInclude {
            original_path: path,
            rel_from_manifest: path,
            len: 0,
        }
    }

    /// Vérifie le chemin heureux : mutation en place + métriques correctes.
    ///
    /// total_static_bytes = 6 (Static "<html>") + 10 (StaticInclude "a.html") = 16.
    #[test]
    fn test_resolve_success() {
        let mut tokens = vec![FlatPageToken::Static("<html>"), make_include("a.html")];

        let schema = SchemaIndex {
            fixed: &[],
            varlena: &[],
        };
        let result = resolve_and_measure(
            &mut tokens,
            &schema,
            |path| match path {
                "a.html" => Ok(10),
                other => Err(format!("unknown : {other}")),
            },
            |_| unreachable!("aucun AssetRef dans ce test"),
            0, // aucune capacité js_deps dans cette fixture
        );

        // Métriques correctes.
        assert_eq!(
            result,
            Ok(TemplateMetrics {
                total_static_bytes: 16,
                total_dynamic_bytes: 0,
                include_count: 1
            }),
        );

        // Preuve de mutation en place : len vaut 10, pas 0.
        // Les slices &'src str sont inchangées — seul le scalaire `len` a évolué.
        match &tokens[1] {
            FlatPageToken::StaticInclude {
                len,
                original_path,
                rel_from_manifest,
            } => {
                assert_eq!(*len, 10, "len doit être muté de 0 à 10");
                assert_eq!(*original_path, "a.html", "original_path inchangé");
                assert_eq!(*rel_from_manifest, "a.html", "rel_from_manifest inchangé");
            }
            _ => panic!("tokens[1] doit être un StaticInclude"),
        }
    }

    /// Vérifie l'accumulation fail-slow des erreurs I/O.
    ///
    /// "a.html" est résolu avec succès (mutation effective à len=10).
    /// "missing.html" échoue (IoError accumulé).
    /// Le retour est Err même si une résolution partielle a eu lieu :
    /// la capacité PAGE_STATIC_CAP ne peut pas être garantie si un fichier
    /// inclus est absent.
    ///
    /// Note : la mutation de "a.html" est vérifiable même en cas d'Err —
    /// preuve que le parcours ne s'est pas arrêté à la première erreur.
    #[test]
    fn test_resolve_partial_error() {
        let mut tokens = vec![
            FlatPageToken::Static("<html>"),
            make_include("a.html"),
            make_include("missing.html"),
        ];

        let schema = SchemaIndex {
            fixed: &[],
            varlena: &[],
        };
        let result = resolve_and_measure(
            &mut tokens,
            &schema,
            |path| match path {
                "a.html" => Ok(10),
                other => Err(format!("introuvable : {other}")),
            },
            |_| unreachable!("aucun AssetRef dans ce test"),
            0, // aucune capacité js_deps dans cette fixture
        );

        // Exactement 1 erreur accumulée.
        let errors = result.expect_err("doit retourner Err pour un fichier manquant");
        assert_eq!(errors.len(), 1, "exactement 1 IoError attendu");

        match &errors[0] {
            ResolverError::IoError { path, details } => {
                assert_eq!(*path, "missing.html");
                assert!(
                    details.contains("missing.html"),
                    "details doit identifier le fichier : {details:?}",
                );
            }
            ResolverError::UnknownField { entity, field } => {
                panic!("UnknownField inattendu dans ce test : {entity}.{field}");
            }
            ResolverError::UnboundedField { entity, field } => {
                panic!("UnboundedField inattendu dans ce test : {entity}.{field}");
            }
            ResolverError::AssetNotFound { key, .. } => {
                panic!("AssetNotFound inattendu dans ce test : {key}");
            }
        }

        // Invariant fail-slow : "a.html" a bien été muté malgré l'erreur suivante.
        match &tokens[1] {
            FlatPageToken::StaticInclude { len, .. } => {
                assert_eq!(
                    *len, 10,
                    "la mutation de a.html doit survivre à l'erreur partielle"
                );
            }
            _ => panic!("tokens[1] doit rester un StaticInclude"),
        }
    }

    /// Cas limite : AST sans inclusion.
    /// `get_file_size` ne doit jamais être appelé — vérifié par `unreachable!`.
    /// total_static_bytes = 3 ("<p>") + 4 ("</p>") = 7.
    /// Field contribue désormais à total_dynamic_bytes (Phase 3 — SchemaIndex).
    #[test]
    fn test_resolve_no_includes() {
        let mut tokens = vec![
            FlatPageToken::Static("<p>"),
            FlatPageToken::Field {
                entity: "user",
                field: "name",
            },
            FlatPageToken::Static("</p>"),
        ];

        let fixed = vec![FieldSpec {
            name: "name".to_string(),
            kind: FieldKind::I32,
            attnum: 1,
        }];
        let schema = SchemaIndex {
            fixed: &fixed,
            varlena: &[],
        };

        assert_eq!(
            resolve_and_measure(
                &mut tokens,
                &schema,
                |_| unreachable!("get_file_size ne doit pas être appelé sans StaticInclude"),
                |_| unreachable!("aucun AssetRef dans ce test"),
                0, // aucune capacité js_deps dans cette fixture
            ),
            Ok(TemplateMetrics {
                total_static_bytes: 7,
                total_dynamic_bytes: FieldKind::I32.max_display_width(),
                include_count: 0,
            }),
        );
    }

    // =========================================================================
    // Disjoncteur Hot / Cold / Erreur — ADR-007
    //
    // Table de vérité complète d'un champ varlena, les trois lignes possibles.
    // Zéro dépendance PostgreSQL : SchemaIndex est construit à la main, comme
    // tous les autres tests de ce module. L'invariant central de l'ADR — "un
    // champ non borné référencé par une projection provoque un échec explicite
    // de résolution" — est une propriété pure de resolve_and_measure, qui ne
    // dépend en rien de la fidélité de l'introspection PostgreSQL elle-même
    // (fetch_varlena_cols, hors périmètre de ces trois tests).
    // =========================================================================

    fn unbounded_field(name: &str) -> VarlenField {
        VarlenField {
            name: name.to_string(),
            // Provenance non pertinente pour ces tests (Hot/Cold/Erreur, pas
            // qualification SQL) — placeholder neutre, jamais lu par
            // resolve_and_measure ni par les assertions de ce module.
            ref_schema: "test".to_string(),
            ref_table: "joined".to_string(),
            max_len: None,
            escape_policy: EscapePolicy::Escaped,
            is_segment: false,
            nullable: true,
            max_escaped_len_override: None,
        }
    }

    fn bounded_field(name: &str, max_len: usize) -> VarlenField {
        VarlenField {
            name: name.to_string(),
            ref_schema: "test".to_string(),
            ref_table: "joined".to_string(),
            max_len: Some(max_len),
            escape_policy: EscapePolicy::Escaped,
            is_segment: false,
            nullable: true,
            max_escaped_len_override: None,
        }
    }

    /// Ligne 1 de la table de vérité : champ non borné, RÉFÉRENCÉ par l'AST.
    /// → Err([UnboundedField]). C'est l'invariant central de l'ADR-007 :
    /// un champ sans borne connue ne peut jamais contribuer silencieusement
    /// à total_dynamic_bytes — la compilation doit échouer explicitement.
    #[test]
    fn unbounded_field_referenced_fails_resolution() {
        let mut tokens = vec![FlatPageToken::Field {
            entity: "record",
            field: "description",
        }];
        let varlena = vec![unbounded_field("description")];
        let schema = SchemaIndex {
            fixed: &[],
            varlena: &varlena,
        };

        let result = resolve_and_measure(
            &mut tokens,
            &schema,
            |_| unreachable!("aucun StaticInclude dans ce test"),
            |_| unreachable!("aucun AssetRef dans ce test"),
            0, // aucune capacité js_deps dans cette fixture
        );

        assert_eq!(
            result,
            Err(vec![ResolverError::UnboundedField {
                entity: "record",
                field: "description"
            }]),
        );
    }

    /// Ligne 2 de la table de vérité : champ borné, RÉFÉRENCÉ par l'AST.
    /// → Ok, contribue normalement à total_dynamic_bytes via max_escaped_len().
    /// Comportement Hot — inchangé depuis avant ADR-007, vérifié explicitement
    /// pour garantir qu'il n'a pas régressé avec le passage à Option<usize>.
    #[test]
    fn bounded_field_referenced_contributes_normally() {
        let mut tokens = vec![FlatPageToken::Field {
            entity: "record",
            field: "headline",
        }];
        let varlena = vec![bounded_field("headline", 100)];
        let schema = SchemaIndex {
            fixed: &[],
            varlena: &varlena,
        };

        let metrics = resolve_and_measure(
            &mut tokens,
            &schema,
            |_| unreachable!("aucun StaticInclude dans ce test"),
            |_| unreachable!("aucun AssetRef dans ce test"),
            0, // aucune capacité js_deps dans cette fixture
        )
        .expect("champ borné référencé : résolution attendue en succès");

        assert_eq!(
            metrics.total_dynamic_bytes,
            100 * VarlenField::HTML_ESCAPE_FACTOR
        );
        assert_eq!(metrics.total_static_bytes, 0);
    }

    /// Ligne 3 de la table de vérité : champ non borné, JAMAIS référencé.
    /// → Ok, comportement Cold. Le champ existe dans SchemaIndex.varlena
    /// (visible, comme le produit désormais fetch_varlena_cols depuis ADR-007 —
    /// il n'est plus exclu du Vec en amont) mais n'apparaît dans aucune erreur
    /// ni dans total_dynamic_bytes, précisément parce que l'AST ne le mentionne
    /// jamais. Seule la conjonction "non borné + référencé" déclenche l'erreur —
    /// "non borné" seul ne suffit jamais.
    #[test]
    fn unbounded_field_not_referenced_is_cold() {
        let mut tokens = vec![FlatPageToken::Static("<article></article>")];
        let varlena = vec![unbounded_field("description")];
        let schema = SchemaIndex {
            fixed: &[],
            varlena: &varlena,
        };

        let metrics = resolve_and_measure(
            &mut tokens,
            &schema,
            |_| unreachable!("aucun StaticInclude dans ce test"),
            |_| unreachable!("aucun AssetRef dans ce test"),
            0, // aucune capacité js_deps dans cette fixture
        )
        .expect("champ Cold non référencé : résolution attendue en succès");

        assert_eq!(
            metrics.total_dynamic_bytes, 0,
            "champ Cold ne doit jamais contribuer"
        );
        assert_eq!(metrics.total_static_bytes, 19); // "<article></article>".len()
    }

    // =========================================================================
    // `{% asset key %}` — AssetLookup / ResolverError::AssetNotFound.
    //
    // Ces trois tests documentent un invariant précis, pas seulement le
    // chemin heureux : `fragment-forge` transporte la `suggestion` fournie
    // par la closure `resolve_asset_len` TELLE QUELLE — il ne la calcule
    // jamais, ne la reformate jamais, ne la remplace jamais par `None` par
    // défaut. Le calcul réel (casse insensible, Levenshtein) vit côté
    // `build.rs` (`resolve_asset_lookup`/`suggest_asset_key`), hors de
    // portée de ce crate — ces tests n'exercent donc délibérément que le
    // passage à travers `resolve_and_measure`, pas l'algorithme de
    // suggestion lui-même (qui n'existe pas ici).
    // =========================================================================

    /// Chemin heureux : clé résolue, contribue à `total_static_bytes` au
    /// même titre qu'un `Static`/`StaticInclude` — jamais à
    /// `total_dynamic_bytes` (spec §9 : une URL versionnée est un segment
    /// figé au moment de la génération, pas une valeur runtime).
    #[test]
    fn asset_ref_found_contributes_static_bytes() {
        let mut tokens = vec![
            FlatPageToken::Static("<link href=\""),
            FlatPageToken::AssetRef("main.css"),
            FlatPageToken::Static("\">"),
        ];
        let schema = SchemaIndex {
            fixed: &[],
            varlena: &[],
        };

        let metrics = resolve_and_measure(
            &mut tokens,
            &schema,
            |_| unreachable!("aucun StaticInclude dans ce test"),
            |key| {
                assert_eq!(key, "main.css");
                AssetLookup::Found(20) // longueur de l'URL versionnée, pas du fichier
            },
            0, // aucune capacité js_deps dans cette fixture
        )
        .expect("clé présente : résolution attendue en succès");

        assert_eq!(metrics.total_static_bytes, 12 + 20 + 2);
        assert_eq!(
            metrics.total_dynamic_bytes, 0,
            "un asset résolu ne doit jamais contribuer à total_dynamic_bytes"
        );
    }

    /// Clé absente AVEC suggestion : `ResolverError::AssetNotFound` doit
    /// porter la `suggestion` fournie par la closure mot pour mot — aucune
    /// transformation, aucun recalcul. C'est le contrat qui permet à
    /// `build.rs` de calculer le diagnostic (il a les clés candidates,
    /// `fragment-forge` ne les a jamais).
    #[test]
    fn asset_not_found_carries_supplied_suggestion_unchanged() {
        let mut tokens = vec![FlatPageToken::AssetRef("util.svg")];
        let schema = SchemaIndex {
            fixed: &[],
            varlena: &[],
        };

        let result = resolve_and_measure(
            &mut tokens,
            &schema,
            |_| unreachable!("aucun StaticInclude dans ce test"),
            |key| {
                assert_eq!(key, "util.svg");
                AssetLookup::NotFound {
                    suggestion: Some("vouliez-vous dire 'utils.svg' ?".to_string()),
                }
            },
            0, // aucune capacité js_deps dans cette fixture
        );

        let errors = result.expect_err("clé absente : Err attendu");
        assert_eq!(errors.len(), 1);
        match &errors[0] {
            ResolverError::AssetNotFound { key, suggestion } => {
                assert_eq!(*key, "util.svg");
                assert_eq!(
                    suggestion.as_deref(),
                    Some("vouliez-vous dire 'utils.svg' ?"),
                    "la suggestion doit traverser resolve_and_measure sans altération"
                );
            }
            other => panic!("AssetNotFound attendu, obtenu : {other:?}"),
        }
    }

    /// Clé absente SANS suggestion plausible : `suggestion` doit rester
    /// `None`, jamais remplacé par un texte générique fabriqué par
    /// `fragment-forge` — ce cas (aucun candidat proche dans le manifeste,
    /// ex. `silos/195v.svg`) doit rester silencieusement `None` jusqu'au
    /// site d'affichage, qui décide seul comment le présenter.
    #[test]
    fn asset_not_found_without_suggestion_stays_none() {
        let mut tokens = vec![FlatPageToken::AssetRef("silos/195v.svg")];
        let schema = SchemaIndex {
            fixed: &[],
            varlena: &[],
        };

        let result = resolve_and_measure(
            &mut tokens,
            &schema,
            |_| unreachable!("aucun StaticInclude dans ce test"),
            |_| AssetLookup::NotFound { suggestion: None },
            0, // aucune capacité js_deps dans cette fixture
        );

        let errors = result.expect_err("clé absente : Err attendu");
        match &errors[0] {
            ResolverError::AssetNotFound { key, suggestion } => {
                assert_eq!(*key, "silos/195v.svg");
                assert_eq!(
                    *suggestion, None,
                    "aucun candidat plausible : None attendu, pas un texte générique"
                );
            }
            other => panic!("AssetNotFound attendu, obtenu : {other:?}"),
        }
    }
}
