// =============================================================================
// DB-Forge — crates/core/schema/build.rs
// Projet Marius · ADR-002 / Codegen.md
//
// Génère dans $OUT_DIR/generated_schema.rs :
//   - Une struct {Name}Row   : désérialisation sqlx (Option<T> pour nullable)
//   - Une struct {Name}      : stockage repr(C) (sentinel pour nullable fixe)
//   - From<{Name}Row> for {Name}
//   - static {NAME}_COLLECTOR: Collector<MAX>
//   - stub impl Projection
//
// Prérequis : DATABASE_URL pointe vers marius avec rôle marius_admin.
// Exécution : automatique via `cargo build`.
// =============================================================================

use std::env;
use std::fmt::Write as FmtWrite;
use std::fs;
use std::path::PathBuf;

// sqlx::Row : nécessaire pour .get::<T, _>(index) sur les résultats non-macro.
use sqlx::Row;

// Fragment-Forge : génération des corps render() + calcul capacité statique.
use marius_fragment_forge::{FieldSpec, FieldKind, generate_render, generate_capacity_consts};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-env-changed=DATABASE_URL");
    println!("cargo:rerun-if-changed=build.rs");

    let database_url = env::var("DATABASE_URL")
        .expect("DB-Forge : DATABASE_URL non définie.");

    let pool = sqlx::PgPool::connect(&database_url).await?;

    // -------------------------------------------------------------------------
    // Tables surveillées.
    // Prototype : deux tables à profils complémentaires.
    //   content.core       : PK=document_id, 3 nullable, RLS activé
    //   commerce.product_core : PK=id (attnum=2, pas attnum=1), 2 nullable, fillfactor=80
    //
    // Extension future : lire cette liste depuis meta.containment_intent.
    // -------------------------------------------------------------------------
    let watched: &[(&str, &str)] = &[
        ("content",  "core"),
        ("commerce", "product_core"),
    ];

    let out_dir  = PathBuf::from(env::var("OUT_DIR")?);
    let out_path = out_dir.join("generated_schema.rs");

    let mut output = String::from(
        "// GÉNÉRÉ PAR DB-FORGE — NE PAS MODIFIER MANUELLEMENT\n\
         // Régénérer via : cargo build (relit pg_attribute)\n\n\
         use std::path::PathBuf;\n\n",
    );

    for &(schema, table) in watched {
        let columns = fetch_columns(&pool, schema, table).await?;
        let pk      = fetch_pk_column(&pool, schema, table).await?;
        let max_id  = match &pk {
            PrimaryKey::Single(col) => Some(fetch_max_id(&pool, schema, table, col).await?),
            PrimaryKey::Composite   => None,
        };

        write_section_header(&mut output, schema, table, &pk);
        write_row_struct(&mut output, schema, table, &columns);
        write_store_struct(&mut output, schema, table, &columns);
        write_from_impl(&mut output, schema, table, &columns);

        if let (PrimaryKey::Single(col), Some(max)) = (&pk, max_id) {
            write_collector(&mut output, schema, table, col, max);
        }

        write_projection_stub(&mut output, schema, table, &columns, &pk);
    }

    fs::write(&out_path, &output)?;
    eprintln!("DB-Forge : généré → {}", out_path.display());
    Ok(())
}

// =============================================================================
// Types internes
// =============================================================================

#[derive(Debug)]
struct Column {
    attnum:     i16,
    name:       String,
    sql_type:   String,
    is_notnull: bool,
}

#[derive(Debug)]
enum PrimaryKey {
    Single(String),    // nom de la colonne PK
    Composite,         // PK composée — pas de Collector possible
}

// =============================================================================
// I. Introspection
// =============================================================================

/// Colonnes dans l'ordre physique du heap (attnum ASC).
/// ORDER BY attnum est l'invariant de Symétrie Mécanique.
async fn fetch_columns(
    pool:   &sqlx::PgPool,
    schema: &str,
    table:  &str,
) -> Result<Vec<Column>, sqlx::Error> {
    // API non-macro : pas de vérification compile-time, DATABASE_URL non requise
    // pendant la phase de build du build script.
    let rows = sqlx::query(
        "SELECT
             a.attnum::smallint,
             a.attname::text,
             format_type(a.atttypid, a.atttypmod),
             a.attnotnull
         FROM  pg_attribute  a
         JOIN  pg_class      c ON a.attrelid = c.oid
         JOIN  pg_namespace  n ON c.relnamespace = n.oid
         WHERE n.nspname     = $1
           AND c.relname     = $2
           AND a.attnum      > 0
           AND NOT a.attisdropped
         ORDER BY a.attnum",
    )
    .bind(schema)
    .bind(table)
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(|r| Column {
        attnum:     r.get::<i16,    _>(0),
        name:       r.get::<String, _>(1),
        sql_type:   r.get::<String, _>(2),
        is_notnull: r.get::<bool,   _>(3),
    }).collect())
}

/// Identifie la PK.
/// Retourne Single(col) si PK sur une colonne, Composite sinon.
/// Une PK Composite rend le Collector inapplicable.
async fn fetch_pk_column(
    pool:   &sqlx::PgPool,
    schema: &str,
    table:  &str,
) -> Result<PrimaryKey, sqlx::Error> {
    // information_schema évite les subtilités de cast int2 de pg_catalog.
    // fetch_all + match sur la longueur remplace fetch_one pour éviter RowNotFound.
    let rows = sqlx::query(
        "SELECT kcu.column_name::text
         FROM   information_schema.table_constraints  tc
         JOIN   information_schema.key_column_usage   kcu
                ON  kcu.constraint_name = tc.constraint_name
                AND kcu.table_schema    = tc.table_schema
                AND kcu.table_name      = tc.table_name
         WHERE  tc.table_schema     = $1
           AND  tc.table_name       = $2
           AND  tc.constraint_type  = 'PRIMARY KEY'
         ORDER BY kcu.ordinal_position",
    )
    .bind(schema)
    .bind(table)
    .fetch_all(pool)
    .await?;

    match rows.len() {
        0 => {
            eprintln!("DB-Forge [{schema}.{table}] : aucune PK trouvée — traité comme Composite.");
            Ok(PrimaryKey::Composite)
        }
        1 => Ok(PrimaryKey::Single(rows[0].get::<String, _>(0))),
        n => {
            eprintln!("DB-Forge [{schema}.{table}] : PK composite ({n} colonnes) — Collector ignoré.");
            Ok(PrimaryKey::Composite)
        }
    }
}

/// MAX(pk_col) + marge 20% + arrondi power-of-two en words.
/// La colonne PK n'est pas nécessairement 'id' (ex: document_id sur content.core).
async fn fetch_max_id(
    pool:    &sqlx::PgPool,
    schema:  &str,
    table:   &str,
    pk_col:  &str,
) -> Result<usize, Box<dyn std::error::Error>> {
    // format! obligatoire : sqlx ne supporte pas l'interpolation d'identifiants.
    let query = format!(
        "SELECT COALESCE(MAX({pk_col}), 0)::BIGINT FROM {schema}.{table}"
    );

    let max_id: i64 = sqlx::query_scalar::<_, i64>(&query)
        .fetch_one(pool)
        .await
        .unwrap_or(0);

    let with_margin   = (max_id as f64 * 1.20).ceil() as usize;
    let words_needed  = with_margin.max(64).div_ceil(64);
    let words_aligned = words_needed.next_power_of_two();
    let max_entity_id = words_aligned * 64;

    eprintln!(
        "DB-Forge [{schema}.{table}] : MAX({pk_col})={max_id} → \
         MAX_ENTITY_ID={max_entity_id} ({} KB)",
        (words_aligned * 8) / 1024,
    );
    Ok(max_entity_id)
}

// =============================================================================
// II. Mapping de types
// =============================================================================

/// Informations de mapping pour un type SQL.
#[derive(Debug, Clone)]
struct TypeMapping {
    /// Type Rust dans la struct Row (sqlx-compatible).
    row_type:   &'static str,

    /// Type Rust dans la struct Store (#[repr(C)]).
    store_type: &'static str,

    /// Expression de conversion Row → Store pour ce champ.
    from_expr:  &'static str,

    /// Le type est fixed-length (inclus dans repr(C)).
    is_fixed:   bool,

    /// Taille en octets du type (pour calcul layout et static_assertions).
    size_bytes: usize,

    /// Alignement du type en octets — détermine le padding repr(C) et
    /// l'alignement de la struct entière (max des champs).
    /// Utilisé pour émettre assert!(mem::align_of::<Struct>() == max_align).
    alignment:  usize,
}

fn map_type(sql_type: &str) -> TypeMapping {
    // Normaliser : "character varying(255)" → "character varying"
    let t = sql_type.split('(').next().unwrap_or(sql_type).trim().to_lowercase();

    match t.as_str() {
        "int8" | "bigint" => TypeMapping {
            row_type: "i64", store_type: "i64",
            // Sentinel -1 : CHECK (price_cents >= 0) garantit que -1 = absent.
            // Pour les timestamps INT8, sentinel 0 (epoch Unix est invalide en pratique).
            // ATTENTION : le choix du sentinel est domain-specific.
            // La Forge full demandera une annotation par colonne.
            from_expr: "{field}.unwrap_or(-1)",
            is_fixed: true, size_bytes: 8, alignment: 8,
        },
        "int4" | "integer" | "int" | "serial" => TypeMapping {
            row_type: "i32", store_type: "i32",
            // Sentinel 0 : les IDs PostgreSQL (GENERATED ALWAYS AS IDENTITY) commencent à 1.
            from_expr: "{field}.unwrap_or(0)",
            is_fixed: true, size_bytes: 4, alignment: 4,
        },
        "int2" | "smallint" => TypeMapping {
            row_type: "i16", store_type: "i16",
            from_expr: "{field}.unwrap_or(0)",
            is_fixed: true, size_bytes: 2, alignment: 2,
        },
        "bool" | "boolean" => TypeMapping {
            row_type: "bool", store_type: "bool",
            from_expr: "{field}.unwrap_or(false)",
            is_fixed: true, size_bytes: 1, alignment: 1,
        },
        "uuid" => TypeMapping {
            row_type: "[u8; 16]", store_type: "[u8; 16]",
            from_expr: "{field}.unwrap_or([0u8; 16])",
            is_fixed: true, size_bytes: 16, alignment: 1,
        },
        // TIMESTAMPTZ → i64 µs depuis l'epoch Unix.
        // La Row utilise chrono::DateTime<Utc>, le Store utilise i64.
        // Sentinel 0 = absent (epoch 1970-01-01 00:00:00 UTC est invalide en pratique).
        "timestamptz" | "timestamp with time zone" => TypeMapping {
            row_type:   "chrono::DateTime<chrono::Utc>",
            store_type: "i64",
            from_expr:  "{field}.map(|dt| dt.timestamp_micros()).unwrap_or(0)",
            is_fixed: true, size_bytes: 8, alignment: 8,
        },
        "timestamp" | "timestamp without time zone" => TypeMapping {
            row_type:   "chrono::NaiveDateTime",
            store_type: "i64",
            from_expr:  "{field}.map(|dt| dt.and_utc().timestamp_micros()).unwrap_or(0)",
            is_fixed: true, size_bytes: 8, alignment: 8,
        },
        "date" => TypeMapping {
            row_type: "chrono::NaiveDate", store_type: "i32",
            from_expr: "{field}.map(|d| d.num_days_from_ce()).unwrap_or(0)",
            is_fixed: true, size_bytes: 4, alignment: 4,
        },
        "float4" | "real"             => TypeMapping {
            row_type: "f32", store_type: "f32",
            from_expr: "{field}.unwrap_or(0.0)",
            is_fixed: true, size_bytes: 4, alignment: 4,
        },
        "float8" | "double precision" => TypeMapping {
            row_type: "f64", store_type: "f64",
            from_expr: "{field}.unwrap_or(0.0)",
            is_fixed: true, size_bytes: 8, alignment: 8,
        },
        // Varlena : excluded du Store repr(C).
        // Restent dans la Row pour projection Fragment-Forge.
        "text" | "varchar" | "character varying"
        | "jsonb" | "json" | "bytea" | "ltree" => TypeMapping {
            row_type: "String", store_type: "/* VARLENA */",
            from_expr: "/* VARLENA — non transféré */",
            is_fixed: false, size_bytes: 0, alignment: 0,
        },
        // pg_lsn : type PostgreSQL natif (8B, LSN du WAL).
        // Phase 1 : exclu des structs Rust (non utilisé pour le rendu).
        // Phase 2 (SHM) : le Store lira walsn via pointeur mmap → u64.
        // La colonne existe en DDL pour le mécanisme de resync LSN.
        "pg_lsn" => TypeMapping {
            row_type:   "/* PHASE2_ONLY: walsn → u64 via mmap */",
            store_type: "/* PHASE2_ONLY */",
            from_expr:  "/* PHASE2_ONLY */",
            is_fixed: false, size_bytes: 8, alignment: 8,
        },
        other => {
            println!("cargo:warning=DB-Forge : type SQL inconnu '{other}' — exclu");
            TypeMapping {
                row_type: "/* INCONNU */", store_type: "/* INCONNU */",
                from_expr: "/* INCONNU */",
                is_fixed: false, size_bytes: 0, alignment: 0,
            }
        }
    }
}

// =============================================================================
// III. Génération — Struct Row (sqlx-compatible)
// =============================================================================

fn write_row_struct(out: &mut String, schema: &str, table: &str, columns: &[Column]) {
    let name = to_pascal(&format!("{schema}_{table}"));
    writeln!(out, "/// Struct de désérialisation sqlx — types natifs, Option<T> pour nullable.").unwrap();
    writeln!(out, "/// NON repr(C) : utilisée uniquement comme intermédiaire de transport.").unwrap();
    writeln!(out, "#[derive(Debug, sqlx::FromRow)]").unwrap();
    writeln!(out, "pub struct {name}Row {{").unwrap();

    for col in columns {
        let m = map_type(&col.sql_type);
        if m.is_fixed {
            if col.is_notnull {
                writeln!(out, "    pub {}: {},", col.name, m.row_type).unwrap();
            } else {
                writeln!(out, "    pub {}: Option<{}>,  // NULLABLE", col.name, m.row_type).unwrap();
            }
        } else {
            // Types non-fixed : varlena incluse dans la Row, types commentaires exclus.
            if m.row_type.starts_with("/*") {
                // Type Phase 2 ou inconnu : commentaire uniquement, aucun champ généré.
                // pg_lsn (walsn) est le cas nominal — exclu de la Row Phase 1.
                writeln!(out,
                    "    // EXCLU Phase 1 : {} ({}) — {}",
                    col.name, col.sql_type, m.row_type
                ).unwrap();
            } else if col.is_notnull {
                writeln!(out, "    pub {}: {},  // varlena", col.name, m.row_type).unwrap();
            } else {
                writeln!(out, "    pub {}: Option<{}>,  // varlena NULLABLE", col.name, m.row_type).unwrap();
            }
        }
    }
    writeln!(out, "}}\n").unwrap();
}

// =============================================================================
// IV. Génération — Struct Store (#[repr(C)])
// =============================================================================

fn write_store_struct(out: &mut String, schema: &str, table: &str, columns: &[Column]) {
    let name = to_pascal(&format!("{schema}_{table}"));

    writeln!(out, "/// Struct de stockage en mémoire — repr(C), types fixed-length uniquement.").unwrap();
    writeln!(out, "/// Nullable → sentinel (0 pour IDs, 0 pour timestamps, -1 pour INT8 signé).").unwrap();
    writeln!(out, "/// Varlena exclues : projetées séparément par Fragment-Forge.").unwrap();
    writeln!(out, "///").unwrap();
    writeln!(out, "/// AVERTISSEMENT NULLABLE : le choix du sentinel est domain-specific.").unwrap();
    writeln!(out, "/// La Forge full requiert une annotation par colonne nullable.").unwrap();
    writeln!(out, "/// Pour ce prototype, les valeurs par défaut de map_type() s'appliquent.").unwrap();
    writeln!(out, "#[repr(C)]").unwrap();
    writeln!(out, "#[derive(Debug, Clone, Copy, Default)]").unwrap();
    writeln!(out, "pub struct {name} {{").unwrap();

    let mut layout_bytes = 0usize;
    let mut max_align    = 1usize;
    for col in columns {
        let m = map_type(&col.sql_type);
        if m.is_fixed {
            let null_marker = if col.is_notnull { "" } else { " [sentinel]" };
            writeln!(out,
                "    pub {}: {},{}  // attnum={}, {}B{}",
                col.name, m.store_type,
                if col.name.len() < 20 { " ".repeat(20 - col.name.len().min(20)) } else { String::new() },
                col.attnum, m.size_bytes, null_marker,
            ).unwrap();
            layout_bytes += m.size_bytes;
            max_align     = max_align.max(m.alignment);
        } else {
            writeln!(out,
                "    // VARLENA exclu : {} ({}) — Fragment-Forge",
                col.name, col.sql_type
            ).unwrap();
        }
    }
    writeln!(out, "}}").unwrap();

    // Taille padded repr(C) = arrondi au multiple de max_align supérieur.
    let padded_size = layout_bytes.div_ceil(max_align.max(1)) * max_align.max(1);

    writeln!(out, "// Layout fixed-length : {layout_bytes}B données → {padded_size}B padded (align={max_align}B)").unwrap();
    writeln!(out, "// + {}B header heap PostgreSQL (MAXALIGN(23 + ceil({}/8)))",
        { let n = columns.len(); (23 + n.div_ceil(8)).div_ceil(8) * 8 },
        columns.len()
    ).unwrap();
    writeln!(out).unwrap();

    // static_assertions — vérifie la symétrie binaire repr(C) ↔ DDL PostgreSQL.
    // Critique pour la Phase 2 (SHM) : une divergence d'un octet produit
    // de la corruption silencieuse lors du mmap::ptr::read().
    writeln!(out,
        "const _: () = assert!(\n    \
         std::mem::size_of::<{name}>() == {padded_size},\n    \
         \"DB-Forge [{schema}.{table}]: size_of diverge du DDL — reconstruire après ALTER TABLE\",\n\
         );"
    ).unwrap();
    writeln!(out,
        "const _: () = assert!(\n    \
         std::mem::align_of::<{name}>() == {max_align},\n    \
         \"DB-Forge [{schema}.{table}]: align_of diverge du DDL — vérifier les types colonnes\",\n\
         );"
    ).unwrap();
    writeln!(out).unwrap();
}

// =============================================================================
// V. Génération — From<Row> for Store
// =============================================================================

fn write_from_impl(out: &mut String, schema: &str, table: &str, columns: &[Column]) {
    let name = to_pascal(&format!("{schema}_{table}"));
    writeln!(out, "impl From<{name}Row> for {name} {{").unwrap();
    writeln!(out, "    fn from(r: {name}Row) -> Self {{").unwrap();
    writeln!(out, "        Self {{").unwrap();

    for col in columns {
        let m = map_type(&col.sql_type);
        if !m.is_fixed { continue; }

        let expr = if col.is_notnull {
            // NOT NULL : conversion directe (Row et Store ont le même type de base)
            match m.row_type {
                "chrono::DateTime<chrono::Utc>" => {
                    format!("r.{}.timestamp_micros()", col.name)
                }
                "chrono::NaiveDateTime" => {
                    format!("r.{}.and_utc().timestamp_micros()", col.name)
                }
                "chrono::NaiveDate" => {
                    format!("r.{}.num_days_from_ce()", col.name)
                }
                _ => format!("r.{}", col.name),
            }
        } else {
            // NULLABLE : déplie Option via le sentinel défini dans map_type
            m.from_expr.replace("{field}", &format!("r.{}", col.name))
        };

        writeln!(out, "            {}: {},", col.name, expr).unwrap();
    }

    writeln!(out, "        }}").unwrap();
    writeln!(out, "    }}").unwrap();
    writeln!(out, "}}\n").unwrap();
}

// =============================================================================
// VI. Génération — Collector<MAX> statique
// =============================================================================

fn write_collector(
    out:           &mut String,
    schema:        &str,
    table:         &str,
    pk_col:        &str,
    max_entity_id: usize,
) {
    let screaming = to_screaming(&format!("{schema}_{table}"));
    let words     = max_entity_id.div_ceil(64);

    writeln!(out, "// Collector dimensionné pour {schema}.{table}").unwrap();
    writeln!(out, "// PK = {pk_col} | MAX_ID+20% arrondi power-of-two").unwrap();
    writeln!(out, "pub const MAX_{screaming}_ID: usize = {max_entity_id};").unwrap();
    writeln!(out, "pub const {screaming}_WORDS: usize = {words};").unwrap();
    // Deux const generics : MAX (borne domaine) + WORDS (taille tableau).
    // generic_const_exprs est instable — relation imposée par la Forge, pas le type system.
    writeln!(out, "pub static {screaming}_COLLECTOR: crate::collector::Collector<MAX_{screaming}_ID, {screaming}_WORDS> =").unwrap();
    writeln!(out, "    crate::collector::Collector::new_zeroed();\n").unwrap();
}

// =============================================================================
// VII. Génération — stub impl Projection
// =============================================================================

fn write_projection_stub(
    out:     &mut String,
    schema:  &str,
    table:   &str,
    columns: &[Column],
    pk:      &PrimaryKey,
) {
    let name      = to_pascal(&format!("{schema}_{table}"));
    let proj_name = format!("{name}Projection");

    // Colonnes fixed-length uniquement dans le SELECT.
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

    let select = fixed_cols.join(", ");

    let where_clause = match pk {
        PrimaryKey::Single(col) => format!("WHERE {col} = ANY($1) ORDER BY {col} ASC"),
        PrimaryKey::Composite   => "WHERE 1=1 /* PK composite: adapter */".to_string(),
    };

    // Fragment-Forge : calculer AVANT d'émettre quoi que ce soit.
    // Les constantes de capacité sont émises au niveau MODULE (entre struct et impl),
    // pas dans le bloc impl (où elles seraient des constantes associées invalides
    // car non déclarées dans le trait Projection).
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

    let (static_cap, dynamic_cap, render_body) = generate_render(
        schema, table, &name, &field_specs,
        // PK réelle issue de pg_constraint — pas le premier champ par attnum.
        match pk {
            PrimaryKey::Single(col) => col.as_str(),
            PrimaryKey::Composite   => field_specs.first().map(|f| f.name.as_str()).unwrap_or("id"),
        },
    );
    let screaming   = to_screaming(&format!("{schema}_{table}"));
    let cap_consts  = generate_capacity_consts(&screaming, static_cap, dynamic_cap);

    // Struct + constantes au niveau module
    writeln!(out, "pub struct {proj_name};").unwrap();
    writeln!(out).unwrap();
    writeln!(out, "{cap_consts}").unwrap();  // ← niveau module ✓
    writeln!(out, "// Pool requis : marius_user (SELECT non révoqué sur {schema}.{table})").unwrap();
    writeln!(out, "// RLS         : voir 09_rls/01_policies.sql").unwrap();
    writeln!(out, "impl crate::projection::Projection for {proj_name} {{").unwrap();
    writeln!(out, "    type Record = {name};").unwrap();
    writeln!(out).unwrap();

    // fetch_batch — async fn satisfait fn -> impl Future + Send dans le trait.
    // API non-macro sqlx::query_as::<_, RowType>() :
    //   - évite la vérification DATABASE_URL au compile-time de marius-schema
    //   - le build script a déjà validé le schéma via pg_attribute
    writeln!(out, "    async fn fetch_batch(").unwrap();
    writeln!(out, "        pool: &sqlx::PgPool,").unwrap();
    writeln!(out, "        ids:  &[i64],").unwrap();
    writeln!(out, "    ) -> Result<Vec<Self::Record>, sqlx::Error> {{").unwrap();

    if fixed_cols.is_empty() {
        writeln!(out,
            "        todo!(\"DB-Forge: aucune colonne fixed-length pour {schema}.{table}\")"
        ).unwrap();
    } else {
        writeln!(out, "        let rows = sqlx::query_as::<_, {name}Row>(").unwrap();
        writeln!(out,
            "            \"SELECT {select} FROM {schema}.{table} {where_clause}\","
        ).unwrap();
        writeln!(out, "        )").unwrap();
        writeln!(out, "        .bind(ids)").unwrap();
        writeln!(out, "        .fetch_all(pool)").unwrap();
        writeln!(out, "        .await?;").unwrap();
        writeln!(out, "        Ok(rows.into_iter().map({name}::from).collect())").unwrap();
    }

    writeln!(out, "    }}").unwrap();
    writeln!(out).unwrap();

    // render — généré par Fragment-Forge.
    // Corps : push_str (statique) + write! (dynamique). Zéro allocation intermédiaire.
    // field_specs, render_body et cap_consts ont été calculés en tête de fonction.
    writeln!(out, "    fn render(record: &Self::Record, buf: &mut String) {{").unwrap();
    for line in render_body.lines() {
        writeln!(out, "    {line}").unwrap();
    }
    writeln!(out, "    }}").unwrap();
    writeln!(out).unwrap();

    // artifact_path — chemin déterministe basé sur la PK.
    let pk_field = match pk {
        PrimaryKey::Single(col) => col.as_str(),
        PrimaryKey::Composite   => "0",
    };
    writeln!(out, "    fn artifact_path(record: &Self::Record) -> PathBuf {{").unwrap();
    writeln!(out, "        let root = std::env::var(\"MARIUS_ARTIFACTS_DIR\")").unwrap();
    writeln!(out, "            .unwrap_or_else(|_| \"artifacts\".to_string());").unwrap();
    writeln!(out,
        "        PathBuf::from(format!(\"{{root}}/{schema}/{table}/{{}}.html\", record.{pk_field}))"
    ).unwrap();
    writeln!(out, "    }}").unwrap();

    writeln!(out, "}}\n").unwrap();
}

// =============================================================================
// Utilitaires
// =============================================================================

fn write_section_header(out: &mut String, schema: &str, table: &str, pk: &PrimaryKey) {
    let pk_info = match pk {
        PrimaryKey::Single(col) => format!("PK={col}"),
        PrimaryKey::Composite   => "PK composite — Collector N/A".to_string(),
    };
    writeln!(out, "// {}", "=".repeat(60)).unwrap();
    writeln!(out, "// {schema}.{table} · {pk_info}").unwrap();
    writeln!(out, "// {}\n", "=".repeat(60)).unwrap();
}

fn to_pascal(s: &str) -> String {
    s.split('_').map(|w| {
        let mut c = w.chars();
        match c.next() {
            None    => String::new(),
            Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        }
    }).collect()
}

fn to_screaming(s: &str) -> String {
    s.to_uppercase()
}
