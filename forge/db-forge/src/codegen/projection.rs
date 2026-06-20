// =============================================================================
// marius-db-forge · codegen/projection.rs
// Génération du stub impl Projection pour une table.
// =============================================================================

use std::fmt::Write as _;

use crate::mapping::{Column, PrimaryKey, map_type};
use crate::naming::{to_pascal, to_screaming};
use marius_fragment_forge::{FieldSpec, VarlenField, TemplateMetrics};

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
/// `varlena_join` : Option<(schema, table, fk_col)> — None si pas de JOIN.
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
    out:          &mut String,
    schema:       &str,
    table:        &str,
    columns:      &[Column],
    pk:           &PrimaryKey,
    varlena:      &[VarlenField],
    varlena_join: Option<(&str, &str, &str)>,
    render:       Option<(&str, &TemplateMetrics)>,
) {
    let name      = to_pascal(&format!("{schema}_{table}"));
    let proj_name = format!("{name}Projection");
    let screaming = to_screaming(&format!("{schema}_{table}"));

    let varlen_owned_type = if varlena.is_empty() {
        "()".to_string()
    } else {
        format!("{name}VarlenOwned")
    };

    // ── Colonnes fixed-length pour le SELECT ─────────────────────────────────
    let fixed_cols: Vec<&str> = columns.iter()
        .filter(|c| map_type(&c.sql_type).is_fixed)
        .map(|c| c.name.as_str())
        .collect();

    if fixed_cols.is_empty() {
        eprintln!(
            "cargo:warning=DB-Forge [{schema}.{table}] : \
             aucune colonne fixed-length — stub incomplet généré."
        );
    }

    // ── Construction SELECT + FROM (Voie d'Extraction — fetch_from_pg) ────────
    // Ces variables alimentent le corps SQLx de fetch_from_pg ci-dessous.
    // Non utilisées par fetch_batch (Voie d'Exécution mmap).
    let (select, from_clause) = if let Some((vs, vt, _fk)) = varlena_join {
        let varlena_cols: Vec<String> = varlena.iter()
            .map(|v| format!("{vt}.{}", v.name))
            .collect();
        let all_cols: Vec<String> = fixed_cols.iter()
            .map(|c| format!("{schema}.{table}.{c}"))
            .chain(varlena_cols)
            .collect();
        let from = format!(
            "{schema}.{table} LEFT JOIN {vs}.{vt} ON {schema}.{table}.{_fk} = {vs}.{vt}.{_fk}"
        );
        (all_cols.join(", "), from)
    } else {
        (fixed_cols.join(", "), format!("{schema}.{table}"))
    };

    let where_clause = match pk {
        PrimaryKey::Single(col) => format!(
            "WHERE {schema}.{table}.{col} = ANY($1) ORDER BY {schema}.{table}.{col} ASC"
        ),
        PrimaryKey::Composite => "WHERE 1=1 /* PK composite: adapter */".to_string(),
    };

    // ── Fragment-Forge : corps render() + constantes capacité ────────────────
    // ── Construction des FieldSpecs ───────────────────────────────────────────
    // Helper partagé (crate::build_field_specs) — même logique que build.rs
    // utilise pour construire le SchemaIndex passé à resolve_and_measure.
    let field_specs: Vec<FieldSpec> = crate::build_field_specs(columns);

    // pk_field : résolution pour record_id() (invariant : PK Single dans field_specs).
    let pk_col_name: &str = match pk {
        PrimaryKey::Single(col) => col.as_str(),
        PrimaryKey::Composite   => fixed_cols.first().copied().unwrap_or("id"),
    };
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

    // ── OnceLock statique — PackfileReader monté au premier appel de fetch_batch ──
    // Déclaration au niveau module : durée de vie 'static garantie.
    // OnceLock est thread-safe sans verrou — Mmap est Send + Sync.
    writeln!(out,
        "static {screaming}_STORE: std::sync::OnceLock<\
         marius_projection::packfile_reader::PackfileReader<{proj_name}>> = std::sync::OnceLock::new();"
    ).unwrap();
    writeln!(out).unwrap();

    writeln!(out, "// Phase 2 AOT : _pool ignoré — lecture via OnceLock<PackfileReader>.").unwrap();
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
    writeln!(out, "    ) -> Result<Vec<(Self::Record, Self::VarlenOwned)>, sqlx::Error> {{").unwrap();

    if fixed_cols.is_empty() {
        writeln!(out,
            "        todo!(\"DB-Forge: aucune colonne fixed-length pour {schema}.{table}\")"
        ).unwrap();
    } else {
        // Montage du PackfileReader au premier appel — OnceLock garantit l'unicité.
        writeln!(out, "        let reader = {screaming}_STORE.get_or_init(|| {{").unwrap();
        writeln!(out,
            "            marius_projection::packfile_reader::PackfileReader::open(&{proj_name}::store_path())"
        ).unwrap();
        writeln!(out,
            "                .expect(\"[fetch_batch:{schema}.{table}] store.bin absent \
             — exécuter marius-dump avant de démarrer le serveur\")"
        ).unwrap();
        writeln!(out, "        }});").unwrap();
        writeln!(out).unwrap();

        // Itération sur les ids demandés — lookup O(log N) par binary search.
        // Les ids absents du store sont silencieusement ignorés (enreg. supprimé).
        writeln!(out, "        let mut batch = Vec::with_capacity(ids.len());").unwrap();
        writeln!(out, "        for &id in ids {{").unwrap();
        writeln!(out, "            if let Some((record, _vrefs)) = reader.lookup(id) {{").unwrap();

        if varlena.is_empty() {
            // Table sans varlena : copie du Record, VarlenOwned = ().
            writeln!(out, "                batch.push((*record, ()));").unwrap();
        } else {
            // Construction VarlenOwned depuis VarlenRefs (vues mmap → String owned).
            // to_owned() : unique allocation tolérée — bornée, isolée avant render().
            writeln!(out, "                let owned = {name}VarlenOwned {{").unwrap();
            for (i, v) in varlena.iter().enumerate() {
                writeln!(out,
                    "                    {}: _vrefs.get({i}).map(str::to_owned),",
                    v.name
                ).unwrap();
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
    writeln!(out, "    ) -> Result<Vec<(Self::Record, Self::VarlenOwned)>, sqlx::Error> {{").unwrap();

    if fixed_cols.is_empty() {
        writeln!(out,
            "        todo!(\"DB-Forge: aucune colonne fixed-length pour {schema}.{table}\")"
        ).unwrap();
    } else {
        writeln!(out, "        let rows = sqlx::query_as::<_, {name}Row>(").unwrap();
        writeln!(out, "            \"SELECT {select} FROM {from_clause} {where_clause}\",").unwrap();
        writeln!(out, "        )").unwrap();
        writeln!(out, "        .bind(ids)").unwrap();
        writeln!(out, "        .fetch_all(pool)").unwrap();
        writeln!(out, "        .await?;").unwrap();

        if varlena.is_empty() {
            // Pas de varlena : From<Row> pour la conversion.
            writeln!(out,
                "        Ok(rows.into_iter().map(|r| ({name}StorageRow::from(r), ())).collect())"
            ).unwrap();
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
            let mut max_align    = 1usize;

            for col in columns {
                let m = map_type(&col.sql_type);
                if !m.is_fixed { continue; }

                layout_bytes += m.size_bytes;
                max_align     = max_align.max(m.alignment);

                let sentinel = col.sentinel.as_deref().unwrap_or(m.default_sentinel);

                let mut expr = if col.is_notnull {
                    match m.row_type {
                        "chrono::DateTime<chrono::Utc>" =>
                            format!("{}.timestamp_micros()", col.name),
                        "chrono::NaiveDateTime" =>
                            format!("{}.and_utc().timestamp_micros()", col.name),
                        "chrono::NaiveDate" =>
                            format!("{}.num_days_from_ce()", col.name),
                        _ => col.name.clone(),
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
            let tail_pad    = padded_size - layout_bytes;
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

    // ── render() ─────────────────────────────────────────────────────────────
    // varlena param : "_varlena" tant qu'aucun corps réel ne le consomme
    // (stub Voie B) ou que la table n'a pas de varlena. Dès qu'un template
    // résolu (render.is_some()) référence varlena, le préfixe disparaît —
    // generate_aot_snippet émet `varlena.{field}.as_deref()` qui exige le nom.
    let body_is_real = render.is_some();
    let varlena_param = if varlena.is_empty() {
        "_varlena: &()".to_string()
    } else if body_is_real {
        format!("varlena: &{name}VarlenOwned")
    } else {
        format!("_varlena: &{name}VarlenOwned")
    };
    writeln!(out,
        "    fn render(record: &Self::Record, {varlena_param}, buf: &mut String) {{"
    ).unwrap();
    if render_body.is_empty() {
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
    writeln!(out, "        let root = std::env::var(\"MARIUS_ARTIFACTS_DIR\")").unwrap();
    writeln!(out, "            .unwrap_or_else(|_| \"artifacts\".to_string());").unwrap();
    writeln!(out,
        "        ::std::path::PathBuf::from(format!(\"{{root}}/{schema}_{table}_pack.bin\"))"
    ).unwrap();
    writeln!(out, "    }}").unwrap();
    writeln!(out).unwrap();

    // ── store_path() (Phase 1.4 : binary dump) ────────────────────────────────
    writeln!(out, "    fn store_path() -> ::std::path::PathBuf {{").unwrap();
    writeln!(out, "        let root = std::env::var(\"MARIUS_ARTIFACTS_DIR\")").unwrap();
    writeln!(out, "            .unwrap_or_else(|_| \"artifacts\".to_string());").unwrap();
    writeln!(out,
        "        ::std::path::PathBuf::from(format!(\"{{root}}/{schema}_{table}_store.bin\"))"
    ).unwrap();
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
        writeln!(out, "        toc:   &mut Vec<marius_projection::VarlenSlot>,").unwrap();
        writeln!(out, "    ) {{").unwrap();
        for v in varlena {
            writeln!(out, "        match owned.{}.as_deref() {{", v.name).unwrap();
            writeln!(out, "            Some(s) if !s.is_empty() => {{").unwrap();
            writeln!(out, "                let offset = heap.len() as u32;").unwrap();
            writeln!(out, "                let len    = s.len() as u32;").unwrap();
            writeln!(out, "                heap.extend_from_slice(s.as_bytes());").unwrap();
            writeln!(out, "                toc.push(marius_projection::VarlenSlot {{ offset, len }});").unwrap();
            writeln!(out, "            }}").unwrap();
            writeln!(out, "            _ => toc.push(marius_projection::VarlenSlot {{ offset: u32::MAX, len: 0 }}),").unwrap();
            writeln!(out, "        }}").unwrap();
        }
        writeln!(out, "    }}").unwrap();
    }

    writeln!(out, "}}\n").unwrap();
}
