// crates/forge/fragment-forge/src/fragment/codegen.rs

//! Phase 2.2 — Générateur AOT (transpileur) : `&[FlatPageToken]` → `String`
//! de code Rust. Aucune validation sémantique ici (AST supposé correct,
//! Phases 1.3+1.4). Contient aussi l'en-tête du fichier généré (fonction
//! `marius_html_escape` inline, zéro dépendance externe côté runtime).

use crate::fragment::token::FlatPageToken;
use crate::schema::{EscapePolicy, SchemaIndex};
#[cfg(test)]
use crate::schema::{FieldKind, FieldSpec, VarlenField};

// =============================================================================
// III. Génération du corps de render()
// =============================================================================

// =============================================================================
// V. En-tête du fichier généré
// =============================================================================

/// Retourne l'en-tête injecté en tête de generated_schema.rs.
///
/// Contient la fonction marius_html_escape(), inline dans le fichier généré
/// pour éviter toute dépendance externe depuis le chemin critique.
///
/// ─── Politique d'escape ──────────────────────────────────────────────────────
///
///   Seuls les 5 caractères dangereux en HTML sont transformés :
///     '&'  → "&amp;"   (doit être premier pour éviter le double-escape)
///     '<'  → "&lt;"
///     '>'  → "&gt;"
///     '"'  → "&quot;"  (attributs HTML)
///     '\'' → "&#39;"   (attributs non-quotés)
///
///   Tous les autres caractères (Unicode inclus) sont émis tels quels via push(ch).
///   L'itérateur chars() garantit que les séquences multi-octets UTF-8 sont
///   traitées correctement sans risque de corruption de la représentation.
///
/// ─── Invariant no-alloc ──────────────────────────────────────────────────────
///
///   marius_html_escape() n'alloue pas. Elle écrit dans buf (déjà réservé).
///   Si buf a été pré-alloué avec STATIC_CAP + DYNAMIC_CAP, et que VarlenField
///   a été configuré avec le bon max_escaped_len(), aucun realloc ne peut survenir.
///
/// ─── Absence de use std::path::PathBuf ───────────────────────────────────────
///
///   PathBuf est importé ici pour artifact_path() généré dans le même fichier.
///   Cet import couvre l'ensemble du module généré.
pub fn generated_file_header() -> &'static str {
    "// GÉNÉRÉ PAR DB-FORGE + FRAGMENT-FORGE — NE PAS MODIFIER MANUELLEMENT\n\
     // Régénérer via : cargo build (relit pg_attribute + pg_description)\n\n\
     use std::path::PathBuf;\n\
     // Import du trait Projection dans le scope du fichier généré.\n\
     // Requis pour que fetch_batch() et render() soient résolus sur les types\n\
     // de projection générés, aussi bien dans le code appelant que dans les tests.\n\
     #[allow(unused_imports)]\n\
     use crate::projection::Projection as _;\n\n\
     /// Échappe les caractères HTML dangereux dans `s` et pousse le résultat dans `buf`.\n\
     ///\n\
     /// Zéro allocation : opère directement sur buf (déjà réservé par render()).\n\
     /// Ordre des branches : '&' en premier pour éviter le double-escape de '&amp;'.\n\
     #[inline(always)]\n\
     fn marius_html_escape(s: &str, buf: &mut String) {\n\
         for ch in s.chars() {\n\
             match ch {\n\
                 '&'  => buf.push_str(\"&amp;\"),\n\
                 '<'  => buf.push_str(\"&lt;\"),\n\
                 '>'  => buf.push_str(\"&gt;\"),\n\
                 '\"' => buf.push_str(\"&quot;\"),\n\
                 '\\'' => buf.push_str(\"&#39;\"),\n\
                 _    => buf.push(ch),\n\
             }\n\
         }\n\
     }\n\n"
}

// =============================================================================
// Phase 2.2 — Générateur AOT (Transpileur)
// =============================================================================
//
// Responsabilité unique : transpiler &[FlatPageToken<'src>] → String de code Rust.
//
// Frontières strictes :
//   - Aucune validation sémantique ici. L'AST est supposé correct (Phases 1.3+1.4).
//   - L'indentation est plate (2 niveaux max) : garanti par l'invariant Phase 1.4.
//   - Le code généré est autonome : `buf`, les variables d'entité et leurs champs
//     sont supposés dans le scope de la fonction encapsulante (build.rs).
//   - `{:?}` sur &str délègue l'échappement au Debug de Rust.
//     Zéro escaper maison. Résultat : un littéral Rust syntaxiquement valide.
//
// Invariant de pré-allocation (DOD) :
//   La première instruction du snippet est toujours `buf.reserve(N)`.
//   N = metrics.total_static_bytes (mesuré exactement en Phase 2.1).
//   Cette instruction garantit que le vecteur sous-jacent au `buf: &mut String`
//   du runtime ne réalloue jamais pour les octets HTML statiques.

/// Transpile l'AST en un bloc d'instructions Rust natif.
///
/// N'émet PAS `buf.reserve()` — c'est la responsabilité de l'orchestrateur
/// qui référence PAGE_TOTAL_CAP (calculé depuis les métriques).
///
/// Délègue le choix d'émission à SchemaIndex :
///   Field fixe   → write_fmt (pas d'allocation).
///   Field varlena → html_escape via ref locale as_deref().
///   IfBool        → `if record.field != 0` (u8 dans StorageRow, pas bool).
///
/// # Résolution des assets
/// `resolve_asset_url` : supposée infaillible à ce stade — toute clé absente
/// du manifeste a déjà fait échouer la compilation via
/// `ResolverError::AssetNotFound` dans `resolve_and_measure`, appelé
/// obligatoirement avant cette fonction (même précédent que `StaticInclude`,
/// dont l'existence est vérifiée par `get_file_size` avant que
/// `include_str!` ne soit émis ici). Un panic ici signale une violation de
/// cet ordonnancement par l'appelant (`build.rs`), jamais une clé
/// utilisateur invalide.
///
/// `'r` distinct de `'src` et de la lifetime (anonyme, par argument) de
/// `key` dans la closure : sans ce paramètre nommé, `impl Fn(&str) -> &str`
/// s'élide en `for<'a> Fn(&'a str) -> &'a str` (HRTB — la sortie liée à
/// l'entrée). Une closure réelle capturant `&HashMap` (build.rs) renvoie un
/// emprunt sur la durée de vie de la map, jamais sur celle de `key` : elle
/// ne peut satisfaire cette borne que si la map vit `'static`, ce qui n'est
/// pas le cas. `'r` découple la sortie de l'entrée et se résout, à l'appel,
/// sur la durée de vie réelle capturée par la closure.
pub fn generate_aot_snippet<'src, 'r>(
    tokens: &[FlatPageToken<'src>],
    schema: &SchemaIndex<'_>,
    resolve_asset_url: impl Fn(&str) -> &'r str,
    // Code Rust déjà assemblé par `build.rs` pour ModulesPlaceholder — une
    // ligne `if record.js_deps & BIT != 0 { buf.push_str(...); }` par
    // capacité active, chaîne vide si aucune. Inséré verbatim (ce N'EST PAS
    // un littéral à échapper comme AssetRef/StaticInclude : c'est déjà du
    // code source, pas une valeur) — voir doc du variant.
    modules_snippet: &str,
) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(25 + tokens.len() * 60);

    // ── Déclarations de références varlena ────────────────────────────────────
    let mut varlena_seen: Vec<&str> = tokens
        .iter()
        .filter_map(|t| match t {
            FlatPageToken::Field { field, .. } if schema.find_varlena(field).is_some() => {
                Some(*field)
            }
            _ => None,
        })
        .collect();
    varlena_seen.sort_unstable();
    varlena_seen.dedup();
    for name in &varlena_seen {
        writeln!(
            out,
            "let {name}_ref: Option<&str> = varlena.{name}.as_deref();"
        )
        .unwrap();
    }

    let mut indent: &str = "";

    for token in tokens {
        match token {
            FlatPageToken::Static(s) => {
                if s.len() == 1 {
                    let c = s.chars().next().unwrap();
                    writeln!(out, "{}buf.push({:?});", indent, c).unwrap();
                } else {
                    writeln!(out, "{}buf.push_str({:?});", indent, s).unwrap();
                }
            }

            FlatPageToken::Field { field, .. } => {
                if let Some(v) = schema.find_varlena(field) {
                    // CONTRAT-implementation-varlena-raw.md, Étape 4 : match
                    // exhaustif sur EscapePolicy — Raw ne passe JAMAIS par
                    // marius_html_escape (contenu HTML déjà constitué, à
                    // injecter tel quel) ; Escaped et PreEscaped conservent le
                    // comportement existant (l'échappement runtime ne dépend
                    // que du contenu réel étant du texte, pas de la capacité
                    // déclarée — seul PreEscaped change le facteur de
                    // capacité, jamais le comportement d'échappement lui-même).
                    match v.escape_policy {
                        EscapePolicy::Raw => {
                            writeln!(
                                out,
                                "{}if let Some(s) = {field}_ref {{ buf.push_str(s); }}",
                                indent,
                            )
                            .unwrap();
                        }
                        EscapePolicy::Escaped | EscapePolicy::PreEscaped => {
                            writeln!(
                                out,
                                "{}if let Some(s) = {field}_ref {{ marius_html_escape(s, buf); }}",
                                indent,
                            )
                            .unwrap();
                        }
                    }
                } else {
                    writeln!(
                        out,
                        r#"{}::std::fmt::Write::write_fmt(buf, format_args!("{{}}", record.{field})).ok();"#,
                        indent,
                    ).unwrap();
                }
            }

            FlatPageToken::IfBool { field, .. } => {
                // u8 dans StorageRow (bytemuck::Pod interdit bool).
                writeln!(out, "{}if record.{field} != 0 {{", indent).unwrap();
                indent = "    ";
            }

            FlatPageToken::EndIf => {
                indent = "";
                out.push_str("}\n");
            }

            // ScriptStart/ScriptEnd : jamais émis eux-mêmes (No-Op pur,
            // mission §2, cible Fragment isolé) — le contenu capturé entre
            // les deux continue d'être émis normalement à sa position
            // d'origine par ses propres tokens si `hoist_and_dedupe_scripts`
            // n'a pas tourné en amont (build.rs, jamais ici).
            FlatPageToken::ScriptStart | FlatPageToken::ScriptEnd => {}

            FlatPageToken::StaticInclude {
                rel_from_manifest, ..
            } => {
                writeln!(
                    out,
                    "{}buf.push_str(include_str!({:?}));",
                    indent, rel_from_manifest,
                )
                .unwrap();
            }

            // Asset : URL versionnée gravée en dur, exactement comme un
            // segment Static — zéro indirection, zéro allocation au runtime
            // (spec §9). Pas d'`include_str!` : ce n'est pas un contenu de
            // fichier à inliner, c'est une chaîne déjà connue au moment de
            // la génération.
            FlatPageToken::AssetRef(key) => {
                let url = resolve_asset_url(key);
                writeln!(out, "{}buf.push_str({:?});", indent, url).unwrap();
            }

            // Insertion verbatim — `modules_snippet` est déjà du code Rust
            // complet (0 à N lignes `if record.js_deps & BIT != 0 { ... }`),
            // jamais une valeur à formater/échapper comme les autres
            // variantes de cette fonction.
            FlatPageToken::ModulesPlaceholder => {
                out.push_str(modules_snippet);
            }
        }
    }

    out
}

/// Génère le corps de `render_segments()` pour un composant portant au moins
/// un champ `is_segment == true` — CONTRAT-implementation-projection-
/// segmentee.md, Étape 5. Appelée par `build.rs` à la place de
/// `generate_aot_snippet` uniquement quand `varlena.iter().any(|v|
/// v.is_segment)` — jamais les deux pour le même composant.
///
/// ── Algorithme (arbitré en session, 23/07/2026) ───────────────────────────
///
/// Identique à `generate_aot_snippet` pour tout token qui n'est pas un champ
/// `is_segment` — même émission `buf.push_str`/`marius_html_escape`/etc.,
/// dans `buf`. La seule différence : un champ `is_segment` clôt le « run »
/// `Buffered` courant (`segments.push(Segment::Buffered { start, end })`),
/// pousse sa valeur comme `Segment::Borrowed` autonome (jamais concaténée
/// dans `buf`), puis rouvre un nouveau run pour ce qui suit.
///
/// `seg_start` est une variable Rust générée, déclarée une seule fois en tête
/// de fonction (`let mut seg_start: usize = buf.len();` — vaut 0 en pratique,
/// `buf` arrivant vide par contrat, mais recalculé dynamiquement plutôt que
/// supposé pour rester robuste à toute évolution future du contrat), puis
/// réassignée (jamais re-`let`) à chaque réouverture de run — y compris à
/// l'intérieur d'un bloc `{% if %}` généré : la réassignation à l'intérieur
/// d'un bloc conditionnel est correcte par construction, puisque le bloc
/// entier est sauté à l'exécution si la condition est fausse, laissant
/// `seg_start` intact avec sa valeur d'avant le bloc — le run englobant se
/// poursuit alors sans discontinuité, exactement comme si le champ segmenté
/// n'existait pas pour cet enregistrement.
///
/// Ce raisonnement a été vérifié à la main sur le cas d'un champ segmenté
/// unique à l'intérieur d'un `{% if %}` avant d'écrire cette fonction — les
/// deux branches d'exécution (condition vraie/fausse) produisent un état de
/// `segments` cohérent dans les deux cas.
pub fn generate_segmented_snippet<'src, 'r>(
    tokens: &[FlatPageToken<'src>],
    schema: &SchemaIndex<'_>,
    resolve_asset_url: impl Fn(&str) -> &'r str,
    // Voir doc du paramètre homonyme de `generate_aot_snippet` — même
    // contrat : code Rust déjà assemblé, inséré verbatim, jamais une valeur
    // à formater.
    modules_snippet: &str,
) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(25 + tokens.len() * 60);

    // ── Déclarations de références varlena — identique à generate_aot_snippet ──
    let mut varlena_seen: Vec<&str> = tokens
        .iter()
        .filter_map(|t| match t {
            FlatPageToken::Field { field, .. } if schema.find_varlena(field).is_some() => {
                Some(*field)
            }
            _ => None,
        })
        .collect();
    varlena_seen.sort_unstable();
    varlena_seen.dedup();
    for name in &varlena_seen {
        writeln!(
            out,
            "let {name}_ref: Option<&str> = varlena.{name}.as_deref();"
        )
        .unwrap();
    }

    // Ouverture du premier run — toujours à la racine de la fonction, jamais
    // à l'intérieur d'un bloc généré (les tokens IfBool ne peuvent apparaître
    // qu'après ce point dans la boucle ci-dessous).
    writeln!(out, "let mut seg_start: usize = buf.len();").unwrap();

    let mut indent = String::new();

    for token in tokens {
        match token {
            FlatPageToken::Static(s) => {
                if s.len() == 1 {
                    let c = s.chars().next().unwrap();
                    writeln!(out, "{indent}buf.push({c:?});").unwrap();
                } else {
                    writeln!(out, "{indent}buf.push_str({s:?});").unwrap();
                }
            }

            FlatPageToken::Field { field, .. } => {
                if let Some(v) = schema.find_varlena(field) {
                    if v.is_segment {
                        // Clôture du run courant — toujours valide même si
                        // ce token est le tout premier de la fonction
                        // (seg_start == 0 == buf.len() à cet instant, run
                        // vide légitimement poussé, aucun octet perdu).
                        writeln!(
                            out,
                            "{indent}segments.push(marius_projection::Segment::Buffered {{ start: seg_start, end: buf.len() }});"
                        )
                        .unwrap();
                        writeln!(
                            out,
                            "{indent}if let Some(s) = {field}_ref {{ segments.push(marius_projection::Segment::Borrowed(s)); }}"
                        )
                        .unwrap();
                        writeln!(out, "{indent}seg_start = buf.len();").unwrap();
                    } else {
                        match v.escape_policy {
                            EscapePolicy::Raw => {
                                writeln!(
                                    out,
                                    "{indent}if let Some(s) = {field}_ref {{ buf.push_str(s); }}"
                                )
                                .unwrap();
                            }
                            EscapePolicy::Escaped | EscapePolicy::PreEscaped => {
                                writeln!(
                                    out,
                                    "{indent}if let Some(s) = {field}_ref {{ marius_html_escape(s, buf); }}"
                                )
                                .unwrap();
                            }
                        }
                    }
                } else {
                    writeln!(
                        out,
                        r#"{indent}::std::fmt::Write::write_fmt(buf, format_args!("{{}}", record.{field})).ok();"#,
                    )
                    .unwrap();
                }
            }

            FlatPageToken::IfBool { field, .. } => {
                writeln!(out, "{indent}if record.{field} != 0 {{").unwrap();
                indent.push_str("    ");
            }

            FlatPageToken::EndIf => {
                let new_len = indent.len().saturating_sub(4);
                indent.truncate(new_len);
                writeln!(out, "{indent}}}").unwrap();
            }

            FlatPageToken::ScriptStart | FlatPageToken::ScriptEnd => {}

            FlatPageToken::StaticInclude {
                rel_from_manifest, ..
            } => {
                writeln!(
                    out,
                    "{indent}buf.push_str(include_str!({rel_from_manifest:?}));",
                )
                .unwrap();
            }

            FlatPageToken::AssetRef(key) => {
                let url = resolve_asset_url(key);
                writeln!(out, "{indent}buf.push_str({url:?});").unwrap();
            }

            // Insertion verbatim, même contrat que generate_aot_snippet —
            // `modules_snippet` n'est jamais imbriqué dans un run segmenté
            // (le marqueur vit dans <head>, hors de tout champ is_segment).
            FlatPageToken::ModulesPlaceholder => {
                out.push_str(modules_snippet);
            }
        }
    }

    // Clôture du dernier run — toujours émise, qu'il y ait eu 0 ou N champs
    // segmentés (si 0, ce run couvre tout buf, comportement équivalent à
    // l'implémentation par défaut de render_segments — mais cette fonction
    // n'est de toute façon appelée par build.rs que si has_segment == true).
    writeln!(
        out,
        "segments.push(marius_projection::Segment::Buffered {{ start: seg_start, end: buf.len() }});"
    )
    .unwrap();

    out
}

// =============================================================================
// Tests — Phase 2.2
// =============================================================================

#[cfg(test)]
mod tests_phase_2_2 {
    use super::{
        EscapePolicy, FieldKind, FieldSpec, FlatPageToken, SchemaIndex, VarlenField,
        generate_aot_snippet, generate_segmented_snippet,
    };

    fn make_schema<'a>(fixed: &'a [FieldSpec], varlena: &'a [VarlenField]) -> SchemaIndex<'a> {
        SchemaIndex { fixed, varlena }
    }

    /// Snippet avec champ fixed (write_fmt) et champ varlena (html_escape).
    /// IfBool émet != 0 (u8 dans StorageRow).
    /// Aucun buf.reserve dans le snippet — c'est la responsabilité de l'orchestrateur.
    #[test]
    fn test_generate_aot_snippet_typed() {
        let fixed = vec![
            FieldSpec {
                name: "title".to_string(),
                kind: FieldKind::I32,
                attnum: 1,
            },
            FieldSpec {
                name: "is_published".to_string(),
                kind: FieldKind::Bool,
                attnum: 2,
            },
        ];
        let varlena = vec![VarlenField {
            name: "body".to_string(),
            // Provenance non pertinente ici — generate_aot_snippet ne lit
            // jamais ref_schema/ref_table (seulement .name, via find_varlena).
            ref_schema: "test_schema".to_string(),
            ref_table: "test_table".to_string(),
            max_len: Some(1000),
            escape_policy: EscapePolicy::Escaped,
            is_segment: false,
            nullable: true,
            max_escaped_len_override: None,
        }];
        let schema = make_schema(&fixed, &varlena);

        let tokens: &[FlatPageToken<'_>] = &[
            FlatPageToken::Static("<article>"),
            FlatPageToken::Field {
                entity: "record",
                field: "title",
            },
            FlatPageToken::Field {
                entity: "varlena",
                field: "body",
            },
            FlatPageToken::IfBool {
                entity: "record",
                field: "is_published",
            },
            FlatPageToken::Static("<span>publié</span>"),
            FlatPageToken::EndIf,
            FlatPageToken::StaticInclude {
                original_path: "...",
                rel_from_manifest: "frag.html",
                len: 42,
            },
        ];

        let got = generate_aot_snippet(
            tokens,
            &schema,
            |_| unreachable!("aucun AssetRef dans ce test"),
            "",
        );

        // Varlena ref déclarée en tête, triée.
        assert!(
            got.contains("let body_ref: Option<&str> = varlena.body.as_deref();"),
            "déclaration varlena absente:\n{got}"
        );
        // Fixed → write_fmt.
        assert!(
            got.contains(
                r#"::std::fmt::Write::write_fmt(buf, format_args!("{}", record.title)).ok();"#
            ),
            "write_fmt absent:\n{got}"
        );
        // Varlena → html_escape.
        assert!(
            got.contains("if let Some(s) = body_ref { marius_html_escape(s, buf); }"),
            "html_escape absent:\n{got}"
        );
        // IfBool → != 0 (u8).
        assert!(
            got.contains("if record.is_published != 0 {"),
            "condition u8 absente:\n{got}"
        );
        // StaticInclude.
        assert!(
            got.contains(r#"buf.push_str(include_str!("frag.html"));"#),
            "include_str absent:\n{got}"
        );
        // Pas de buf.reserve dans le snippet.
        assert!(
            !got.contains("buf.reserve"),
            "buf.reserve ne doit pas être dans le snippet:\n{got}"
        );
    }

    /// CONTRAT-implementation-varlena-raw.md, Étape 4 : un champ
    /// EscapePolicy::Raw produit `buf.push_str(s)` direct, JAMAIS
    /// `marius_html_escape` — HTML déjà constitué, injecté tel quel.
    #[test]
    fn test_generate_aot_snippet_raw_field_bypasses_html_escape() {
        let fixed: Vec<FieldSpec> = vec![];
        let varlena = vec![VarlenField {
            name: "content".to_string(),
            ref_schema: "content".to_string(),
            ref_table: "body".to_string(),
            max_len: Some(32_000),
            escape_policy: EscapePolicy::Raw,
            is_segment: false,
            nullable: true,
            max_escaped_len_override: None,
        }];
        let schema = make_schema(&fixed, &varlena);

        let tokens: &[FlatPageToken<'_>] = &[FlatPageToken::Field {
            entity: "varlena",
            field: "content",
        }];

        let got = generate_aot_snippet(
            tokens,
            &schema,
            |_| unreachable!("aucun AssetRef dans ce test"),
            "",
        );

        assert!(
            got.contains("let content_ref: Option<&str> = varlena.content.as_deref();"),
            "déclaration varlena absente:\n{got}"
        );
        assert!(
            got.contains("if let Some(s) = content_ref { buf.push_str(s); }"),
            "buf.push_str direct absent (Raw ne doit jamais échapper):\n{got}"
        );
        assert!(
            !got.contains("marius_html_escape"),
            "marius_html_escape ne doit JAMAIS apparaître pour un champ Raw:\n{got}"
        );
    }

    /// Snippet sans varlena : aucune déclaration de ref.
    #[test]
    fn test_generate_aot_snippet_no_varlena() {
        let fixed = vec![FieldSpec {
            name: "id".to_string(),
            kind: FieldKind::I64,
            attnum: 1,
        }];
        let schema = make_schema(&fixed, &[]);
        let tokens: &[FlatPageToken<'_>] = &[
            FlatPageToken::Static("<p>"),
            FlatPageToken::Field {
                entity: "record",
                field: "id",
            },
            FlatPageToken::Static("</p>"),
        ];
        let got = generate_aot_snippet(
            tokens,
            &schema,
            |_| unreachable!("aucun AssetRef dans ce test"),
            "",
        );
        assert!(
            !got.contains("_ref"),
            "pas de déclaration ref sans varlena:\n{got}"
        );
        assert!(got.contains("record.id"), "champ id absent:\n{got}");
        assert!(
            !got.contains("buf.reserve"),
            "buf.reserve hors scope:\n{got}"
        );
    }

    // ── generate_segmented_snippet — CONTRAT-implementation-projection-segmentee.md, Étape 5 ──

    fn segment_field(name: &str) -> VarlenField {
        VarlenField {
            name: name.to_string(),
            ref_schema: "content".to_string(),
            ref_table: "body".to_string(),
            max_len: Some(32_000),
            escape_policy: EscapePolicy::Raw,
            is_segment: true,
            nullable: true,
            max_escaped_len_override: None,
        }
    }

    #[test]
    fn generate_segmented_snippet_splits_around_segment_field() {
        let fixed: Vec<FieldSpec> = vec![];
        let varlena = vec![segment_field("content")];
        let schema = make_schema(&fixed, &varlena);

        let tokens: &[FlatPageToken<'_>] = &[
            FlatPageToken::Static("<article>"),
            FlatPageToken::Field {
                entity: "varlena",
                field: "content",
            },
            FlatPageToken::Static("</article>"),
        ];

        let got = generate_segmented_snippet(
            tokens,
            &schema,
            |_| unreachable!("aucun AssetRef dans ce test"),
            "",
        );

        assert!(
            got.contains("let mut seg_start: usize = buf.len();"),
            "déclaration seg_start absente:\n{got}"
        );
        assert!(
            got.contains("segments.push(marius_projection::Segment::Buffered"),
            "push Buffered absent:\n{got}"
        );
        assert!(
            got.contains(
                "if let Some(s) = content_ref { segments.push(marius_projection::Segment::Borrowed(s)); }"
            ),
            "push Borrowed absent ou mal formé:\n{got}"
        );
        assert!(
            !got.contains("marius_html_escape"),
            "un champ segmenté ne doit jamais passer par marius_html_escape:\n{got}"
        );
        // Deux runs Buffered : avant et après le champ segmenté.
        assert_eq!(
            got.matches("Segment::Buffered").count(),
            2,
            "deux runs Buffered attendus (avant/après le champ segmenté):\n{got}"
        );
    }

    #[test]
    fn generate_segmented_snippet_handles_segment_inside_if_block() {
        let fixed: Vec<FieldSpec> = vec![];
        let varlena = vec![segment_field("content")];
        let schema = make_schema(&fixed, &varlena);

        let tokens: &[FlatPageToken<'_>] = &[
            FlatPageToken::Static("<p>"),
            FlatPageToken::IfBool {
                entity: "record",
                field: "is_readable",
            },
            FlatPageToken::Field {
                entity: "varlena",
                field: "content",
            },
            FlatPageToken::EndIf,
            FlatPageToken::Static("</p>"),
        ];

        let got = generate_segmented_snippet(
            tokens,
            &schema,
            |_| unreachable!("aucun AssetRef dans ce test"),
            "",
        );

        // Le push Buffered/Borrowed à l'intérieur du if doit être indenté —
        // preuve qu'il est bien conditionnel, pas exécuté inconditionnellement.
        assert!(
            got.contains("    segments.push(marius_projection::Segment::Buffered"),
            "le push à l'intérieur du bloc if devrait être indenté :\n{got}"
        );
        assert!(
            got.contains("if record.is_readable != 0 {"),
            "bloc if absent:\n{got}"
        );
        // Le dernier push (clôture finale) est à l'indentation racine (pas de
        // préfixe 4-espaces), après la fermeture du bloc if.
        let last_push_line = got
            .lines()
            .filter(|l| l.trim_start().starts_with("segments.push"))
            .next_back()
            .expect("au moins un push attendu");
        assert!(
            !last_push_line.starts_with(' '),
            "le push final doit être à la racine, pas à l'intérieur du if:\n{got}"
        );
    }

    #[test]
    fn generate_segmented_snippet_final_close_always_emitted() {
        // Aucun champ segmenté référencé dans les tokens (cas dégénéré,
        // jamais déclenché en pratique par build.rs — has_segment serait
        // false — mais la fonction ne doit pas paniquer ni produire un état
        // incohérent si elle est appelée quand même).
        let fixed: Vec<FieldSpec> = vec![];
        let varlena: Vec<VarlenField> = vec![];
        let schema = make_schema(&fixed, &varlena);

        let tokens: &[FlatPageToken<'_>] = &[FlatPageToken::Static("<p>Rien à segmenter</p>")];

        let got = generate_segmented_snippet(
            tokens,
            &schema,
            |_| unreachable!("aucun AssetRef dans ce test"),
            "",
        );

        assert_eq!(
            got.matches("Segment::Buffered").count(),
            1,
            "un seul run Buffered attendu, couvrant tout buf:\n{got}"
        );
    }
}
