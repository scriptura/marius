// =============================================================================
// marius-db-forge · mapping.rs
// Mapping SQL → Rust : types, layout, sentinels.
// Extrait de crates/core/schema/build.rs (Phase 0 — extraction isofonctionnelle).
// =============================================================================

/// Colonne issue de pg_attribute.
/// `attnum` : numéro physique dans le heap — invariant de Symétrie Mécanique.
#[derive(Debug)]
pub struct Column {
    pub attnum:     i16,
    pub name:       String,
    /// Normalisé par format_type() : ex "character varying(255)", "timestamp with time zone".
    pub sql_type:   String,
    pub is_notnull: bool,
    /// Phase 3 : valeur sentinel annotée via `COMMENT ON COLUMN ... IS 'marius:sentinel=<v>'`.
    /// None → sentinel par défaut du TypeMapping (default_sentinel).
    /// Ignoré si is_notnull (pas de wrapping Option en Row, sentinel sans objet).
    pub sentinel:   Option<String>,
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
    pub row_type:         &'static str,
    /// Type Rust dans la struct StorageRow (#[repr(C)]).
    pub store_type:       &'static str,
    /// Expression de conversion Row → StorageRow.
    /// Placeholders : `{field}` → nom de colonne, `{sentinel}` → valeur sentinel.
    /// Utilisé uniquement pour les colonnes NULLABLE (is_notnull == false).
    pub from_expr:        &'static str,
    /// Valeur sentinel par défaut si Column.sentinel == None.
    /// Substitué dans {sentinel} de from_expr. Domain-specific via Phase 3.
    pub default_sentinel: &'static str,
    /// true si fixed-length → présent dans StorageRow.
    /// false pour varlena (TEXT, VARCHAR, BYTEA…) et types Phase 2.
    pub is_fixed:         bool,
    /// Taille en octets dans la struct repr(C).
    pub size_bytes:       usize,
    /// Alignement naturel en octets (repr(C) aligne sur ce multiple).
    pub alignment:        usize,
}

/// Retourne le mapping SQL → Rust pour un type donné.
///
/// La normalisation `split('(')` rend le match insensible à la précision :
/// "character varying(255)" → "character varying".
pub fn map_type(sql_type: &str) -> TypeMapping {
    let t = sql_type.split('(').next().unwrap_or(sql_type).trim().to_lowercase();

    match t.as_str() {
        "int8" | "bigint" => TypeMapping {
            row_type:   "i64",
            store_type: "i64",
            // Sentinel -1 : CHECK (col >= 0) garantit que -1 = absent.
            // ATTENTION : domain-specific. Phase 3 lira le sentinel depuis pg_description.
            from_expr:        "{field}.unwrap_or({sentinel})",
            default_sentinel: "-1",
            is_fixed: true, size_bytes: 8, alignment: 8,
        },
        "int4" | "integer" | "int" | "serial" => TypeMapping {
            row_type:   "i32",
            store_type: "i32",
            // Sentinel 0 : les IDs (GENERATED ALWAYS AS IDENTITY) commencent à 1.
            from_expr:        "{field}.unwrap_or({sentinel})",
            default_sentinel: "0",
            is_fixed: true, size_bytes: 4, alignment: 4,
        },
        "int2" | "smallint" => TypeMapping {
            row_type:   "i16",
            store_type: "i16",
            from_expr:        "{field}.unwrap_or({sentinel})",
            default_sentinel: "0",
            is_fixed: true, size_bytes: 2, alignment: 2,
        },
        "bool" | "boolean" => TypeMapping {
            row_type:   "bool",
            store_type: "bool",
            from_expr:        "{field}.unwrap_or({sentinel})",
            default_sentinel: "false",
            is_fixed: true, size_bytes: 1, alignment: 1,
        },
        "uuid" => TypeMapping {
            row_type:   "[u8; 16]",
            store_type: "[u8; 16]",
            from_expr:        "{field}.unwrap_or({sentinel})",
            default_sentinel: "[0u8; 16]",
            // [u8; 16] : alignement 1 (tableau d'octets, pas de contrainte supérieure).
            is_fixed: true, size_bytes: 16, alignment: 1,
        },
        // TIMESTAMPTZ → i64 µs depuis l'epoch Unix.
        // Row : chrono::DateTime<Utc> (type riche, sqlx-compatible).
        // Store : i64 (POD, repr(C)-safe). Sentinel 0 = absent.
        "timestamptz" | "timestamp with time zone" => TypeMapping {
            row_type:   "chrono::DateTime<chrono::Utc>",
            store_type: "i64",
            from_expr:        "{field}.map(|dt| dt.timestamp_micros()).unwrap_or({sentinel})",
            default_sentinel: "0",
            is_fixed: true, size_bytes: 8, alignment: 8,
        },
        "timestamp" | "timestamp without time zone" => TypeMapping {
            row_type:   "chrono::NaiveDateTime",
            store_type: "i64",
            from_expr:        "{field}.map(|dt| dt.and_utc().timestamp_micros()).unwrap_or({sentinel})",
            default_sentinel: "0",
            is_fixed: true, size_bytes: 8, alignment: 8,
        },
        "date" => TypeMapping {
            row_type:   "chrono::NaiveDate",
            store_type: "i32",
            // num_days_from_ce() : jours depuis 0001-01-01, toujours positif.
            from_expr:        "{field}.map(|d| d.num_days_from_ce()).unwrap_or({sentinel})",
            default_sentinel: "0",
            is_fixed: true, size_bytes: 4, alignment: 4,
        },
        "float4" | "real" => TypeMapping {
            row_type:   "f32",
            store_type: "f32",
            from_expr:        "{field}.unwrap_or({sentinel})",
            default_sentinel: "0.0",
            is_fixed: true, size_bytes: 4, alignment: 4,
        },
        "float8" | "double precision" => TypeMapping {
            row_type:   "f64",
            store_type: "f64",
            from_expr:        "{field}.unwrap_or({sentinel})",
            default_sentinel: "0.0",
            is_fixed: true, size_bytes: 8, alignment: 8,
        },
        // Varlena : exclus du StorageRow repr(C).
        // Présents dans Row (transport sqlx), portés par VarlenOwned (rendu).
        "text" | "varchar" | "character varying"
        | "jsonb" | "json" | "bytea" | "ltree" => TypeMapping {
            row_type:   "String",
            store_type: "/* VARLENA — exclu du StorageRow repr(C) */",
            from_expr:        "/* VARLENA — non transféré dans StorageRow */",
            default_sentinel: "",
            is_fixed: false, size_bytes: 0, alignment: 0,
        },
        // pg_lsn : 8 octets, pointeur WAL. Phase 1 : commenté. Phase 2 : u64 via mmap.
        "pg_lsn" => TypeMapping {
            row_type:   "/* PHASE2_ONLY: walsn → u64 via mmap */",
            store_type: "/* PHASE2_ONLY */",
            from_expr:        "/* PHASE2_ONLY */",
            default_sentinel: "",
            is_fixed: false, size_bytes: 8, alignment: 8,
        },

        "geometry" => TypeMapping {
            row_type:   "Vec<u8>", // PostGIS renvoie du WKB (Well-Known Binary)
            store_type: "/* VARLENA — exclu du StorageRow repr(C) */",
            from_expr:        "/* VARLENA — non transféré dans StorageRow */",
            default_sentinel: "",
            is_fixed: false, size_bytes: 0, alignment: 0,
        },
        
        other => {
            println!("cargo:warning=DB-Forge : type SQL inconnu '{other}' — exclu");
            TypeMapping {
                row_type: "/* INCONNU */", store_type: "/* INCONNU */",
                from_expr: "/* INCONNU */", default_sentinel: "",
                is_fixed: false, size_bytes: 0, alignment: 0,
            }
        }
    }
}
