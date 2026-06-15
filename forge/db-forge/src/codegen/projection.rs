// =============================================================================
// marius-db-forge · codegen/projection.rs
// Génération du stub impl Projection pour une table.
// =============================================================================

use std::fmt::Write as _;

use crate::mapping::{Column, PrimaryKey, map_type};
use crate::naming::{to_pascal, to_screaming};
use marius_fragment_forge::{FieldSpec, FieldKind, VarlenField, generate_render, generate_capacity_consts};

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
pub fn write_projection_stub(
    out:          &mut String,
    schema:       &str,
    table:        &str,
    columns:      &[Column],
    pk:           &PrimaryKey,
    varlena:      &[VarlenField],
    varlena_join: Option<(&str, &str, &str)>,
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

    // ── Construction SELECT + FROM ────────────────────────────────────────────
    let (select, from_clause) = if let Some((vs, vt, _fk)) = varlena_join {
        let varlena_cols: Vec<String> = varlena.iter()
            .map(|v| format!("{vt}.{}", v.name))
            .collect();
        let all_cols: Vec<String> = fixed_cols.iter().map(|c| c.to_string())
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
    // Seules les colonnes fixed-length connues de FieldKind sont incluses.
    // Les types PHASE2_ONLY (pg_lsn) ou inconnus sont silencieusement exclus.
    let field_specs: Vec<FieldSpec> = columns.iter()
        .filter(|c| map_type(&c.sql_type).is_fixed)
        .filter_map(|c| {
            FieldKind::from_sql_type(&c.sql_type).map(|kind| FieldSpec {
                name:   c.name.clone(),
                kind,
                attnum: c.attnum,
            })
        })
        .collect();

    // pk_field : &FieldSpec — contrat generate_render (signature scellée).
    // Résolution : chercher dans field_specs le champ portant le nom PK.
    // Fallback sur le premier FieldSpec si PK composite (cas dégradé).
    let pk_col_name: &str = match pk {
        PrimaryKey::Single(col) => col.as_str(),
        PrimaryKey::Composite   => fixed_cols.first().copied().unwrap_or("id"),
    };
    let pk_field: &FieldSpec = field_specs
        .iter()
        .find(|f| f.name == pk_col_name)
        .unwrap_or_else(|| {
            // Invariant : toute table avec PK Single a son champ PK dans field_specs.
            // Un panic ici indique un type PK non supporté par FieldKind (ex: uuid).
            panic!(
                "DB-Forge [{schema}.{table}]: champ PK '{pk_col_name}' \
                 absent des FieldSpecs — type PK non supporté par FieldKind. \
                 Déclarer la colonne PK avec un type fixed-length reconnu."
            )
        });

    let (static_cap, dynamic_cap, render_body) = generate_render(
        schema, table, &name,
        &field_specs,
        pk_field,
        varlena,
    );
    let cap_consts = generate_capacity_consts(&screaming, static_cap, dynamic_cap);

    // ── Émission ──────────────────────────────────────────────────────────────
    writeln!(out, "pub struct {proj_name};").unwrap();
    writeln!(out).unwrap();

    // Constantes au niveau module (pas dans le bloc impl).
    writeln!(out, "{cap_consts}").unwrap();

    writeln!(out, "// Pool requis : marius_user (SELECT sur {schema}.{table})").unwrap();
    writeln!(out, "// RLS         : voir 09_rls/01_policies.sql").unwrap();
    writeln!(out, "impl crate::projection::Projection for {proj_name} {{").unwrap();
    writeln!(out, "    type Record = {name}StorageRow;").unwrap();
    writeln!(out, "    type VarlenOwned = {varlen_owned_type};").unwrap();
    writeln!(out).unwrap();

    // ── fetch_batch ───────────────────────────────────────────────────────────
    writeln!(out, "    async fn fetch_batch(").unwrap();
    writeln!(out, "        pool: &sqlx::PgPool,").unwrap();
    writeln!(out, "        ids:  &[i64],").unwrap();
    writeln!(out, "    ) -> Result<Vec<(Self::Record, Self::VarlenOwned)>, sqlx::Error> {{").unwrap();

    if fixed_cols.is_empty() {
        writeln!(out,
            "        todo!(\"DB-Forge: aucune colonne fixed-length pour {schema}.{table}\")"
        ).unwrap();
    } else {
        writeln!(out, "        let rows = sqlx::query_as::<_, {name}Row>(").unwrap();
        writeln!(out,
            "            \"SELECT {select} FROM {from_clause} {where_clause}\","
        ).unwrap();
        writeln!(out, "        )").unwrap();
        writeln!(out, "        .bind(ids)").unwrap();
        writeln!(out, "        .fetch_all(pool)").unwrap();
        writeln!(out, "        .await?;").unwrap();

        if varlena.is_empty() {
            // Pas de varlena : From<Row> consomme r entièrement.
            writeln!(out,
                "        Ok(rows.into_iter().map(|r| ({name}StorageRow::from(r), ())).collect())"
            ).unwrap();
        } else {
            // Avec varlena : déstructuration complète pour éviter E0382 (partial move).
            // From<{Name}Row> N'EST PAS appelé ici — logique de conversion reproduite inline.
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

            // StorageRow depuis les bindings fixed — logique From<Row> inline.
            writeln!(out, "            let storage = {name}StorageRow {{").unwrap();

            let mut layout_bytes = 0usize;
            let mut max_align    = 1usize;

            for col in columns {
                let m = map_type(&col.sql_type);
                if !m.is_fixed { continue; }
                // Accumulation binaire pour le calcul du padding
                layout_bytes += m.size_bytes;
                max_align     = max_align.max(m.alignment);

                let mut expr = if col.is_notnull {
                    match m.row_type {
                        "chrono::DateTime<chrono::Utc>" => {
                            format!("{}.timestamp_micros()", col.name)
                        }
                        "chrono::NaiveDateTime" => {
                            format!("{}.and_utc().timestamp_micros()", col.name)
                        }
                        "chrono::NaiveDate" => {
                            format!("{}.num_days_from_ce()", col.name)
                        }
                        _ => col.name.clone(),
                    }
                } else {
                    m.from_expr.replace("{field}", &col.name)
                };

                // Cast explicite vers le type compact de destination (u8)
                if m.row_type == "bool" {
                    expr = format!("({expr}) as u8");
                }
                
                writeln!(out, "                {}: {},", col.name, expr).unwrap();
            }

            // Injection du tail padding structurel pour satisfaire l'alignement et bytemuck
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

    // ── render() ─────────────────────────────────────────────────────────────
    let varlena_param = if varlena.is_empty() {
        "_varlena: &()".to_string()
    } else {
        format!("varlena: &{name}VarlenOwned")
    };
    writeln!(out,
        "    fn render(record: &Self::Record, {varlena_param}, buf: &mut String) {{"
    ).unwrap();
    for line in render_body.lines() {
        writeln!(out, "    {line}").unwrap();
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

    writeln!(out, "}}\n").unwrap();
}
