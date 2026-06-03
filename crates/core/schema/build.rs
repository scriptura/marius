// =============================================================================
// DB-Forge — crates/core/schema/build.rs
// Projet Marius · ADR-002 / ADR-003 / Codegen.md / no_std-attitude-within-marius.md
//
// Génère dans $OUT_DIR/generated_schema.rs :
//   - Une struct {Name}Row          : désérialisation sqlx (Option<T> pour nullable)
//   - Une struct {Name}StorageRow   : stockage repr(C) (sentinel pour nullable fixe)
//   - Une struct {Name}VarlenOwned  : données varlena possédées (Option<String>, 'static)
//                                     Absente si pas de varlena (type VarlenOwned = ())
//   - From<{Name}Row> for {Name}StorageRow
//   - static {NAME}_COLLECTOR: Collector<MAX, WORDS>
//   - impl Projection stub (fetch_batch + render + artifact_path)
//
// ─── Philosophie Miroir Binaire ───────────────────────────────────────────────
//
//   PostgreSQL est la Source de Vérité Unique (SoT).
//   Ce script lit le catalogue système (pg_attribute, pg_constraint,
//   information_schema, pg_description, pg_stats) et produit des structs
//   Rust alignées bit-à-bit sur le DDL.
//
//   Toute divergence entre le layout PostgreSQL et la struct Rust est une
//   bombe à retardement : panique au runtime pour Phase 1, corruption
//   silencieuse pour Phase 2 (mmap / SHM). Les static_assertions émises
//   ici font exploser la bombe au moment de la compilation.
//
// ─── Taxonomie des structs générées (ADR-003) ────────────────────────────────
//
//   {Name}Row           : sqlx::FromRow, non-repr(C).
//                         Types natifs Rust + Option<T> pour nullable.
//                         Varlena : Option<String> (allocation sqlx).
//                         Durée de vie : éphémère (consommée dans fetch_batch).
//
//   {Name}StorageRow    : #[repr(C)], layout aligné sur le DDL PostgreSQL.
//                         Types fixed-length uniquement. Nullable → sentinel.
//                         Varlena exclues (fat pointer 16B briserait repr(C)).
//                         Durée de vie : persistante.
//
//   {Name}VarlenOwned   : struct possédée, Send + 'static.
//                         Champs varlena : Option<String>.
//                         Peut traverser tokio::spawn et rayon::par_iter.
//                         render() reconstruit les &str localement via as_deref().
//                         Absente si la table n'a pas de JOIN varlena → type = ().
//
// ─── Séquence de génération par table ────────────────────────────────────────
//
//   1. fetch_columns         : colonnes fixed + varlena depuis pg_attribute.
//   2. fetch_pk_column       : PK depuis information_schema (Single ou Composite).
//   3. fetch_max_id          : MAX(pk_col) → dimensionnement Collector.
//   4. fetch_varlena_cols    : colonnes varlena depuis la table jointe
//                               (max_len, is_pre_escaped, avg_width).
//   5. write_row_struct      : {Name}Row.
//   6. write_store_struct    : {Name}StorageRow + static_assertions layout.
//   7. write_varlen_owned_struct : {Name}VarlenOwned (ou commentaire si vide).
//   8. write_from_impl       : From<{Name}Row> for {Name}StorageRow.
//   9. write_collector       : Collector<MAX, WORDS> statique.
//  10. write_projection_stub : impl Projection
//                               (type VarlenOwned, fetch_batch tuple, render, artifact_path).
//
// Prérequis : DATABASE_URL pointe vers marius avec rôle marius_admin.
// Exécution : automatique via `cargo build`.
// =============================================================================

use std::env;
use std::fmt::Write as FmtWrite;
use std::fs;
use std::path::PathBuf;

// sqlx::Row : nécessaire pour .get::<T, _>(index) sur les résultats non-macro.
// L'API non-macro est obligatoire dans un build.rs : sqlx::query!() requiert
// une connexion active au moment de la compilation du crate principal,
// ce qui est incompatible avec l'exécution séquentielle du build script.
use sqlx::Row;

// Fragment-Forge : génération des corps render() + calcul capacité statique.
// VarlenField et generated_file_header() sont nouveaux par rapport au build.rs nominal.
use marius_fragment_forge::{
    FieldSpec, FieldKind, VarlenField,
    generate_render, generate_capacity_consts, generated_file_header,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Invalider le cache Cargo si DATABASE_URL ou ce fichier changent.
    // Sans ces directives, Cargo ne ré-exécuterait pas le build script
    // lors d'une modification du schéma PostgreSQL.
    println!("cargo:rerun-if-env-changed=DATABASE_URL");
    println!("cargo:rerun-if-changed=build.rs");

    let database_url = env::var("DATABASE_URL")
        .expect("DB-Forge : DATABASE_URL non définie.");

    let pool = sqlx::PgPool::connect(&database_url).await?;

    // -------------------------------------------------------------------------
    // Tables surveillées avec JOIN varlena optionnel.
    //
    // Format : (schema, table, Option<(join_schema, join_table, fk_col)>)
    //
    //   content.core          : PK=document_id, 3 nullable, RLS activé.
    //                           JOIN sur content.identity (champs texte auteur).
    //   commerce.product_core : PK=id (attnum=2, pas attnum=1), 2 nullable.
    //                           Pas de table jointe (varlena = []).
    //
    // Extension future : lire cette liste depuis meta.containment_intent
    // pour piloter la Forge entièrement depuis le DDL.
    // -------------------------------------------------------------------------
    let watched: &[(&str, &str, Option<(&str, &str, &str)>)] = &[
        ("content",  "core",         Some(("content", "identity", "document_id"))),
        ("commerce", "product_core", None),
    ];

    let out_dir  = PathBuf::from(env::var("OUT_DIR")?);
    let out_path = out_dir.join("generated_schema.rs");

    // En-tête généré par Fragment-Forge.
    // Contient : avertissement "NE PAS MODIFIER", import PathBuf,
    // et la fonction marius_html_escape() inline (zéro dépendance externe).
    let mut output = String::from(generated_file_header());

    for &(schema, table, varlena_join) in watched {
        let columns = fetch_columns(&pool, schema, table).await?;
        let pk      = fetch_pk_column(&pool, schema, table).await?;
        let max_id  = match &pk {
            PrimaryKey::Single(col) => Some(fetch_max_id(&pool, schema, table, col).await?),
            PrimaryKey::Composite   => None,
        };

        // Colonnes varlena depuis la table jointe (si définie).
        // Si varlena_join est None → varlena reste vide, aucun champ varlena généré.
        let varlena: Vec<VarlenField> = if let Some((vs, vt, _fk)) = varlena_join {
            fetch_varlena_cols(&pool, vs, vt).await?
        } else {
            vec![]
        };

        write_section_header(&mut output, schema, table, &pk);
        write_row_struct(&mut output, schema, table, &columns, &varlena);
        write_store_struct(&mut output, schema, table, &columns);
        write_varlen_owned_struct(&mut output, schema, table, &varlena);
        write_from_impl(&mut output, schema, table, &columns);

        if let (PrimaryKey::Single(col), Some(max)) = (&pk, max_id) {
            write_collector(&mut output, schema, table, col, max);
        }

        write_projection_stub(&mut output, schema, table, &columns, &pk, &varlena, varlena_join);
    }

    fs::write(&out_path, &output)?;
    eprintln!("DB-Forge : généré → {}", out_path.display());
    Ok(())
}

// =============================================================================
// Types internes
// =============================================================================

/// Colonne issue de pg_attribute.
#[derive(Debug)]
struct Column {
    /// Numéro d'attribut physique (ORDER BY attnum = invariant de Symétrie Mécanique).
    attnum:     i16,
    name:       String,
    /// Type SQL normalisé par format_type() (ex: "character varying(255)").
    sql_type:   String,
    /// true si la colonne a une contrainte NOT NULL dans le DDL.
    is_notnull: bool,
}

/// Clé primaire d'une table.
#[derive(Debug)]
enum PrimaryKey {
    /// PK sur une colonne unique → Collector applicable.
    Single(String),
    /// PK composée → Collector N/A (bit-vector sur domaine entier non applicable).
    Composite,
}

// =============================================================================
// I. Introspection — colonnes fixed-length
// =============================================================================

/// Colonnes dans l'ordre physique du heap (attnum ASC).
///
/// ORDER BY attnum est l'invariant de Symétrie Mécanique : il garantit que
/// l'ordre des champs dans {Name}StorageRow (#[repr(C)]) correspond exactement
/// à l'ordre des colonnes dans le heap tuple PostgreSQL.
/// Toute déviation produirait un désalignement silencieux en Phase 2 (mmap).
async fn fetch_columns(
    pool:   &sqlx::PgPool,
    schema: &str,
    table:  &str,
) -> Result<Vec<Column>, sqlx::Error> {
    // API non-macro : pas de vérification compile-time, DATABASE_URL non requise
    // pendant la phase de build du build script.
    // format_type() normalise les types avec précision (ex: "character varying(255)").
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
           AND a.attnum      > 0        -- exclut les colonnes système (attnum <= 0)
           AND NOT a.attisdropped       -- exclut les colonnes supprimées (ALTER TABLE DROP)
         ORDER BY a.attnum              -- invariant de Symétrie Mécanique",
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

// =============================================================================
// I-b. Introspection — clé primaire
// =============================================================================

/// Identifie la PK via information_schema (évite les subtilités de pg_catalog).
///
/// Retourne Single(col) si PK sur une colonne, Composite sinon.
/// Une PK Composite rend le Collector inapplicable : un bit-vector indexé
/// par ID entier suppose un domaine ID contigu sur une seule colonne i64.
async fn fetch_pk_column(
    pool:   &sqlx::PgPool,
    schema: &str,
    table:  &str,
) -> Result<PrimaryKey, sqlx::Error> {
    // fetch_all + match sur longueur remplace fetch_one pour éviter RowNotFound
    // quand la table n'a pas de PK (situation possible en DDL PostgreSQL).
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

// =============================================================================
// I-c. Introspection — dimensionnement du Collector
// =============================================================================

/// MAX(pk_col) + marge 20% + arrondi power-of-two en words (blocs de 64 bits).
///
/// ─── Calcul ──────────────────────────────────────────────────────────────────
///
///   max_id        = MAX(pk_col) observé en base (0 si table vide).
///   with_margin   = ceil(max_id × 1.20)  — absorbe la croissance courte-terme.
///   words_needed  = ceil(max_id / 64)    — nombre de mots u64 minimum.
///   words_aligned = next_power_of_two()  — alignement pour SIMD (TZCNT).
///   max_entity_id = words_aligned × 64  — borne du domaine IDs.
///
///   Le Collector<MAX, WORDS> généré aura MAX = max_entity_id, WORDS = words_aligned.
///
/// Note : la colonne PK n'est pas nécessairement nommée 'id'.
/// Ex : document_id sur content.core (attnum=1, pas 'id').
async fn fetch_max_id(
    pool:    &sqlx::PgPool,
    schema:  &str,
    table:   &str,
    pk_col:  &str,
) -> Result<usize, Box<dyn std::error::Error>> {
    // format! obligatoire : sqlx ne supporte pas l'interpolation d'identifiants SQL.
    // Risque injection nul : pk_col est issu de pg_constraint (catalogue système).
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
// I-d. Introspection — colonnes varlena de la table jointe
// =============================================================================

/// Récupère les colonnes varlena (varchar, text) d'une table jointe.
///
/// ─── Politique de max_len ────────────────────────────────────────────────────
///
///   VARCHAR(N)            : max_len = N  (atttypmod - 4 dans pg_attribute).
///   TEXT avec CHECK       : max_len = N  (extrait de pg_constraint CHECK).
///   TEXT sans contrainte  : exclu du listing render (cargo:warning émis).
///                           Si référencé dans le listing dense : panic!.
///                           Pour le rendu page complète : fallback 10 000
///                           avec cargo:warning.
///   max_escaped_len > 64 KB : panic! (dépassement du seuil de sécurité AOT).
///
/// ─── Politique is_pre_escaped ────────────────────────────────────────────────
///
///   Si pg_description contient 'marius:pre_escaped' pour la colonne,
///   is_pre_escaped = true → facteur d'escape = 1 (au lieu de 5).
///   Ce tag est posé via : COMMENT ON COLUMN schema.table.col IS 'marius:pre_escaped';
///   Typiquement : slugs, titres normalisés, tout champ garanti sans '&<>"\''.
///
/// ─── Validation AOT (avg_width) ──────────────────────────────────────────────
///
///   Si pg_stats.avg_width > 80% de max_len : cargo:warning.
///   Signal que DYNAMIC_CAP est sous-pression sur les données réelles.
///   Recommandation : augmenter max_len ou relancer ANALYZE si données récentes.
///   avg_width est disponible uniquement après ANALYZE (peut être NULL).
async fn fetch_varlena_cols(
    pool:   &sqlx::PgPool,
    schema: &str,
    table:  &str,
) -> Result<Vec<VarlenField>, Box<dyn std::error::Error>> {
    // Requête principale : colonnes varchar/bpchar/text, dans l'ordre attnum.
    // atttypmod pour VARCHAR(N) = N + 4 (overhead interne PostgreSQL).
    // atttypmod = -1 pour TEXT sans contrainte de longueur.
    let rows = sqlx::query(
        "SELECT
             a.attname::text,
             a.atttypmod::integer,
             COALESCE(d.description, '')::text  -- commentaire COMMENT ON COLUMN
         FROM pg_attribute  a
         JOIN pg_class      c ON c.oid = a.attrelid
         JOIN pg_namespace  n ON n.oid = c.relnamespace
         -- pg_description : commentaires posés par COMMENT ON COLUMN.
         -- objsubid = attnum pour cibler la colonne (pas la table entière).
         LEFT JOIN pg_description d
               ON  d.objoid    = a.attrelid
               AND d.objsubid  = a.attnum
         WHERE n.nspname = $1
           AND c.relname = $2
           AND a.attnum  > 0
           AND NOT a.attisdropped
           AND a.atttypid IN (
               SELECT oid FROM pg_type
               WHERE typname IN ('varchar', 'bpchar', 'text')
           )
         ORDER BY a.attnum",
    )
    .bind(schema)
    .bind(table)
    .fetch_all(pool)
    .await?;

    let mut fields = Vec::new();

    for row in rows {
        let name:        String = row.get(0);
        let typmod:      i32    = row.get(1);
        let description: String = row.get(2);

        // Détection du tag pre_escaped dans le commentaire SQL.
        // Comparaison exacte (trim()) pour éviter les faux positifs.
        let is_pre_escaped = description.trim() == "marius:pre_escaped";

        // ── Résolution de max_len ─────────────────────────────────────────────
        //
        // 1. VARCHAR(N) : atttypmod = N + 4. Si typmod > 4 → max_len = typmod - 4.
        // 2. TEXT/BPCHAR sans limite : typmod = -1.
        //    Tenter d'extraire une contrainte CHECK (length(col) <= N).
        // 3. Aucune contrainte : exclure du listing render.
        let max_len: usize = if typmod > 4 {
            // Cas 1 : VARCHAR(N).
            (typmod - 4) as usize
        } else {
            // Cas 2 : TEXT ou BPCHAR sans précision → chercher CHECK (length(col) <= N).
            let check_row = sqlx::query(
                // pg_constraint contient les CHECK en texte brut (consrc).
                // On cherche un pattern `length(col_name) <= N` ou `char_length(col_name) <= N`.
                "SELECT con.consrc::text
                 FROM   pg_constraint  con
                 JOIN   pg_class       cls ON cls.oid = con.conrelid
                 JOIN   pg_namespace   ns  ON ns.oid  = cls.relnamespace
                 WHERE  ns.nspname  = $1
                   AND  cls.relname = $2
                   AND  con.contype = 'c'  -- CHECK
                   AND  (con.consrc LIKE '%length(' || $3 || ')%'
                      OR con.consrc LIKE '%char_length(' || $3 || ')%')",
            )
            .bind(schema)
            .bind(table)
            .bind(&name)
            .fetch_optional(pool)
            .await?;

            if let Some(check_r) = check_row {
                let consrc: String = check_r.get(0);
                // Extraction de N depuis le texte de la contrainte.
                // Patterns visés : `(length(col) <= 500)` ou `(char_length(col) <= 500)`.
                // On cherche le premier entier après "<=".
                parse_check_length_limit(&consrc).unwrap_or_else(|| {
                    // Contrainte CHECK présente mais parsing échoue (forme inconnue).
                    // Fallback conservateur : 10 000 octets + warning.
                    println!(
                        "cargo:warning=DB-Forge [{schema}.{table}.{name}]: \
                         CHECK trouvé mais longueur non parsable : `{consrc}`. \
                         Fallback max_len=10000."
                    );
                    10_000
                })
            } else {
                // Cas 3 : TEXT sans contrainte ni CHECK → exclusion du listing render.
                println!(
                    "cargo:warning=DB-Forge [{schema}.{table}.{name}]: \
                     TEXT sans contrainte de longueur — exclu du listing render. \
                     Réserver au rendu page complète (allocation dynamique acceptable)."
                );
                continue; // passe à la colonne suivante
            }
        };

        // ── Validation AOT : seuil absolu 64 KB ──────────────────────────────
        //
        // Un champ dont max_escaped_len > 64 KB produirait un DYNAMIC_CAP trop élevé
        // pour être alloué sur la pile, et indiquerait une colonne inadaptée au listing.
        // Ce panic arrête le build avant que le problème n'atteigne le runtime.
        let escape_factor = if is_pre_escaped { 1 } else { VarlenField::HTML_ESCAPE_FACTOR };
        let max_escaped   = max_len * escape_factor;
        if max_escaped > 65_536 {
            panic!(
                "DB-Forge [{schema}.{table}.{name}]: \
                 max_escaped_len ({max_escaped}B) > 64 KB. \
                 Réduire la contrainte VARCHAR/CHECK ou exclure du listing render."
            );
        }

        // ── Validation AOT : pression sur DYNAMIC_CAP (avg_width) ────────────
        //
        // pg_stats.avg_width est la largeur moyenne observée après ANALYZE.
        // Si avg_width > 80% de max_len, DYNAMIC_CAP est sous-pression :
        // les données réelles frôlent le pire cas, ce qui indique soit une
        // mauvaise estimation de max_len, soit une croissance non anticipée.
        let avg_row = sqlx::query(
            "SELECT avg_width::integer FROM pg_stats
             WHERE schemaname = $1 AND tablename = $2 AND attname = $3",
        )
        .bind(schema)
        .bind(table)
        .bind(&name)
        .fetch_optional(pool)
        .await?;

        if let Some(r) = avg_row {
            let avg_width: i32 = r.get(0);
            if avg_width as usize > max_len * 8 / 10 {
                println!(
                    "cargo:warning=DB-Forge [{schema}.{table}.{name}]: \
                     avg_width observé ({avg_width}B) > 80% de max_len ({max_len}B). \
                     Pression sur DYNAMIC_CAP. Relancer ANALYZE si données récentes. \
                     Envisager d'augmenter la contrainte VARCHAR."
                );
            }
        }

        fields.push(VarlenField { name, max_len, is_pre_escaped });
    }

    Ok(fields)
}

/// Extrait la limite N depuis un texte de contrainte CHECK de la forme
/// `(length(col) <= N)` ou `(char_length(col) <= N)`.
///
/// Retourne None si le pattern n'est pas reconnu (contrainte CHECK de forme inconnue).
fn parse_check_length_limit(consrc: &str) -> Option<usize> {
    // Recherche de "<=" puis extraction du premier token numérique suivant.
    let after_le = consrc.split("<=").nth(1)?;
    after_le
        .trim()
        .trim_end_matches(')')
        .trim()
        .parse::<usize>()
        .ok()
}

// =============================================================================
// II. Mapping de types
// =============================================================================

/// Informations de mapping pour un type SQL.
#[derive(Debug, Clone)]
struct TypeMapping {
    /// Type Rust dans la struct Row (sqlx-compatible, peut être Option<T>).
    row_type:   &'static str,

    /// Type Rust dans la struct StorageRow (#[repr(C)]).
    store_type: &'static str,

    /// Expression de conversion Row → StorageRow pour ce champ.
    /// Utilise le placeholder `{field}` substitué par build.rs.
    from_expr:  &'static str,

    /// true si ce type est fixed-length et peut figurer dans repr(C).
    /// false pour les varlena (TEXT, VARCHAR, BYTEA…) et les types Phase 2.
    is_fixed:   bool,

    /// Taille en octets du type dans la struct repr(C).
    /// Utilisé pour le calcul du layout et les static_assertions.
    size_bytes: usize,

    /// Alignement du type en octets.
    /// repr(C) aligne chaque champ sur son alignement naturel et
    /// aligne la struct entière sur le max des alignements.
    /// Utilisé pour calculer la taille padded et émettre assert!(align_of == max_align).
    alignment:  usize,
}

fn map_type(sql_type: &str) -> TypeMapping {
    // Normalise "character varying(255)" → "character varying"
    // pour que le match soit insensible à la précision VARCHAR(N).
    let t = sql_type.split('(').next().unwrap_or(sql_type).trim().to_lowercase();

    match t.as_str() {
        "int8" | "bigint" => TypeMapping {
            row_type: "i64", store_type: "i64",
            // Sentinel -1 : CHECK (price_cents >= 0) garantit que -1 = absent.
            // Pour les timestamps INT8, sentinel 0 (epoch Unix invalide en pratique).
            // ATTENTION : le choix du sentinel est domain-specific.
            // La Forge full requiert une annotation COMMENT ON COLUMN par colonne nullable.
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
            // [u8; 16] : alignement 1 (tableau d'octets, pas de contrainte d'alignement).
            is_fixed: true, size_bytes: 16, alignment: 1,
        },
        // TIMESTAMPTZ → i64 µs depuis l'epoch Unix.
        // La Row utilise chrono::DateTime<Utc> (type riche), le Store utilise i64 (POD).
        // Sentinel 0 = absent (epoch 1970-01-01 00:00:00 UTC invalide en pratique).
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
            row_type:   "chrono::NaiveDate",
            store_type: "i32",
            // num_days_from_ce() : jours depuis 0001-01-01, toujours positif.
            from_expr:  "{field}.map(|d| d.num_days_from_ce()).unwrap_or(0)",
            is_fixed: true, size_bytes: 4, alignment: 4,
        },
        "float4" | "real" => TypeMapping {
            row_type: "f32", store_type: "f32",
            from_expr: "{field}.unwrap_or(0.0)",
            is_fixed: true, size_bytes: 4, alignment: 4,
        },
        "float8" | "double precision" => TypeMapping {
            row_type: "f64", store_type: "f64",
            from_expr: "{field}.unwrap_or(0.0)",
            is_fixed: true, size_bytes: 8, alignment: 8,
        },
        // Varlena : exclus du StorageRow repr(C).
        // Présents dans la Row (Option<String>) pour transport sqlx.
        // Le render() les accède via RenderPayload (&'a str, zéro copie).
        "text" | "varchar" | "character varying"
        | "jsonb" | "json" | "bytea" | "ltree" => TypeMapping {
            row_type:  "String",
            store_type: "/* VARLENA — exclu du StorageRow repr(C) */",
            from_expr:  "/* VARLENA — non transféré dans StorageRow */",
            is_fixed: false, size_bytes: 0, alignment: 0,
        },
        // pg_lsn : pointeur dans le Write-Ahead Log de PostgreSQL (8 octets).
        // Phase 1 : exclu des structs Rust (non utilisé pour le rendu HTML).
        // Phase 2 (SHM) : le StorageRow lira walsn via pointeur mmap → u64.
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

/// Génère {Name}Row : struct de désérialisation sqlx.
///
/// Non-repr(C) : l'ordre des champs suit l'ordre de la SELECT (attnum ASC pour fixed,
/// puis champs varlena de la table jointe en fin de liste).
///
/// Les champs varlena de la table jointe sont déclarés Option<String> :
/// le LEFT JOIN peut retourner NULL même si le champ est NOT NULL dans sa table source.
fn write_row_struct(
    out:     &mut String,
    schema:  &str,
    table:   &str,
    columns: &[Column],
    varlena: &[VarlenField],
) {
    let name = to_pascal(&format!("{schema}_{table}"));
    writeln!(out,
        "/// Struct de désérialisation sqlx pour {schema}.{table}.\n\
         /// Types natifs Rust + Option<T> pour nullable. NON repr(C).\n\
         /// Varlena JOIN : Option<String> (allocation sqlx, durée éphémère).\n\
         /// Transformer en StorageRow (From impl) + RenderPayload (as_deref) avant usage."
    ).unwrap();
    writeln!(out, "#[derive(Debug, sqlx::FromRow)]").unwrap();
    writeln!(out, "pub struct {name}Row {{").unwrap();

    for col in columns {
        let m = map_type(&col.sql_type);
        if m.is_fixed {
            if col.is_notnull {
                writeln!(out, "    pub {}: {},", col.name, m.row_type).unwrap();
            } else {
                writeln!(out, "    pub {}: Option<{}>,  // NULLABLE → sentinel dans StorageRow", col.name, m.row_type).unwrap();
            }
        } else {
            if m.row_type.starts_with("/*") {
                // Phase 2 ou type inconnu : commentaire uniquement, aucun champ généré.
                writeln!(out,
                    "    // EXCLU Phase 1 : {} ({}) — {}",
                    col.name, col.sql_type, m.row_type
                ).unwrap();
            } else if col.is_notnull {
                writeln!(out, "    pub {}: {},  // varlena table principale", col.name, m.row_type).unwrap();
            } else {
                writeln!(out, "    pub {}: Option<{}>,  // varlena NULLABLE table principale", col.name, m.row_type).unwrap();
            }
        }
    }

    // Champs varlena depuis la table jointe (LEFT JOIN) — toujours Option<String>.
    // Même si le champ est NOT NULL dans la table jointe, le LEFT JOIN retourne
    // NULL si aucune ligne ne correspond à la FK.
    for v in varlena {
        writeln!(out,
            "    pub {}: Option<String>,  // varlena JOIN VARCHAR({}) — → RenderPayload as_deref",
            v.name, v.max_len
        ).unwrap();
    }

    writeln!(out, "}}\n").unwrap();
}

// =============================================================================
// IV. Génération — Struct StorageRow (#[repr(C)])
// =============================================================================

/// Génère {Name}StorageRow : struct de stockage en mémoire contiguë.
///
/// ─── Invariant repr(C) ───────────────────────────────────────────────────────
///
///   Chaque champ est placé à l'offset dicté par son alignement naturel.
///   La struct est alignée sur le maximum des alignements de ses champs.
///   Le compilateur Rust honore exactement cet invariant pour repr(C).
///
/// ─── Varlena exclues ─────────────────────────────────────────────────────────
///
///   String / &str = fat pointer = 2 mots machine (16B sur x86_64).
///   Inclure un fat pointer dans repr(C) briserait la symétrie binaire avec
///   le heap tuple PostgreSQL. Les varlena sont portées par RenderPayload.
///
/// ─── static_assertions ───────────────────────────────────────────────────────
///
///   Deux assertions compilateur vérifient que la struct générée correspond
///   exactement au layout calculé par build.rs. Un ALTER TABLE non suivi d'une
///   reconstruction déclenche une erreur de compilation, pas une corruption.
fn write_store_struct(
    out:     &mut String,
    schema:  &str,
    table:   &str,
    columns: &[Column],
) {
    let name = to_pascal(&format!("{schema}_{table}"));

    writeln!(out,
        "/// Struct de stockage en mémoire contiguë pour {schema}.{table}.\n\
         /// #[repr(C)] : layout bit-à-bit aligné sur le heap tuple PostgreSQL.\n\
         /// Champs fixed-length uniquement. Nullable → sentinel (0 ou -1 selon type).\n\
         /// Varlena exclues : projetées depuis RenderPayload par Fragment-Forge.\n\
         ///\n\
         /// AVERTISSEMENT NULLABLE : le choix du sentinel est domain-specific.\n\
         /// La Forge full requiert une annotation COMMENT ON COLUMN par colonne nullable.\n\
         /// Pour ce prototype, les valeurs par défaut de map_type() s'appliquent."
    ).unwrap();
    writeln!(out, "#[repr(C)]").unwrap();
    writeln!(out, "#[derive(Debug, Clone, Copy, Default)]").unwrap();
    writeln!(out, "pub struct {name}StorageRow {{").unwrap();

    let mut layout_bytes = 0usize;
    let mut max_align    = 1usize;

    for col in columns {
        let m = map_type(&col.sql_type);
        if m.is_fixed {
            let null_marker = if col.is_notnull { "" } else { " [sentinel]" };
            // Padding d'alignement visuel pour lisibilité du layout.
            let pad = if col.name.len() < 20 {
                " ".repeat(20 - col.name.len().min(20))
            } else {
                String::new()
            };
            writeln!(out,
                "    pub {}: {},{}  // attnum={}, {}B{}",
                col.name, m.store_type, pad,
                col.attnum, m.size_bytes, null_marker,
            ).unwrap();
            layout_bytes += m.size_bytes;
            max_align     = max_align.max(m.alignment);
        } else {
            writeln!(out,
                "    // VARLENA exclu : {} ({}) → RenderPayload",
                col.name, col.sql_type
            ).unwrap();
        }
    }
    writeln!(out, "}}").unwrap();

    // Taille padded repr(C) : arrondie au multiple supérieur de max_align.
    // C'est la taille effective retournée par std::mem::size_of::<Struct>().
    let padded_size = layout_bytes.div_ceil(max_align.max(1)) * max_align.max(1);

    // Commentaire de layout pour diagnostic (visible dans le fichier généré).
    writeln!(out, "// Layout fixed-length : {layout_bytes}B données → {padded_size}B padded (align={max_align}B)").unwrap();
    // Taille du header heap PostgreSQL : MAXALIGN(23 + ceil(N/8)) pour N colonnes.
    // Référence : src/include/access/htup_details.h (HeapTupleHeaderData).
    writeln!(out, "// + {}B header heap PostgreSQL (MAXALIGN(23 + ceil({}/8)))",
        { let n = columns.len(); (23 + n.div_ceil(8)).div_ceil(8) * 8 },
        columns.len()
    ).unwrap();
    writeln!(out).unwrap();

    // static_assertions : vérifient la symétrie binaire à la compilation.
    // Un ALTER TABLE non suivi d'une reconstruction déclenche ici, pas au runtime.
    writeln!(out,
        "const _: () = assert!(\n    \
         std::mem::size_of::<{name}StorageRow>() == {padded_size},\n    \
         \"DB-Forge [{schema}.{table}]: size_of diverge du DDL — reconstruire après ALTER TABLE\",\n\
         );"
    ).unwrap();
    writeln!(out,
        "const _: () = assert!(\n    \
         std::mem::align_of::<{name}StorageRow>() == {max_align},\n    \
         \"DB-Forge [{schema}.{table}]: align_of diverge du DDL — vérifier les types colonnes\",\n\
         );"
    ).unwrap();
    writeln!(out).unwrap();
}

// =============================================================================
// IV-b. Génération — Struct VarlenOwned (données varlena possédées)
// =============================================================================

/// Génère {Name}VarlenOwned : struct possédée portant les données varlena.
///
/// ─── Rôle ────────────────────────────────────────────────────────────────────
///
///   Porte les Option<String> issues du fetch SQLx (ou du buffer de page Phase 2).
///   'static + Send : peut traverser tokio::spawn et rayon::par_iter sans contrainte
///   de lifetime. C'est la propriété qui la distingue de RenderPayload<'a>.
///
///   Le Dispatcher reçoit Vec<(StorageRow, VarlenOwned)> depuis fetch_batch,
///   distribue les tuples sur les threads Rayon, et dans chaque thread :
///     let name_ref = varlena.name.as_deref();  // &str local, lifetime inféré
///   Ces &str sont construits par le corps de render() généré par Fragment-Forge,
///   pas par le Dispatcher — ils n'ont pas besoin d'exister hors de render().
///
/// ─── Si aucun varlena ────────────────────────────────────────────────────────
///
///   type VarlenOwned = () dans le trait.
///   Aucune struct n'est générée. Le compilateur élimine le paramètre à zéro coût.
///   render() reçoit &() et ignore le paramètre varlena.
///
/// ─── Comparaison avec RenderPayload<'a> (session précédente) ─────────────────
///
///   RenderPayload<'a> (Option<&'a str>) n'est plus un type émis dans le fichier
///   généré. Les &str sont reconstruits localement dans render() via as_deref().
///   Le lifetime 'a est inféré par le compilateur depuis le paramètre varlena,
///   sans apparaître dans aucune interface publique.
fn write_varlen_owned_struct(
    out:     &mut String,
    schema:  &str,
    table:   &str,
    varlena: &[VarlenField],
) {
    // Si aucun varlena : type VarlenOwned = () dans le trait → pas de struct à générer.
    // Le commentaire ci-dessous documente l'absence intentionnelle.
    if varlena.is_empty() {
        writeln!(out,
            "// {schema}.{table} : aucun champ varlena — type VarlenOwned = () dans le trait.\n"
        ).unwrap();
        return;
    }

    let name = to_pascal(&format!("{schema}_{table}"));

    writeln!(out,
        "/// Données varlena possédées pour {schema}.{table}.\n\
         /// Send + 'static : traversée tokio::spawn et rayon::par_iter sans contrainte.\n\
         /// Produite par fetch_batch() depuis les Row SQLx (conversion directe).\n\
         /// render() reconstruit les &str localement via as_deref() — zéro copie."
    ).unwrap();
    writeln!(out, "#[derive(Debug, Default)]").unwrap();
    writeln!(out, "pub struct {name}VarlenOwned {{").unwrap();

    for v in varlena {
        writeln!(out,
            "    /// VARCHAR({}) — {} × {}.",
            v.max_len,
            if v.is_pre_escaped { "pré-échappé, facteur" } else { "escape HTML, facteur" },
            if v.is_pre_escaped { 1 } else { VarlenField::HTML_ESCAPE_FACTOR },
        ).unwrap();
        writeln!(out, "    pub {}: Option<String>,", v.name).unwrap();
    }

    writeln!(out, "}}\n").unwrap();
}

// =============================================================================
// V. Génération — From<Row> for StorageRow
// =============================================================================

/// Génère From<{Name}Row> for {Name}StorageRow.
///
/// Transfert des champs fixed-length uniquement.
/// Les champs varlena (Option<String>) restent dans la Row jusqu'à la construction
/// du RenderPayload par le Dispatcher (as_deref()).
///
/// Conversions :
///   NOT NULL  → valeur directe (ou timestamp_micros() pour les types chrono).
///   NULLABLE  → m.from_expr  : unwrap_or(sentinel) ou map(…).unwrap_or(0).
fn write_from_impl(
    out:     &mut String,
    schema:  &str,
    table:   &str,
    columns: &[Column],
) {
    let name = to_pascal(&format!("{schema}_{table}"));
    writeln!(out, "impl From<{name}Row> for {name}StorageRow {{").unwrap();
    writeln!(out, "    fn from(r: {name}Row) -> Self {{").unwrap();
    writeln!(out, "        Self {{").unwrap();

    for col in columns {
        let m = map_type(&col.sql_type);
        if !m.is_fixed { continue; }

        let expr = if col.is_notnull {
            // NOT NULL : conversion directe (Row et StorageRow ont le même type de base)
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
// VI. Génération — Collector<MAX, WORDS> statique
// =============================================================================

/// Génère le Collector dimensionné pour la table.
///
/// MAX_ENTITY_ID : borne du domaine IDs (calculée par fetch_max_id).
/// WORDS         : nombre de mots u64 = MAX_ENTITY_ID / 64.
///
/// La relation WORDS = MAX / 64 est imposée par la Forge car generic_const_exprs
/// est instable en Rust stable. Le type system ne peut pas l'exprimer ;
/// la Forge garantit la cohérence à la génération.
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
    writeln!(out,
        "pub static {screaming}_COLLECTOR: crate::collector::Collector<MAX_{screaming}_ID, {screaming}_WORDS> ="
    ).unwrap();
    writeln!(out, "    crate::collector::Collector::new_zeroed();\n").unwrap();
}

// =============================================================================
// VII. Génération — impl Projection stub
// =============================================================================

/// Génère le stub impl Projection pour la table (conforme au trait ADR-003).
///
/// ─── type Record ─────────────────────────────────────────────────────────────
///
///   Record = {Name}StorageRow (champs fixed uniquement, repr(C)).
///
/// ─── type VarlenOwned ────────────────────────────────────────────────────────
///
///   Tables avec varlena  : {Name}VarlenOwned (Option<String> possédées, 'static).
///   Tables sans varlena  : () — coût zéro, éliminé par le compilateur.
///
/// ─── fetch_batch ─────────────────────────────────────────────────────────────
///
///   Retourne Vec<(StorageRow, VarlenOwned)> possédé.
///   La conversion depuis la Row SQLx se fait dans le corps généré :
///     StorageRow  ← From<{Name}Row>
///     VarlenOwned ← déplacement des Option<String> depuis la Row
///
/// ─── render ──────────────────────────────────────────────────────────────────
///
///   Signature : fn render(&StorageRow, &VarlenOwned, &mut String)
///   Corps généré par Fragment-Forge :
///     - as_deref() local pour reconstruire les &str depuis VarlenOwned
///     - push_str (statique) + write_fmt (fixed) + marius_html_escape (varlena)
fn write_projection_stub(
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

    // Nom du type VarlenOwned selon présence de varlena.
    // () pour les tables sans JOIN varlena : éliminé à la compilation.
    let varlen_owned_type = if varlena.is_empty() {
        "()".to_string()
    } else {
        format!("{name}VarlenOwned")
    };

    // ── SELECT des colonnes fixed-length ─────────────────────────────────────
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

    // ── Construction de la requête fetch_batch ────────────────────────────────
    // Si une table jointe est définie, on ajoute les colonnes varlena au SELECT
    // et on construit la clause LEFT JOIN.
    let (select, from_clause) = if let Some((vs, vt, fk)) = varlena_join {
        let varlena_cols: Vec<String> = varlena.iter()
            .map(|v| format!("{vt}.{}", v.name))
            .collect();
        let all_cols: Vec<String> = fixed_cols.iter().map(|c| c.to_string())
            .chain(varlena_cols)
            .collect();
        let select = all_cols.join(", ");
        let from   = format!(
            "{schema}.{table} LEFT JOIN {vs}.{vt} ON {schema}.{table}.{fk} = {vs}.{vt}.{fk}"
        );
        (select, from)
    } else {
        (fixed_cols.join(", "), format!("{schema}.{table}"))
    };

    let where_clause = match pk {
        PrimaryKey::Single(col) => {
            format!("WHERE {schema}.{table}.{col} = ANY($1) ORDER BY {schema}.{table}.{col} ASC")
        }
        PrimaryKey::Composite => "WHERE 1=1 /* PK composite: adapter */".to_string(),
    };

    // ── Fragment-Forge : calcul des capacités ─────────────────────────────────
    // Calculé AVANT l'émission pour éviter une dépendance sur l'ordre de génération.
    // Les constantes de capacité sont émises au niveau MODULE, pas dans le bloc impl.
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

    let pk_field_name = match pk {
        PrimaryKey::Single(col) => col.as_str(),
        PrimaryKey::Composite   => field_specs.first().map(|f| f.name.as_str()).unwrap_or("id"),
    };

    let (static_cap, dynamic_cap, render_body) = generate_render(
        schema, table, &name,
        &field_specs,
        // PK réelle issue de pg_constraint — pas le premier champ par attnum.
        // Ex: document_id est en attnum=1 pour content.core,
        //     id est en attnum=2 pour commerce.product_core.
        pk_field_name,
        varlena,
    );
    let cap_consts = generate_capacity_consts(&screaming, static_cap, dynamic_cap);

    // ── Émission ─────────────────────────────────────────────────────────────
    writeln!(out, "pub struct {proj_name};").unwrap();
    writeln!(out).unwrap();

    // Constantes de capacité au niveau module (pas dans impl).
    writeln!(out, "{cap_consts}").unwrap();

    writeln!(out, "// Pool requis : marius_user (SELECT non révoqué sur {schema}.{table})").unwrap();
    writeln!(out, "// RLS         : voir 09_rls/01_policies.sql").unwrap();
    writeln!(out, "impl crate::projection::Projection for {proj_name} {{").unwrap();

    // type Record : StorageRow, repr(C), fixed-length uniquement.
    writeln!(out, "    type Record = {name}StorageRow;").unwrap();

    // type VarlenOwned : struct possédée ou () si pas de varlena.
    // Send + 'static garantis dans les deux cas : () est trivial, VarlenOwned dérive Default.
    writeln!(out, "    type VarlenOwned = {varlen_owned_type};").unwrap();
    writeln!(out).unwrap();

    // fetch_batch : retourne Vec<(StorageRow, VarlenOwned)>.
    // La Row SQLx sert d'intermédiaire de transport ; elle est consommée ici.
    // StorageRow ← From<Row>, VarlenOwned ← déplacement des Option<String>.
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

        // Conversion Row → (StorageRow, VarlenOwned).
        if varlena.is_empty() {
            // Pas de varlena : VarlenOwned = (), From<Row> consomme r entièrement.
            writeln!(out,
                "        Ok(rows.into_iter().map(|r| ({name}StorageRow::from(r), ())).collect())"
            ).unwrap();
        } else {
            // Avec varlena : From<Row> consomme r par valeur ET les Option<String>
            // doivent être déplacées hors de r pour VarlenOwned.
            // Ces deux opérations ne peuvent pas être séquentielles sur la même valeur r :
            // déplacer un champ Option<String> invalide le move ultérieur de r entier
            // (E0382 — partial move).
            //
            // Solution : déstructuration complète de r en un seul pattern let.
            // Tous les champs sont nommés simultanément ; aucun move partiel n'a lieu.
            // StorageRow et VarlenOwned sont construits depuis les bindings nommés,
            // indépendamment l'un de l'autre.
            //
            // Conséquence : From<{Name}Row> for {Name}StorageRow n'est PAS appelé ici.
            // La logique de conversion fixed (sentinels, timestamp_micros) est
            // reproduite inline depuis map_type(). Toute modification de map_type()
            // doit être répercutée dans cette branche.
            writeln!(out, "        Ok(rows.into_iter().map(|r| {{").unwrap();

            // Déstructuration complète de r.
            // Champs fixed : liés par nom, utilisés pour StorageRow.
            // Champs varlena JOIN (dans `varlena`) : liés par nom, utilisés pour VarlenOwned.
            // Champs Phase2 / inconnus (row_type commence par "/*") : ignorés via `..`.
            // La clause `..` absorbe les champs non nommés restants sans les déplacer.
            writeln!(out, "            let {name}Row {{").unwrap();
            // Champs fixed depuis columns.
            for col in columns {
                let m = map_type(&col.sql_type);
                if m.is_fixed {
                    writeln!(out, "                {},", col.name).unwrap();
                }
            }
            // Champs varlena JOIN depuis varlena (absents de columns, issus du JOIN).
            for v in varlena {
                writeln!(out, "                {},", v.name).unwrap();
            }
            // `..` : absorbe les champs varlena de la table principale (non-JOIN)
            // et les champs Phase2/inconnus sans les nommer ni les déplacer.
            writeln!(out, "                ..").unwrap();
            writeln!(out, "            }} = r;").unwrap();

            // Construction de VarlenOwned depuis les bindings varlena nommés.
            writeln!(out, "            let owned = {name}VarlenOwned {{").unwrap();
            for v in varlena {
                writeln!(out, "                {},", v.name).unwrap();
            }
            writeln!(out, "            }};").unwrap();

            // Construction de StorageRow depuis les bindings fixed nommés.
            // Reproduit la logique de From<Row> : sentinels pour nullable,
            // conversions chrono pour les timestamps.
            // Utilise les bindings locaux (pas r.champ — r est déjà déstructuré).
            writeln!(out, "            let storage = {name}StorageRow {{").unwrap();
            for col in columns {
                let m = map_type(&col.sql_type);
                if !m.is_fixed { continue; }

                let expr = if col.is_notnull {
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
                    // Remplace {field} par le binding local (pas r.champ).
                    m.from_expr.replace("{field}", &col.name)
                };

                writeln!(out, "                {}: {},", col.name, expr).unwrap();
            }
            writeln!(out, "            }};").unwrap();

            writeln!(out, "            (storage, owned)").unwrap();
            writeln!(out, "        }}).collect())").unwrap();
        }
    }

    writeln!(out, "    }}").unwrap();
    writeln!(out).unwrap();

    // render() : signature conforme au trait ADR-003.
    // varlena : &Self::VarlenOwned — Fragment-Forge reconstruit les &str localement.
    // Pour () (pas de varlena) : le paramètre _varlena est ignoré sans overhead.
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

    // artifact_path : chemin déterministe basé sur la PK (lecture depuis StorageRow).
    writeln!(out, "    fn artifact_path(record: &Self::Record) -> PathBuf {{").unwrap();
    writeln!(out, "        let root = std::env::var(\"MARIUS_ARTIFACTS_DIR\")").unwrap();
    writeln!(out, "            .unwrap_or_else(|_| \"artifacts\".to_string());").unwrap();
    writeln!(out,
        "        PathBuf::from(format!(\"{{root}}/{schema}/{table}/{{}}.html\", record.{pk_field_name}))"
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

/// Convertit "content_core" → "ContentCore".
fn to_pascal(s: &str) -> String {
    s.split('_').map(|w| {
        let mut c = w.chars();
        match c.next() {
            None    => String::new(),
            Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        }
    }).collect()
}

/// Convertit "content_core" → "CONTENT_CORE".
fn to_screaming(s: &str) -> String {
    s.to_uppercase()
}
