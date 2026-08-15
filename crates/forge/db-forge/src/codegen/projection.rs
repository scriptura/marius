// crates/forge/db-forge/src/codegen/projection.rs
//
//! # marius-db-forge - projection
//! Génération AOT du stub `impl Projection` pour une table SQL.

use std::fmt::Write as _;

use crate::mapping::{Column, PrimaryKey, map_type};
use crate::naming::{to_pascal, to_screaming};
use marius_fragment_forge::{FieldSpec, TemplateMetrics, VarlenField};

/// Construit l'expression SELECT pour une colonne fixed-length.
///
/// Nom brut (qualifié ou non selon `qualifier`) dans l'immense majorité des
/// cas. Cast explicite + alias `AS <nom>` quand `TypeMapping::select_cast`
/// est renseigné — pg_lsn à ce jour (sqlx sans Decode natif pour ce type,
/// vérifié contre docs.rs/sqlx : aucun autre type de mapping.rs n'a besoin
/// de ce mécanisme). L'alias est obligatoire dès qu'un cast est appliqué :
/// sans lui, `sqlx::FromRow` (dérivé, appariement par nom de colonne) ne
/// retrouve plus le champ — Postgres nomme une expression castée d'après
/// son opérateur final, jamais d'après la colonne source.
///
/// `qualifier` : préfixe `schema.table` appliqué à la référence de colonne
/// (cast ou nom brut) — `None` en l'absence de JOIN (source unique, aucune
/// ambiguïté possible, jamais besoin de qualifier).
fn select_expr_for(col: &Column, qualifier: Option<&str>) -> String {
    let qualified = match qualifier {
        Some(q) => format!("{q}.{}", col.name),
        None => col.name.clone(),
    };
    match map_type(&col.sql_type).select_cast {
        Some(cast_tpl) => format!("{} AS {}", cast_tpl.replace("{}", &qualified), col.name),
        None => qualified,
    }
}

/// Génère le stub `impl Projection` complet pour une table.
///
/// Émet dans l'ordre :
///   1. `pub struct {Name}Projection;`
///   2. Constantes de capacité (`{NAME}_STATIC_CAP`, `_DYNAMIC_CAP`, `_TOTAL_CAP`)
///   3. `impl crate::projection::Projection for {Name}Projection { … }`
///      - type Record, type VarlenOwned
///      - fetch_batch() avec SELECT + FROM + WHERE construits depuis le schéma
///      - render() avec corps généré par Fragment-Forge
///      - artifact_path()
///
/// `varlena_join` : &[(schema, table, fk_col)] — un triplet par slot
/// (join_slot_idx croissant, cf. registry.rs). Tranche vide = aucun JOIN.
/// CONTRAT-implementation-multi-slot-varlena.md, Étape 4 : remplace
/// l'ancien Option<(schema, table, fk_col)>, limité à un seul JOIN par
/// composant (limite Phase 1, jamais comblée avant cette révision).
///
/// `render` : Option<(render_body, metrics)> — résultat du pipeline Voie B
/// (scan → parse_tokens → validate_ast → resolve_and_measure → generate_aot_snippet),
/// orchestré par build.rs (lecture disque du template `.marius`).
///   `Some((body, metrics))` : template trouvé et résolu — émet les vraies
///     constantes de capacité et le corps réel de render().
///   `None` : aucun template pour cette table — émet un stub vide avec
///     capacités à zéro (comportement de transition, render() ne fait rien).
#[allow(clippy::too_many_arguments)]
pub fn write_projection_stub(
    out: &mut String,
    schema: &str,
    table: &str,
    columns: &[Column],
    pk: &PrimaryKey,
    varlena: &[VarlenField],
    varlena_join: &[(&str, &str, &str)],
    render: Option<(&str, &TemplateMetrics)>,
) {
    let name = to_pascal(&format!("{schema}_{table}"));
    let proj_name = format!("{name}Projection");
    let screaming = to_screaming(&format!("{schema}_{table}"));

    let varlen_owned_type = if varlena.is_empty() {
        "()".to_string()
    } else {
        format!("{name}VarlenOwned")
    };

    // ── Colonnes fixed-length pour le SELECT ─────────────────────────────────
    let fixed_cols: Vec<&str> = columns
        .iter()
        .filter(|c| map_type(&c.sql_type).is_fixed)
        .map(|c| c.name.as_str())
        .collect();

    if fixed_cols.is_empty() {
        eprintln!(
            "cargo:warning=DB-Forge [{schema}.{table}] : \
             aucune colonne fixed-length — stub incomplet généré."
        );
    }

    // pk_col_name : calculé ici (avant le FROM/JOIN, qui en a besoin — Constat
    // n°2) plutôt qu'après comme dans la version précédente du générateur.
    // Échec de build explicite (Article Zéro §0.1bis) si une jointure varlena
    // est combinée à une PK composite : meta.component_varlena_join ne stocke
    // qu'un seul nom de colonne (fk_col) — la seule sémantique généralisable
    // est « FK enfant → PK parent », qui exige une PK à colonne unique côté
    // table composant. Générer un JOIN sur un stub de PK composite produirait
    // un SQL invalide, silencieusement, à l'exécution plutôt qu'à la
    // compilation — inacceptable pour un compilateur AOT.
    if !varlena_join.is_empty() && matches!(pk, PrimaryKey::Composite) {
        panic!(
            "DB-Forge [{schema}.{table}]: jointure varlena (meta.component_varlena_join) \
             combinée à une clé primaire composite n'est pas supportée par le générateur \
             actuel — le JOIN suppose une clé primaire à colonne unique côté table \
             composant. Ouvrir un Contrat d'Implémentation dédié avant d'introduire ce cas."
        );
    }
    let pk_col_name: &str = match pk {
        PrimaryKey::Single(col) => col.as_str(),
        PrimaryKey::Composite => fixed_cols.first().copied().unwrap_or("id"),
    };

    // ── Construction SELECT + FROM (Voie d'Extraction — fetch_from_pg) ────────
    // Ces variables alimentent le corps SQLx de fetch_from_pg ci-dessous.
    // Non utilisées par fetch_batch (Voie d'Exécution mmap).
    let (select, from_clause) = if !varlena_join.is_empty() {
        // Qualification de chaque champ varlena par SA table de provenance
        // (VarlenField::ref_table, Étape 2 du Contrat multi-slot) — corrige le
        // bug Phase 1 où un unique `vt` capturé hors boucle était appliqué à
        // tort à tous les champs, quel que soit leur slot d'origine réel.
        let varlena_cols: Vec<String> = varlena
            .iter()
            .map(|v| format!("{}.{}", v.ref_table, v.name))
            .collect();
        // select_expr_for (pas un simple `format!("{schema}.{table}.{c}")`
        // comme avant Phase 2 walsn) : porte le cast pg_lsn + alias quand
        // nécessaire, nom qualifié brut sinon — colonne par colonne, dérivé
        // indépendamment de `fixed_cols` (qui reste des noms bruts, utilisés
        // ailleurs pour pk_col_name/ON/WHERE, jamais pour le SELECT).
        let qualifier = format!("{schema}.{table}");
        let all_cols: Vec<String> = columns
            .iter()
            .filter(|c| map_type(&c.sql_type).is_fixed)
            .map(|c| select_expr_for(c, Some(&qualifier)))
            .chain(varlena_cols)
            .collect();
        // Une clause LEFT JOIN par slot (join_slot_idx croissant, ordre déjà
        // garanti par registry.rs), enchaînées sur la même table pivot.
        // Sémantique par jointure inchangée depuis le correctif Phase 1
        // (Constat n°2) : fk_col de l'enfant référence la PK du parent —
        // {schema}.{table}.{pk_col_name} = {vs}.{vt}.{fk}, jamais l'inverse.
        let joins: Vec<String> = varlena_join
            .iter()
            .map(|(vs, vt, fk)| {
                format!("LEFT JOIN {vs}.{vt} ON {schema}.{table}.{pk_col_name} = {vs}.{vt}.{fk}")
            })
            .collect();
        let from = format!("{schema}.{table} {}", joins.join(" "));
        (all_cols.join(", "), from)
    } else {
        // Même logique que la branche JOIN ci-dessus, sans qualification
        // (source unique) — select_expr_for(c, None).
        let select_list: Vec<String> = columns
            .iter()
            .filter(|c| map_type(&c.sql_type).is_fixed)
            .map(|c| select_expr_for(c, None))
            .collect();
        (select_list.join(", "), format!("{schema}.{table}"))
    };

    let where_clause = match pk {
        PrimaryKey::Single(col) => {
            format!("WHERE {schema}.{table}.{col} = ANY($1) ORDER BY {schema}.{table}.{col} ASC")
        }
        PrimaryKey::Composite => "WHERE 1=1 /* PK composite: adapter */".to_string(),
    };

    // ── Fragment-Forge : corps render() + constantes capacité ────────────────
    // ── Construction des FieldSpecs ───────────────────────────────────────────
    // Helper partagé (crate::build_field_specs) — même logique que build.rs
    // utilise pour construire le SchemaIndex passé à resolve_and_measure.
    let field_specs: Vec<FieldSpec> = crate::build_field_specs(columns);

    // pk_field : résolution pour record_id() (invariant : PK Single dans field_specs).
    // pk_col_name déjà calculé ci-dessus.
    let pk_field: &FieldSpec = field_specs
        .iter()
        .find(|f| f.name == pk_col_name)
        .unwrap_or_else(|| {
            panic!(
                "DB-Forge [{schema}.{table}]: champ PK '{pk_col_name}' \
                 absent des FieldSpecs — type PK non supporté par FieldKind."
            )
        });

    // ── Voie B : constantes de capacité + corps de render() ───────────────────
    // `render` vient de build.rs (pipeline complet exécuté sur le template
    // .marius, si trouvé). Pas de stub Voie A : soit le template est résolu,
    // soit la table reste à zéro-capacité (render() vide) en attendant un template.
    let (cap_consts, render_body) = match render {
        Some((body, metrics)) => {
            let total = metrics.total_static_bytes + metrics.total_dynamic_bytes;
            let consts = format!(
                "pub const {screaming}_STATIC_CAP:  usize = {};\n\
                 pub const {screaming}_DYNAMIC_CAP: usize = {};\n\
                 pub const {screaming}_TOTAL_CAP:   usize = {};\n",
                metrics.total_static_bytes, metrics.total_dynamic_bytes, total,
            );
            (consts, body.to_string())
        }
        None => {
            // Pas de warning ici : build.rs (responsable de l'I/O disque) a déjà
            // signalé l'absence de template via cargo:warning au moment de la
            // lecture. write_projection_stub reste un générateur pur, sans avis
            // sur la raison du None.
            let consts = format!(
                "pub const {screaming}_STATIC_CAP:  usize = 0;\n\
                 pub const {screaming}_DYNAMIC_CAP: usize = 0;\n\
                 pub const {screaming}_TOTAL_CAP:   usize = 0;\n"
            );
            (consts, String::new())
        }
    };

    // ── Émission ──────────────────────────────────────────────────────────────
    writeln!(out, "pub struct {proj_name};").unwrap();
    writeln!(out).unwrap();

    // Constantes au niveau module (pas dans le bloc impl).
    writeln!(out, "{cap_consts}").unwrap();

    // ── StoreRegistry statique — remplace l'ancien OnceLock<PackfileReader> ──
    // Déclaration au niveau module : durée de vie 'static garantie.
    // Contrairement à OnceLock, remplaçable après le premier montage — c'est
    // précisément ce que le pipeline réactif (ingest_and_swap) exige.
    // Cf. DESIGN-store-registry.md — StoreRegistry<P> est mono-slot par
    // Projection (pas de HashMap/clé), cohérent avec le fait que fetch_batch
    // est monomorphisé sur P à la compilation.
    writeln!(
        out,
        "static {screaming}_STORE: marius_projection::StoreRegistry<{proj_name}> = \
         marius_projection::StoreRegistry::new();"
    )
    .unwrap();
    writeln!(out).unwrap();

    writeln!(
        out,
        "// Phase 2 AOT : _pool ignoré — lecture via StoreRegistry<PackfileReader>."
    )
    .unwrap();
    writeln!(out, "// RLS         : voir 09_rls/01_policies.sql").unwrap();
    writeln!(out, "impl crate::projection::Projection for {proj_name} {{").unwrap();
    writeln!(out, "    type Record = {name}StorageRow;").unwrap();
    writeln!(out, "    type VarlenOwned = {varlen_owned_type};").unwrap();
    writeln!(out).unwrap();

    // ── fetch_batch (Phase 2 AOT) ─────────────────────────────────────────────
    // _pool : paramètre de trait conservé pour compatibilité de signature.
    //         Jamais utilisé en production — le pool SQLx est absent du hot path.
    //         Fail-fast : si store.bin est absent au premier appel, panic immédiat.
    //         Pas de fallback réseau — l'absence de store est une erreur fatale AOT.
    writeln!(out, "    async fn fetch_batch(").unwrap();
    writeln!(out, "        _pool: &sqlx::PgPool,").unwrap();
    writeln!(out, "        ids:   &[i64],").unwrap();
    writeln!(
        out,
        "    ) -> Result<Vec<(Self::Record, Self::VarlenOwned)>, sqlx::Error> {{"
    )
    .unwrap();

    if fixed_cols.is_empty() {
        writeln!(
            out,
            "        todo!(\"DB-Forge: aucune colonne fixed-length pour {schema}.{table}\")"
        )
        .unwrap();
    } else {
        // Un seul load() par appel — jamais dans la boucle sur `ids` (INV-5,
        // DESIGN-store-registry.md §7) : tout le batch est résolu contre une
        // unique version de store.bin, jamais deux générations mélangées.
        writeln!(out, "        let reader = {screaming}_STORE.load();").unwrap();
        writeln!(out).unwrap();

        // Itération sur les ids demandés — lookup O(log N) par binary search.
        // Les ids absents du store sont silencieusement ignorés (enreg. supprimé).
        writeln!(
            out,
            "        let mut batch = Vec::with_capacity(ids.len());"
        )
        .unwrap();
        writeln!(out, "        for &id in ids {{").unwrap();
        writeln!(
            out,
            "            if let Some((record, _vrefs)) = reader.lookup(id) {{"
        )
        .unwrap();

        if varlena.is_empty() {
            // Table sans varlena : copie du Record, VarlenOwned = ().
            writeln!(out, "                batch.push((*record, ()));").unwrap();
        } else {
            // Construction VarlenOwned depuis VarlenRefs (vues mmap → String owned).
            // to_owned() : unique allocation tolérée — bornée, isolée avant render().
            writeln!(out, "                let owned = {name}VarlenOwned {{").unwrap();
            for (i, v) in varlena.iter().enumerate() {
                writeln!(
                    out,
                    "                    {}: _vrefs.get({i}).map(str::to_owned),",
                    v.name
                )
                .unwrap();
            }
            writeln!(out, "                }};").unwrap();
            writeln!(out, "                batch.push((*record, owned));").unwrap();
        }

        writeln!(out, "            }}").unwrap();
        writeln!(out, "        }}").unwrap();
        writeln!(out, "        Ok(batch)").unwrap();
    }

    writeln!(out, "    }}").unwrap();
    writeln!(out).unwrap();

    // ── fetch_from_pg (Voie d'Extraction — cold path marius-dump) ─────────────
    // Corps SQLx identique à l'ancien fetch_batch Phase 1.
    // pool : utilisé (requête réseau réelle vers PostgreSQL).
    // Appelée uniquement par dumper::dump_table — jamais par le Dispatcher.
    writeln!(out, "    async fn fetch_from_pg(").unwrap();
    writeln!(out, "        pool: &sqlx::PgPool,").unwrap();
    writeln!(out, "        ids:  &[i64],").unwrap();
    writeln!(
        out,
        "    ) -> Result<Vec<(Self::Record, Self::VarlenOwned)>, sqlx::Error> {{"
    )
    .unwrap();

    if fixed_cols.is_empty() {
        writeln!(
            out,
            "        todo!(\"DB-Forge: aucune colonne fixed-length pour {schema}.{table}\")"
        )
        .unwrap();
    } else {
        writeln!(out, "        let rows = sqlx::query_as::<_, {name}Row>(").unwrap();
        writeln!(
            out,
            "            \"SELECT {select} FROM {from_clause} {where_clause}\","
        )
        .unwrap();
        writeln!(out, "        )").unwrap();
        writeln!(out, "        .bind(ids)").unwrap();
        writeln!(out, "        .fetch_all(pool)").unwrap();
        writeln!(out, "        .await?;").unwrap();

        if varlena.is_empty() {
            // Pas de varlena : From<Row> pour la conversion.
            writeln!(
                out,
                "        Ok(rows.into_iter().map(|r| ({name}StorageRow::from(r), ())).collect())"
            )
            .unwrap();
        } else {
            // Avec varlena : déstructuration complète (évite E0382 partial move).
            writeln!(out, "        Ok(rows.into_iter().map(|r| {{").unwrap();

            writeln!(out, "            let {name}Row {{").unwrap();
            for col in columns {
                let m = map_type(&col.sql_type);
                if m.is_fixed {
                    writeln!(out, "                {},", col.name).unwrap();
                }
            }
            for v in varlena {
                writeln!(out, "                {},", v.name).unwrap();
            }
            writeln!(out, "                ..").unwrap();
            writeln!(out, "            }} = r;").unwrap();

            // VarlenOwned depuis les bindings varlena.
            writeln!(out, "            let owned = {name}VarlenOwned {{").unwrap();
            for v in varlena {
                writeln!(out, "                {},", v.name).unwrap();
            }
            writeln!(out, "            }};").unwrap();

            // StorageRow depuis les bindings fixed — sentinel Phase 3 intégré.
            writeln!(out, "            let storage = {name}StorageRow {{").unwrap();

            let mut layout_bytes = 0usize;
            let mut max_align = 1usize;

            for col in columns {
                let m = map_type(&col.sql_type);
                if !m.is_fixed {
                    continue;
                }

                layout_bytes += m.size_bytes;
                max_align = max_align.max(m.alignment);

                let sentinel = col.sentinel.as_deref().unwrap_or(m.default_sentinel);

                let mut expr = if col.is_notnull {
                    // pg_lsn AVANT le match sur m.row_type — même correctif
                    // que from_impl.rs (write_from_impl) : depuis Phase 2
                    // walsn, row_type de pg_lsn vaut "i64", indiscernable de
                    // bigint/int8 sur ce seul critère. col.sql_type reste la
                    // seule clé fiable. Ce site est un DEUXIÈME générateur,
                    // indépendant de write_from_impl (déstructuration/
                    // reconstruction inline pour fetch_from_pg, jamais un
                    // appel à From::from()) — découvert après coup, non
                    // routé par le correctif de from_impl.rs.
                    if col.sql_type == "pg_lsn" {
                        format!("{} as u64", col.name)
                    } else {
                        match m.row_type {
                            "chrono::DateTime<chrono::Utc>" => {
                                format!("{}.timestamp_micros()", col.name)
                            }
                            "chrono::NaiveDateTime" => {
                                format!("{}.and_utc().timestamp_micros()", col.name)
                            }
                            "chrono::NaiveDate" => format!("{}.num_days_from_ce()", col.name),
                            _ => col.name.clone(),
                        }
                    }
                } else {
                    m.from_expr
                        .replace("{field}", &col.name)
                        .replace("{sentinel}", sentinel)
                };

                if m.row_type == "bool" {
                    expr = format!("({expr}) as u8");
                }

                if col.name == expr {
                    writeln!(out, "                {},", col.name).unwrap();
                } else {
                    writeln!(out, "                {}: {},", col.name, expr).unwrap();
                }
            }

            let padded_size = layout_bytes.div_ceil(max_align.max(1)) * max_align.max(1);
            let tail_pad = padded_size - layout_bytes;
            if tail_pad > 0 {
                writeln!(out, "                _pad: [0u8; {tail_pad}],").unwrap();
            }

            writeln!(out, "            }};").unwrap();
            writeln!(out, "            (storage, owned)").unwrap();
            writeln!(out, "        }}).collect())").unwrap();
        }
    }

    writeln!(out, "    }}").unwrap();
    writeln!(out).unwrap();

    // ── render() / render_segments() ──────────────────────────────────────────
    // CONTRAT-implementation-projection-segmentee.md, Étape 5 : has_segment
    // recalculé ici à partir du même `varlena` que build.rs a déjà utilisé
    // pour choisir generate_aot_snippet vs generate_segmented_snippet — aucune
    // valeur à faire transiter séparément, la même information est disponible
    // aux deux endroits.
    let has_segment = varlena.iter().any(|v| v.is_segment);

    // varlena param de render() : "_varlena" tant qu'aucun corps réel ne le
    // consomme (stub Voie B), que la table n'a pas de varlena, OU que le
    // composant est segmenté (render() devient alors un stub jamais appelé —
    // cf. plus bas — le paramètre n'est donc jamais lu dans ce cas non plus).
    let body_is_real = render.is_some();
    let varlena_param = if varlena.is_empty() {
        "_varlena: &()".to_string()
    } else if has_segment {
        format!("_varlena: &{name}VarlenOwned")
    } else if body_is_real {
        format!("varlena: &{name}VarlenOwned")
    } else {
        format!("_varlena: &{name}VarlenOwned")
    };
    writeln!(
        out,
        "    // Nesting inévitable, cosmétique : composition indépendante d'un"
    )
    .unwrap();
    writeln!(
        out,
        "    // bloc {{% if %}} du template et du if let Some(s) systématique émis"
    )
    .unwrap();
    writeln!(
        out,
        "    // pour tout champ varlena — fusionner algorithmiquement les deux"
    )
    .unwrap();
    writeln!(
        out,
        "    // ajouterait de la complexité réelle à l'émetteur pour un gain"
    )
    .unwrap();
    writeln!(out, "    // purement esthétique (session du 23/07/2026).").unwrap();
    writeln!(out, "    #[allow(clippy::collapsible_if)]").unwrap();
    writeln!(
        out,
        "    fn render(record: &Self::Record, {varlena_param}, buf: &mut String) {{"
    )
    .unwrap();
    if has_segment {
        // Composant segmenté : render() n'est jamais invoquée en pratique —
        // BatchRenderer::render_batch appelle systématiquement
        // render_segments() (Étape 4). Présente uniquement parce que le
        // trait l'exige (pas de valeur par défaut pour render() lui-même,
        // contrairement à render_segments()) — cf. StubSegmentedProjection,
        // même patron, déjà exercé par les tests de batch_renderer.rs.
        writeln!(out, "        let _ = (record, buf);").unwrap();
        writeln!(
            out,
            "        unreachable!(\"{name}Projection::render() ne devrait jamais être appelée — composant segmenté, BatchRenderer appelle toujours render_segments().\");"
        )
        .unwrap();
    } else if render_body.is_empty() {
        // Aucun template .marius trouvé pour cette table — stub neutre.
        writeln!(out, "        let _ = (record, buf);").unwrap();
    } else {
        // Template résolu par build.rs (Voie B complète) : buf.reserve()
        // référence la constante de capacité totale émise plus haut, puis
        // le corps généré par generate_aot_snippet (qui n'émet pas reserve
        // lui-même — c'est la responsabilité de l'appelant, ici ce bloc).
        writeln!(out, "        buf.reserve({screaming}_TOTAL_CAP);").unwrap();
        for line in render_body.lines() {
            writeln!(out, "    {line}").unwrap();
        }
    }
    writeln!(out, "    }}").unwrap();
    writeln!(out).unwrap();

    if has_segment {
        // MAX_SEGMENTS = 2N+1, N = nombre de champs is_segment dans ce join
        // (chaque champ segmenté ferme un run Buffered, pousse un Borrowed,
        // rouvre un run — cf. generate_segmented_snippet). Hypothèse
        // simplificatrice documentée : chaque champ segmenté n'apparaît
        // qu'une fois dans le template — vraie pour tous les cas réels à ce
        // jour (content.core : 1 champ segmenté → MAX_SEGMENTS = 3). Un
        // champ référencé plusieurs fois sur-approvisionnerait le Vec sans
        // jamais casser la correction — coût négligeable, jamais un bug.
        let segment_count = varlena.iter().filter(|v| v.is_segment).count();
        let max_segments = 2 * segment_count + 1;
        writeln!(out, "    const MAX_SEGMENTS: usize = {max_segments};").unwrap();
        writeln!(out).unwrap();

        writeln!(out, "    #[allow(clippy::collapsible_if)]").unwrap();
        writeln!(
            out,
            "    fn render_segments<'seg>(record: &Self::Record, varlena: &'seg {name}VarlenOwned, buf: &mut String, segments: &mut Vec<marius_projection::Segment<'seg>>) {{"
        )
        .unwrap();
        // render_body est ici le corps produit par generate_segmented_snippet
        // (build.rs a déjà choisi le bon générateur selon has_segment, avant
        // même d'appeler write_projection_stub) — jamais celui de
        // generate_aot_snippet pour un composant segmenté.
        writeln!(out, "        buf.reserve({screaming}_TOTAL_CAP);").unwrap();
        for line in render_body.lines() {
            writeln!(out, "    {line}").unwrap();
        }
        writeln!(out, "    }}").unwrap();
        writeln!(out).unwrap();
    }

    // ── record_id() ───────────────────────────────────────────────────────────
    // Accesseur direct du champ PK — coût zéro, inliné par LLVM.
    // Requis par BatchRenderer::render_batch pour construire PackfileEntry.id
    // sans connaissance du nom du champ PK au niveau du trait générique.
    writeln!(out, "    #[inline(always)]").unwrap();
    writeln!(out, "    fn record_id(record: &Self::Record) -> i64 {{").unwrap();
    // On retrouve la colonne d'origine dans le slice pour lire son sql_type
    let pk_column = columns
        .iter()
        .find(|c| c.name == pk_field.name)
        .expect("DB-Forge : colonne PK absente du slice columns — incohérence fetch_pk_column/fetch_columns");
    let pk_mapping = crate::mapping::map_type(&pk_column.sql_type);

    // Résolution du cast selon la taille native (zéro overhead si déjà i64)
    let pk_expr = if pk_mapping.store_type == "i64" {
        format!("record.{}", pk_field.name)
    } else {
        format!("record.{} as i64", pk_field.name)
    };
    writeln!(out, "        {}", pk_expr).unwrap();
    writeln!(out, "    }}").unwrap();
    writeln!(out).unwrap();

    // ── packfile_path() (HTML Fragments) ──────────────────────────────────────
    // Chemin statique par table — aucun paramètre record.
    // Un seul open() par batch (INV O(1) syscalls).
    writeln!(out, "    fn packfile_path() -> ::std::path::PathBuf {{").unwrap();
    writeln!(
        out,
        "        let root = std::env::var(\"MARIUS_ARTIFACTS_DIR\")"
    )
    .unwrap();
    writeln!(
        out,
        "            .unwrap_or_else(|_| \"artifacts\".to_string());"
    )
    .unwrap();
    writeln!(
        out,
        "        ::std::path::PathBuf::from(format!(\"{{root}}/{schema}_{table}_pack.bin\"))"
    )
    .unwrap();
    writeln!(out, "    }}").unwrap();
    writeln!(out).unwrap();

    // ── store_path() (Phase 1.4 : binary dump) ────────────────────────────────
    writeln!(out, "    fn store_path() -> ::std::path::PathBuf {{").unwrap();
    writeln!(
        out,
        "        let root = std::env::var(\"MARIUS_ARTIFACTS_DIR\")"
    )
    .unwrap();
    writeln!(
        out,
        "            .unwrap_or_else(|_| \"artifacts\".to_string());"
    )
    .unwrap();
    writeln!(
        out,
        "        ::std::path::PathBuf::from(format!(\"{{root}}/{schema}_{table}_store.bin\"))"
    )
    .unwrap();
    writeln!(out, "    }}").unwrap();
    writeln!(out).unwrap();

    // ── store_registry() — point d'entrée générique vers {SCREAMING}_STORE ──
    // Requis par le trait (pas seulement cold_start_store, inhérente) pour
    // que du code générique <P: Projection> (ingest_and_swap) puisse
    // atteindre la static propre à cette Projection sans la nommer.
    writeln!(
        out,
        "    fn store_registry() -> &'static marius_projection::StoreRegistry<Self> {{"
    )
    .unwrap();
    writeln!(out, "        &{screaming}_STORE").unwrap();
    writeln!(out, "    }}").unwrap();
    writeln!(out).unwrap();

    // ── varlena_field_count() + encode_varlena() ─────────────────────────────
    // Émis uniquement si la table a des colonnes varlena.
    // Sans ces overrides, le trait applique les defaults (0 / no-op) →
    // PackfileBuilder n'écrit ni TOC ni Heap → store tronqué.
    //
    // Sentinel : offset=u32::MAX, len=0 pour None OU Some("").
    // Some("") est sémantiquement absent : écrire un slot len=0 dans le heap
    // consomme une entrée TOC pour zéro octet utile et bloque le chemin sentinel
    // côté reader (qui teste offset == u32::MAX, pas len == 0).
    if !varlena.is_empty() {
        let vf = varlena.len();

        writeln!(out, "    #[inline(always)]").unwrap();
        writeln!(out, "    fn varlena_field_count() -> u16 {{ {vf} }}").unwrap();
        writeln!(out).unwrap();

        writeln!(out, "    fn encode_varlena(").unwrap();
        writeln!(out, "        owned: &Self::VarlenOwned,").unwrap();
        writeln!(out, "        heap:  &mut Vec<u8>,").unwrap();
        writeln!(
            out,
            "        toc:   &mut Vec<marius_projection::VarlenSlot>,"
        )
        .unwrap();
        writeln!(out, "    ) {{").unwrap();
        for v in varlena {
            writeln!(out, "        match owned.{}.as_deref() {{", v.name).unwrap();
            writeln!(out, "            Some(s) if !s.is_empty() => {{").unwrap();
            writeln!(out, "                let offset = heap.len() as u32;").unwrap();
            writeln!(out, "                let len    = s.len() as u32;").unwrap();
            writeln!(out, "                heap.extend_from_slice(s.as_bytes());").unwrap();
            writeln!(
                out,
                "                toc.push(marius_projection::VarlenSlot {{ offset, len }});"
            )
            .unwrap();
            writeln!(out, "            }}").unwrap();
            writeln!(out, "            _ => toc.push(marius_projection::VarlenSlot {{ offset: u32::MAX, len: 0 }}),").unwrap();
            writeln!(out, "        }}").unwrap();
        }
        writeln!(out, "    }}").unwrap();
    }

    writeln!(out, "}}\n").unwrap();

    // ── cold_start_store() — provisionnement à froid du StoreRegistry ────────
    // Fonction inhérente (hors trait Projection) : appelée une fois au
    // bootstrap (main.rs), avant tout Dispatcher/serveur Axum. Fail-fast si
    // store.bin est absent/invalide — cf. DESIGN-store-registry.md §5.
    writeln!(out, "impl {proj_name} {{").unwrap();
    writeln!(
        out,
        "    pub fn cold_start_store() -> ::std::io::Result<()> {{"
    )
    .unwrap();
    writeln!(
        out,
        "        {screaming}_STORE.cold_start(&{proj_name}::store_path())"
    )
    .unwrap();
    writeln!(out, "    }}").unwrap();
    writeln!(out, "}}\n").unwrap();
}
