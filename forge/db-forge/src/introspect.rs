// =============================================================================
// marius-db-forge · introspect.rs
// Requêtes SQLx d'introspection pg_catalog / information_schema / pg_stats.
// Extrait de crates/core/schema/build.rs (Phase 0 — extraction isofonctionnelle).
// =============================================================================

use sqlx::Row as _;

use crate::mapping::{Column, PrimaryKey};
use marius_fragment_forge::VarlenField;

// =============================================================================
// I. Colonnes fixed-length (pg_attribute)
// =============================================================================

/// Colonnes dans l'ordre physique du heap (attnum ASC).
///
/// ORDER BY attnum est l'invariant de Symétrie Mécanique : il garantit que
/// l'ordre des champs dans {Name}StorageRow (#[repr(C)]) correspond exactement
/// à l'ordre des colonnes dans le heap tuple PostgreSQL.
pub async fn fetch_columns(
    pool:   &sqlx::PgPool,
    schema: &str,
    table:  &str,
) -> Result<Vec<Column>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT
             a.attnum::smallint,
             a.attname::text,
             format_type(a.atttypid, a.atttypmod),
             a.attnotnull,
             COALESCE(col_description(c.oid, a.attnum), '')::text
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
        sentinel:   parse_sentinel(&r.get::<String, _>(4)),
    }).collect())
}

// =============================================================================
// II. Clé primaire (information_schema)
// =============================================================================

/// Identifie la PK via information_schema.
///
/// Retourne Single(col) si PK sur une colonne unique, Composite sinon.
/// Une PK Composite rend le Collector inapplicable.
pub async fn fetch_pk_column(
    pool:   &sqlx::PgPool,
    schema: &str,
    table:  &str,
) -> Result<PrimaryKey, sqlx::Error> {
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
// III. Dimensionnement du Collector (MAX(pk_col))
// =============================================================================

/// MAX(pk_col) + marge 20% + arrondi power-of-two en words (blocs de 64 bits).
///
///   max_id        = MAX(pk_col) observé (0 si table vide).
///   with_margin   = ceil(max_id × 1.20).
///   words_needed  = ceil(with_margin / 64).
///   words_aligned = next_power_of_two().
///   max_entity_id = words_aligned × 64.
pub async fn fetch_max_id(
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
// IV. Colonnes varlena de la table jointe (pg_attribute + pg_stats + pg_description)
// =============================================================================

/// Colonnes varlena (varchar, bpchar, text) d'une table.
///
/// ─── Politique max_len ───────────────────────────────────────────────────────
///
///   VARCHAR(N)           : max_len = atttypmod - 4.
///   TEXT avec CHECK      : max_len extrait de pg_constraint (consrc).
///   TEXT sans contrainte : exclu (cargo:warning). Fallback 10 000 si force.
///   max_escaped_len > 64 KB : panic! (seuil AOT).
///
/// ─── Politique is_pre_escaped ─────────────────────────────────────────────────
///
///   Si COMMENT ON COLUMN ... IS 'marius:pre_escaped' → facteur escape = 1.
///   Sinon : facteur = VarlenField::HTML_ESCAPE_FACTOR (5).
pub async fn fetch_varlena_cols(
    pool:   &sqlx::PgPool,
    schema: &str,
    table:  &str,
) -> Result<Vec<VarlenField>, Box<dyn std::error::Error>> {
    let rows = sqlx::query(
        "SELECT
             a.attname::text,
             a.atttypmod::integer,
             COALESCE(d.description, '')::text
         FROM pg_attribute  a
         JOIN pg_class      c ON c.oid = a.attrelid
         JOIN pg_namespace  n ON n.oid = c.relnamespace
         LEFT JOIN pg_description d
               ON  d.objoid   = a.attrelid
               AND d.objsubid = a.attnum
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

        let is_pre_escaped = description.trim() == "marius:pre_escaped";

        // ── Résolution de max_len ─────────────────────────────────────────────
        let max_len: usize = if typmod > 4 {
            // Cas 1 : VARCHAR(N) → atttypmod = N + 4.
            (typmod - 4) as usize
        } else {
            // Cas 2 : TEXT/BPCHAR sans précision → chercher CHECK (length(col) <= N).
            let check_row = sqlx::query(
                "SELECT con.consrc::text
                 FROM   pg_constraint  con
                 JOIN   pg_class       cls ON cls.oid = con.conrelid
                 JOIN   pg_namespace   ns  ON ns.oid  = cls.relnamespace
                 WHERE  ns.nspname  = $1
                   AND  cls.relname = $2
                   AND  con.contype = 'c'
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
                parse_check_length_limit(&consrc).unwrap_or_else(|| {
                    println!(
                        "cargo:warning=DB-Forge [{schema}.{table}.{name}]: \
                         CHECK trouvé mais longueur non parsable : `{consrc}`. \
                         Fallback max_len=10000."
                    );
                    10_000
                })
            } else {
                // Cas 3 : TEXT sans contrainte → exclu du listing render.
                println!(
                    "cargo:warning=DB-Forge [{schema}.{table}.{name}]: \
                     TEXT sans contrainte de longueur — exclu du listing render."
                );
                continue;
            }
        };

        // ── Validation AOT : seuil absolu 64 KB ──────────────────────────────
        let escape_factor = if is_pre_escaped { 1 } else { VarlenField::HTML_ESCAPE_FACTOR };
        let max_escaped   = max_len * escape_factor;
        if max_escaped > 65_536 {
            panic!(
                "DB-Forge [{schema}.{table}.{name}]: \
                 max_escaped_len ({max_escaped}B) > 64 KB. \
                 Réduire la contrainte VARCHAR/CHECK ou exclure du listing render."
            );
        }

        // ── Validation AOT : pression avg_width → DYNAMIC_CAP ────────────────
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
                     Pression sur DYNAMIC_CAP."
                );
            }
        }

        fields.push(VarlenField { name, max_len, is_pre_escaped });
    }

    Ok(fields)
}

// =============================================================================
// Utilitaire : extraction de N depuis CHECK (length(col) <= N)
// =============================================================================

/// Extrait la limite N depuis un texte de contrainte CHECK.
///
/// Patterns visés : `(length(col) <= N)` ou `(char_length(col) <= N)`.
/// Retourne None si le pattern n'est pas reconnu.
pub(crate) fn parse_check_length_limit(consrc: &str) -> Option<usize> {
    let after_le = consrc.split("<=").nth(1)?;
    after_le
        .trim()
        .trim_end_matches(')')
        .trim()
        .parse::<usize>()
        .ok()
}

// =============================================================================
// Utilitaire : extraction du sentinel depuis pg_description
// =============================================================================

/// Extrait la valeur sentinel depuis le commentaire de colonne PostgreSQL.
///
/// Convention Phase 3 :
///   `COMMENT ON COLUMN schema.table.col IS 'marius:sentinel=<valeur>';`
///
/// Supporte les commentaires composés (séparés par ';') :
///   `'Description lisible ; marius:sentinel=-1'` → Some("-1")
///
/// Retourne None si la clé `marius:sentinel=` est absente.
pub(crate) fn parse_sentinel(description: &str) -> Option<String> {
    const KEY: &str = "marius:sentinel=";
    description
        .split(';')
        .map(str::trim)
        .find(|s| s.starts_with(KEY))
        .map(|s| s[KEY.len()..].trim().to_string())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_check_simple() {
        assert_eq!(parse_check_length_limit("(length(label) <= 255)"), Some(255));
    }

    #[test]
    fn parse_check_char_length() {
        assert_eq!(parse_check_length_limit("(char_length(bio) <= 1000)"), Some(1000));
    }

    #[test]
    fn parse_check_unknown() {
        assert_eq!(parse_check_length_limit("(label IS NOT NULL)"), None);
    }

    // ── Tests parse_sentinel ─────────────────────────────────────────────────

    #[test]
    fn sentinel_simple() {
        assert_eq!(parse_sentinel("marius:sentinel=0"), Some("0".to_string()));
    }

    #[test]
    fn sentinel_negative() {
        assert_eq!(parse_sentinel("marius:sentinel=-1"), Some("-1".to_string()));
    }

    #[test]
    fn sentinel_composite_comment() {
        assert_eq!(
            parse_sentinel("Colonne de liaison ; marius:sentinel=0"),
            Some("0".to_string())
        );
    }

    #[test]
    fn sentinel_absent() {
        assert_eq!(parse_sentinel("Description sans annotation"), None);
    }

    #[test]
    fn sentinel_empty_comment() {
        assert_eq!(parse_sentinel(""), None);
    }

    #[test]
    fn sentinel_key_without_value() {
        // Clé présente mais valeur vide → None (filtre sur is_empty)
        assert_eq!(parse_sentinel("marius:sentinel="), None);
    }
}

// =============================================================================
// Tests d'intégration — Phase 4.4
// Marqués #[ignore] : requièrent DATABASE_URL + schéma marius opérationnel.
// Exécution : cargo test -p marius-db-forge -- --ignored
// =============================================================================

#[cfg(test)]
mod integration {
    use super::*;
    use crate::registry::fetch_component_list;
    use crate::validate::validate_layout;

    async fn connect() -> sqlx::PgPool {
        let url = std::env::var("DATABASE_URL")
            .expect("DATABASE_URL requis pour les tests d'intégration");
        sqlx::PgPool::connect(&url).await
            .expect("Connexion PgPool échouée")
    }

    /// fetch_component_list() retourne au moins 2 composants.
    /// Invariant : meta.containment_intent contient au minimum
    /// content.core et commerce.product_core.
    #[tokio::test]
    #[ignore]
    async fn fetch_component_list_returns_at_least_two() {
        let pool  = connect().await;
        let comps = fetch_component_list(&pool).await
            .expect("fetch_component_list échoué");
        assert!(
            comps.len() >= 2,
            "Moins de 2 composants — meta.containment_intent vide ou incomplet : {:?}",
            comps.iter().map(|c| format!("{}.{}", c.schema, c.table)).collect::<Vec<_>>()
        );
    }

    /// fetch_columns() pour content.core retourne les colonnes triées attnum ASC.
    /// Invariant de Symétrie Mécanique : l'ordre attnum == l'ordre StorageRow.
    #[tokio::test]
    #[ignore]
    async fn fetch_columns_content_core_ordered_by_attnum() {
        let pool = connect().await;
        let cols = fetch_columns(&pool, "content", "core").await
            .expect("fetch_columns échoué");
        assert!(
            !cols.is_empty(),
            "Aucune colonne pour content.core — table absente ou vide"
        );
        for w in cols.windows(2) {
            assert!(
                w[0].attnum < w[1].attnum,
                "Colonnes non triées par attnum : {} ({}) >= {} ({})",
                w[0].name, w[0].attnum,
                w[1].name, w[1].attnum,
            );
        }
    }

    /// validate_layout() passe pour tous les composants enregistrés
    /// avec intent_density != 0.
    /// Régression directe de Phase 2 : aucune divergence tolérée post-correction.
    #[tokio::test]
    #[ignore]
    async fn validate_layout_passes_for_all_registered_components() {
        let pool  = connect().await;
        let comps = fetch_component_list(&pool).await
            .expect("fetch_component_list échoué");

        for comp in &comps {
            if comp.intent_density == 0 { continue; }

            let cols = fetch_columns(&pool, &comp.schema, &comp.table).await
                .expect(&format!("fetch_columns échoué pour {}.{}", comp.schema, comp.table));

            validate_layout(&cols, comp.intent_density)
                .unwrap_or_else(|msg| panic!(
                    "{}.{} : {}",
                    comp.schema, comp.table, msg
                ));
        }
    }
}
