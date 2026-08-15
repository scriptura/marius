// crates/forge/db-forge/src/codegen/mapping.rs
//
//! # marius-db-forge - mapping
//!
//! Mapping SQL → Rust : types, layout, sentinels.

/// Colonne issue de pg_attribute.
/// `attnum` : numéro physique dans le heap — invariant de Symétrie Mécanique.
#[derive(Debug)]
pub struct Column {
    pub attnum: i16,
    pub name: String,
    /// Normalisé par format_type() : ex "character varying(255)", "timestamp with time zone".
    pub sql_type: String,
    pub is_notnull: bool,
    /// Phase 3 : valeur sentinel annotée via `COMMENT ON COLUMN ... IS 'marius:sentinel=<v>'`.
    /// None → sentinel par défaut du TypeMapping (default_sentinel).
    /// Ignoré si is_notnull (pas de wrapping Option en Row, sentinel sans objet).
    pub sentinel: Option<String>,
}

/// Clé primaire d'une table.
#[derive(Debug)]
pub enum PrimaryKey {
    /// PK sur une colonne unique → Collector applicable.
    Single(String),
    /// PK composée → Collector N/A (bit-vector sur domaine entier non applicable).
    Composite,
}

/// Informations de mapping pour un type SQL donné.
#[derive(Debug, Clone)]
pub struct TypeMapping {
    /// Type Rust dans la struct Row (sqlx-compatible, peut être Option<T>).
    pub row_type: &'static str,
    /// Type Rust dans la struct StorageRow (#[repr(C)]).
    pub store_type: &'static str,
    /// Expression de conversion Row → StorageRow.
    /// Placeholders : `{field}` → nom de colonne, `{sentinel}` → valeur sentinel.
    /// Utilisé uniquement pour les colonnes NULLABLE (is_notnull == false).
    pub from_expr: &'static str,
    /// Valeur sentinel par défaut si Column.sentinel == None.
    /// Substitué dans {sentinel} de from_expr. Domain-specific via Phase 3.
    pub default_sentinel: &'static str,
    /// true si fixed-length → présent dans StorageRow.
    /// false pour varlena (TEXT, VARCHAR, BYTEA…) et types Phase 2.
    pub is_fixed: bool,
    /// Taille en octets dans la struct repr(C).
    pub size_bytes: usize,
    /// Alignement naturel en octets (repr(C) aligne sur ce multiple).
    pub alignment: usize,
    /// Expression de cast à appliquer AU NIVEAU DU SELECT SQL, si le type
    /// n'est pas nativement décodable par sqlx (ex: pg_lsn — aucun Decode
    /// natif, cf. docs.rs/sqlx, module postgres::types). `{}` = placeholder
    /// remplacé par la référence de colonne (qualifiée schema.table.col ou
    /// non, selon l'appelant). `None` = colonne sélectionnée telle quelle
    /// (cas général, immense majorité des types).
    ///
    /// Contrat : si `Some`, codegen/projection.rs DOIT ajouter un alias
    /// `AS <nom_original>` — ce template ne le fait jamais lui-même (il ne
    /// connaît pas le nom de colonne, seulement sa référence qualifiée).
    /// Sans cet alias, sqlx::FromRow (dérivé, appariement par nom de
    /// colonne) ne retrouve plus le champ.
    pub select_cast: Option<&'static str>,
}

/// Retourne le mapping SQL → Rust pour un type donné.
///
/// La normalisation `split('(')` rend le match insensible à la précision :
/// "character varying(255)" → "character varying".
pub fn map_type(sql_type: &str) -> TypeMapping {
    let t = sql_type
        .split('(')
        .next()
        .unwrap_or(sql_type)
        .trim()
        .to_lowercase();

    match t.as_str() {
        "int8" | "bigint" => TypeMapping {
            row_type: "i64",
            store_type: "i64",
            // Sentinel -1 : CHECK (col >= 0) garantit que -1 = absent.
            // ATTENTION : domain-specific. Phase 3 lira le sentinel depuis pg_description.
            from_expr: "{field}.unwrap_or({sentinel})",
            default_sentinel: "-1",
            is_fixed: true,
            size_bytes: 8,
            alignment: 8,
            select_cast: None,
        },
        "int4" | "integer" | "int" | "serial" => TypeMapping {
            row_type: "i32",
            store_type: "i32",
            // Sentinel 0 : les IDs (GENERATED ALWAYS AS IDENTITY) commencent à 1.
            from_expr: "{field}.unwrap_or({sentinel})",
            default_sentinel: "0",
            is_fixed: true,
            size_bytes: 4,
            alignment: 4,
            select_cast: None,
        },
        "int2" | "smallint" => TypeMapping {
            row_type: "i16",
            store_type: "i16",
            from_expr: "{field}.unwrap_or({sentinel})",
            default_sentinel: "0",
            is_fixed: true,
            size_bytes: 2,
            alignment: 2,
            select_cast: None,
        },
        "bool" | "boolean" => TypeMapping {
            row_type: "bool",
            store_type: "bool",
            from_expr: "{field}.unwrap_or({sentinel})",
            default_sentinel: "false",
            is_fixed: true,
            size_bytes: 1,
            alignment: 1,
            select_cast: None,
        },
        "uuid" => TypeMapping {
            row_type: "[u8; 16]",
            store_type: "[u8; 16]",
            from_expr: "{field}.unwrap_or({sentinel})",
            default_sentinel: "[0u8; 16]",
            // [u8; 16] : alignement 1 (tableau d'octets, pas de contrainte supérieure).
            is_fixed: true,
            size_bytes: 16,
            alignment: 1,
            select_cast: None,
        },
        // TIMESTAMPTZ → i64 µs depuis l'epoch Unix.
        // Row : chrono::DateTime<Utc> (type riche, sqlx-compatible).
        // Store : i64 (POD, repr(C)-safe). Sentinel 0 = absent.
        "timestamptz" | "timestamp with time zone" => TypeMapping {
            row_type: "chrono::DateTime<chrono::Utc>",
            store_type: "i64",
            from_expr: "{field}.map(|dt| dt.timestamp_micros()).unwrap_or({sentinel})",
            default_sentinel: "0",
            is_fixed: true,
            size_bytes: 8,
            alignment: 8,
            select_cast: None,
        },
        "timestamp" | "timestamp without time zone" => TypeMapping {
            row_type: "chrono::NaiveDateTime",
            store_type: "i64",
            from_expr: "{field}.map(|dt| dt.and_utc().timestamp_micros()).unwrap_or({sentinel})",
            default_sentinel: "0",
            is_fixed: true,
            size_bytes: 8,
            alignment: 8,
            select_cast: None,
        },
        "date" => TypeMapping {
            row_type: "chrono::NaiveDate",
            store_type: "i32",
            // num_days_from_ce() : jours depuis 0001-01-01, toujours positif.
            from_expr: "{field}.map(|d| d.num_days_from_ce()).unwrap_or({sentinel})",
            default_sentinel: "0",
            is_fixed: true,
            size_bytes: 4,
            alignment: 4,
            select_cast: None,
        },
        "float4" | "real" => TypeMapping {
            row_type: "f32",
            store_type: "f32",
            from_expr: "{field}.unwrap_or({sentinel})",
            default_sentinel: "0.0",
            is_fixed: true,
            size_bytes: 4,
            alignment: 4,
            select_cast: None,
        },
        "float8" | "double precision" => TypeMapping {
            row_type: "f64",
            store_type: "f64",
            from_expr: "{field}.unwrap_or({sentinel})",
            default_sentinel: "0.0",
            is_fixed: true,
            size_bytes: 8,
            alignment: 8,
            select_cast: None,
        },
        // Varlena : exclus du StorageRow repr(C).
        // Présents dans Row (transport sqlx), portés par VarlenOwned (rendu).
        "text" | "varchar" | "character varying" | "jsonb" | "json" | "bytea" | "ltree" => {
            TypeMapping {
                row_type: "String",
                store_type: "/* VARLENA — exclu du StorageRow repr(C) */",
                from_expr: "/* VARLENA — non transféré dans StorageRow */",
                default_sentinel: "",
                is_fixed: false,
                size_bytes: 0,
                alignment: 0,
                select_cast: None,
            }
        }
        // pg_lsn : 8 octets, pointeur WAL (XLogRecPtr — entier 64 bits non
        // signé côté Postgres). Phase 2 (HANDOFF-js-deps-capacites-frontend-v2.md,
        // addendum content.core walsn).
        //
        // CORRECTIF (vérifié contre docs.rs/sqlx, module postgres::types,
        // version courante) : sqlx N'A AUCUN support natif pour pg_lsn —
        // pas de PgLsn, pas de Decode. Un premier essai de cette session
        // (row_type = "sqlx::postgres::types::PgLsn") était une affirmation
        // non vérifiée, fausse — cassait la compilation du schéma généré
        // (E0425, type introuvable). Corrigé ici avec une source vérifiée.
        //
        // Stratégie retenue : cast explicite AU NIVEAU DU SELECT SQL plutôt
        // qu'un Decode manuel — (col - '0/0'::pg_lsn)::int8 renvoie l'entier
        // 64 bits brut de la LSN (soustraction pg_lsn → numeric, cast sans
        // risque vers int8 : aucune LSN réelle n'approche i64::MAX octets de
        // WAL). row_type devient un simple "i64", déjà nativement décodé par
        // sqlx (BIGINT) — aucun wrapper à maintenir. select_cast porte ce
        // template ; codegen/projection.rs l'applique et ajoute l'alias
        // `AS <col>` requis par sqlx::FromRow.
        //
        // from_expr (cas NULLABLE générique) : "as u64" suffit, r.field est
        // déjà un i64 après le cast SQL — pas de `.0` à extraire (plus de
        // wrapper). Cas NOT NULL : traité spécifiquement dans from_impl.rs,
        // sur col.sql_type == "pg_lsn" (row_type == "i64" est ambigu avec
        // bigint/int8, qui ne doit lui jamais recevoir ce cast).
        //
        // Sentinel 0 = LSN nulle ('0/0'::pg_lsn, le DEFAULT du DDL lui-même :
        // 0 n'est donc jamais une valeur ambiguë avec une LSN réelle post-
        // écriture WAL, qui commence toujours après le segment 0).
        //
        // CONSÉQUENCE OPÉRATIONNELLE (vérifiée contre store_registry.rs,
        // marius_projection) : is_fixed=true agrandit {Schema}{Table}StorageRow
        // pour toute table portant une colonne pg_lsn (content.core et
        // commerce.product_core à ce jour, cf. grep -rn "pg_lsn" db/) — le
        // stride du store.bin correspondant change. PackfileReader::open
        // valide stride au cold_start et rejette (Err, jamais silencieux)
        // tout store.bin écrit avec l'ancien stride. Régénérer via
        // marius-dump après ce changement, avant tout redémarrage du
        // serveur — même nécessité que recharger les migrations SQL après
        // un ALTER TABLE, sur un artefact distinct.
        "pg_lsn" => TypeMapping {
            row_type: "i64",
            store_type: "u64",
            from_expr: "{field}.map(|v| v as u64).unwrap_or({sentinel})",
            default_sentinel: "0",
            is_fixed: true,
            size_bytes: 8,
            alignment: 8,
            select_cast: Some("({} - '0/0'::pg_lsn)::int8"),
        },

        "geometry" => TypeMapping {
            row_type: "Vec<u8>", // PostGIS renvoie du WKB (Well-Known Binary)
            store_type: "/* VARLENA — exclu du StorageRow repr(C) */",
            from_expr: "/* VARLENA — non transféré dans StorageRow */",
            default_sentinel: "",
            is_fixed: false,
            size_bytes: 0,
            alignment: 0,
            select_cast: None,
        },

        other => {
            println!("cargo:warning=DB-Forge : type SQL inconnu '{other}' — exclu");
            TypeMapping {
                row_type: "/* INCONNU */",
                store_type: "/* INCONNU */",
                from_expr: "/* INCONNU */",
                default_sentinel: "",
                is_fixed: false,
                size_bytes: 0,
                alignment: 0,
                select_cast: None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Vérifie is_fixed, size_bytes, alignment et default_sentinel pour un type SQL.
    fn check(sql: &str, is_fixed: bool, size: usize, align: usize, sentinel: &str) {
        let m = map_type(sql);
        assert_eq!(m.is_fixed, is_fixed, "is_fixed   pour {sql}");
        assert_eq!(m.size_bytes, size, "size_bytes pour {sql}");
        assert_eq!(m.alignment, align, "alignment  pour {sql}");
        assert_eq!(m.default_sentinel, sentinel, "default_sentinel pour {sql}");
    }

    // ── Entiers ──────────────────────────────────────────────────────────────

    #[test]
    fn map_int8() {
        check("int8", true, 8, 8, "-1");
    }
    #[test]
    fn map_bigint() {
        check("bigint", true, 8, 8, "-1");
    }
    #[test]
    fn map_int4() {
        check("int4", true, 4, 4, "0");
    }
    #[test]
    fn map_integer() {
        check("integer", true, 4, 4, "0");
    }
    #[test]
    fn map_serial() {
        check("serial", true, 4, 4, "0");
    }
    #[test]
    fn map_int2() {
        check("int2", true, 2, 2, "0");
    }
    #[test]
    fn map_smallint() {
        check("smallint", true, 2, 2, "0");
    }

    // ── Booléen ───────────────────────────────────────────────────────────────

    #[test]
    fn map_bool() {
        check("bool", true, 1, 1, "false");
    }
    #[test]
    fn map_boolean() {
        check("boolean", true, 1, 1, "false");
    }

    // ── UUID ─────────────────────────────────────────────────────────────────
    // alignment = 1 : [u8; 16] est un tableau d'octets, pas de contrainte sup.

    #[test]
    fn map_uuid() {
        check("uuid", true, 16, 1, "[0u8; 16]");
    }

    // ── Temporels ────────────────────────────────────────────────────────────

    #[test]
    fn map_timestamptz() {
        check("timestamptz", true, 8, 8, "0");
    }
    #[test]
    fn map_timestamp_with_tz() {
        check("timestamp with time zone", true, 8, 8, "0");
    }
    #[test]
    fn map_timestamp() {
        check("timestamp", true, 8, 8, "0");
    }
    #[test]
    fn map_timestamp_without_tz() {
        check("timestamp without time zone", true, 8, 8, "0");
    }
    #[test]
    fn map_date() {
        check("date", true, 4, 4, "0");
    }

    // pg_lsn : Phase 2 walsn — is_fixed=true depuis cette session, jamais
    // testé avant (Phase 1 l'excluait entièrement du StorageRow).
    // row_type="i64" (pas de wrapper : sqlx n'a aucun support natif pour
    // pg_lsn, vérifié contre docs.rs — d'où le cast SQL porté par
    // select_cast, appliqué en amont par codegen/projection.rs).
    #[test]
    fn map_pg_lsn() {
        check("pg_lsn", true, 8, 8, "0");
    }
    #[test]
    fn map_pg_lsn_row_type_is_plain_i64() {
        let m = map_type("pg_lsn");
        assert_eq!(m.row_type, "i64");
        assert_eq!(m.store_type, "u64");
    }
    #[test]
    fn map_pg_lsn_has_select_cast() {
        let m = map_type("pg_lsn");
        assert_eq!(m.select_cast, Some("({} - '0/0'::pg_lsn)::int8"));
    }
    #[test]
    fn map_bigint_has_no_select_cast() {
        // Non-régression : bigint/int8 partage row_type="i64" avec pg_lsn
        // mais ne doit JAMAIS recevoir le cast pg_lsn — select_cast est la
        // seule distinction fiable entre les deux à ce niveau.
        let m = map_type("bigint");
        assert_eq!(m.select_cast, None);
    }

    // ── Flottants ────────────────────────────────────────────────────────────

    #[test]
    fn map_float4() {
        check("float4", true, 4, 4, "0.0");
    }
    #[test]
    fn map_real() {
        check("real", true, 4, 4, "0.0");
    }
    #[test]
    fn map_float8() {
        check("float8", true, 8, 8, "0.0");
    }
    #[test]
    fn map_double_precision() {
        check("double precision", true, 8, 8, "0.0");
    }

    // ── Varlena (non fixed) ───────────────────────────────────────────────────

    #[test]
    fn map_text() {
        check("text", false, 0, 0, "");
    }
    #[test]
    fn map_varchar() {
        check("varchar", false, 0, 0, "");
    }
    #[test]
    fn map_character_varying() {
        check("character varying", false, 0, 0, "");
    }
    #[test]
    fn map_character_varying_precision() {
        check("character varying(255)", false, 0, 0, "");
    }
    #[test]
    fn map_jsonb() {
        check("jsonb", false, 0, 0, "");
    }
    #[test]
    fn map_json() {
        check("json", false, 0, 0, "");
    }
    #[test]
    fn map_bytea() {
        check("bytea", false, 0, 0, "");
    }

    // ── Précision ignorée (normalisation split '(') ───────────────────────────

    #[test]
    fn map_varchar_precision_strips_to_varlena() {
        // "character varying(255)" → même résultat que "character varying"
        let a = map_type("character varying(255)");
        let b = map_type("character varying");
        assert_eq!(a.is_fixed, b.is_fixed);
        assert_eq!(a.size_bytes, b.size_bytes);
    }
}
