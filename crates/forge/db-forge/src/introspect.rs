// crates/forge/db-forge/src/introspect.rs
//
//! # marius-db-forge - introspect
//! Requêtes SQLx d'introspection pg_catalog / information_schema / pg_stats.

use sqlx::Row as _;

use crate::mapping::{Column, PrimaryKey};
use marius_fragment_forge::{EscapePolicy, VarlenField};

// =============================================================================
// I. Colonnes fixed-length (pg_attribute)
// =============================================================================

/// Colonnes dans l'ordre physique du heap (attnum ASC).
///
/// ORDER BY attnum est l'invariant de Symétrie Mécanique : il garantit que
/// l'ordre des champs dans {Name}StorageRow (#[repr(C)]) correspond exactement
/// à l'ordre des colonnes dans le heap tuple PostgreSQL.
pub async fn fetch_columns(
    pool: &sqlx::PgPool,
    schema: &str,
    table: &str,
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

    Ok(rows
        .into_iter()
        .map(|r| Column {
            attnum: r.get::<i16, _>(0),
            name: r.get::<String, _>(1),
            sql_type: r.get::<String, _>(2),
            is_notnull: r.get::<bool, _>(3),
            sentinel: parse_sentinel(&r.get::<String, _>(4)),
        })
        .collect())
}

// =============================================================================
// II. Clé primaire (information_schema)
// =============================================================================

/// Identifie la PK via information_schema.
///
/// Retourne Single(col) si PK sur une colonne unique, Composite sinon.
/// Une PK Composite rend le Collector inapplicable.
pub async fn fetch_pk_column(
    pool: &sqlx::PgPool,
    schema: &str,
    table: &str,
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
            eprintln!(
                "DB-Forge [{schema}.{table}] : PK composite ({n} colonnes) — Collector ignoré."
            );
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
    pool: &sqlx::PgPool,
    schema: &str,
    table: &str,
    pk_col: &str,
) -> Result<usize, Box<dyn std::error::Error>> {
    // format! obligatoire : sqlx ne supporte pas l'interpolation d'identifiants SQL.
    // Risque injection nul : pk_col est issu de pg_constraint (catalogue système).
    let query = format!("SELECT COALESCE(MAX({pk_col}), 0)::BIGINT FROM {schema}.{table}");

    let max_id: i64 = sqlx::query_scalar::<_, i64>(&query)
        .fetch_one(pool)
        .await
        .unwrap_or(0);

    let with_margin = (max_id as f64 * 1.20).ceil() as usize;
    let words_needed = with_margin.max(64).div_ceil(64);
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
/// ─── Politique max_len (ADR-007) ─────────────────────────────────────────────
///
///   VARCHAR(N)           : max_len = Some(atttypmod - 4).
///   TEXT avec CHECK       : max_len extrait de pg_get_constraintdef(oid).
///                           Si non parsable → None (jamais de fallback numérique).
///   TEXT sans contrainte  : max_len = None.
///
///   `None` n'exclut PLUS le champ du listing render (changement vs Phase 4) :
///   le champ reste visible dans le Vec<VarlenField> retourné. La frontière
///   Hot/Cold/Erreur est tranchée plus loin dans le pipeline, par
///   resolve_and_measure (marius-fragment-forge), selon que le champ est
///   référencé ou non par le template résolu — pas ici. Voir ADR-007.
///
///   max_escaped_len > 64 KB : panic! (seuil AOT). Vérifié seulement si
///   max_len = Some(_) — un champ non borné n'a rien à valider ici, sa
///   validation est différée à la résolution (ResolverError::UnboundedField
///   si jamais référencé sans borne).
///
/// ─── Politique escape_policy (EscapePolicy) + is_segment ────────────────────
///
///   Si COMMENT ON COLUMN ... IS 'marius:pre_escaped' → PreEscaped, facteur 1,
///     échappé quand même au runtime (défense en profondeur).
///   Si COMMENT ON COLUMN ... IS 'marius:raw' → Raw, facteur 1, JAMAIS échappé
///     au runtime (HTML déjà constitué — CONTRAT-implementation-varlena-raw.md).
///   Si COMMENT ON COLUMN ... IS 'marius:large_content' → Raw + is_segment,
///     contribution nulle à DYNAMIC_CAP, jamais concaténé dans buf — devient
///     un Segment::Borrowed autonome (CONTRAT-implementation-projection-
///     segmentee.md). Tag déclarant une propriété métier (contenu volumineux),
///     pas un mécanisme — le choix de la stratégie de rendu appartient au
///     générateur.
///   Sinon → Escaped, facteur VarlenField::HTML_ESCAPE_FACTOR (6).
pub async fn fetch_varlena_cols(
    pool: &sqlx::PgPool,
    schema: &str,
    table: &str,
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
        let name: String = row.get(0);
        let typmod: i32 = row.get(1);
        let description: String = row.get(2);

        // CONTRAT-implementation-varlena-raw.md, Étape 1+2 : EscapePolicy
        // (enum fermé) remplace l'ancien bool pre_escaped isolé — mutuellement
        // exclusif de facto, `description` ne peut être égal qu'à une seule
        // chaîne à la fois.
        //
        // CONTRAT-implementation-projection-segmentee.md, Étape 1 :
        // 'marius:large_content' déclare une PROPRIÉTÉ métier (ce champ est un
        // contenu volumineux), pas un mécanisme — c'est au générateur de
        // choisir la stratégie de rendu (aujourd'hui : projection segmentée,
        // Segment::Borrowed zéro-copie). Implique toujours EscapePolicy::Raw :
        // un champ segmenté est par nature emprunté zéro-copie, incompatible
        // avec un passage par marius_html_escape (qui exige de recopier
        // caractère par caractère dans buf).
        let (escape_policy, is_segment) = match description.trim() {
            "marius:pre_escaped" => (EscapePolicy::PreEscaped, false),
            "marius:raw" => (EscapePolicy::Raw, false),
            "marius:large_content" => (EscapePolicy::Raw, true),
            _ => (EscapePolicy::Escaped, false),
        };

        // Garde-fou défensif : cette construction ne devrait jamais produire
        // is_segment=true avec autre chose que Raw (les deux sont fixés
        // ensemble ci-dessus, dans la même branche de match) — vérifié quand
        // même explicitement, incohérence interne à signaler fort plutôt qu'à
        // laisser passer silencieusement si ce match venait à être modifié
        // sans respecter l'invariant.
        if is_segment && escape_policy != EscapePolicy::Raw {
            panic!(
                "DB-Forge [{schema}.{table}.{name}]: incohérence interne — \
                 is_segment=true sans EscapePolicy::Raw. Un champ segmenté \
                 doit toujours être Raw (emprunté zéro-copie, jamais échappé)."
            );
        }

        // ── Résolution de max_len — Option<usize>, jamais de fallback ─────────
        let max_len: Option<usize> = if typmod > 4 {
            // Cas 1 : VARCHAR(N) → atttypmod = N + 4. Borne structurelle, fiable.
            Some((typmod - 4) as usize)
        } else {
            // Cas 2 : TEXT/BPCHAR sans précision → chercher CHECK (length(col) <= N).
            // pg_get_constraintdef(oid) : pg_constraint.consrc a été supprimée en
            // PostgreSQL 12 — c'est l'API stable depuis, retournant le texte complet
            // de la contrainte (ex: "CHECK ((length(description) <= 2000))").
            //
            // Hypothèses non garanties par PostgreSQL pour ce chemin (cf. ADR-007,
            // audit H1–H10) : au plus une contrainte CHECK pertinente par colonne
            // (H1, non vérifié ici — fetch_optional reste volontairement simple
            // dans cette PR, la grammaire stricte est différée), forme canonique
            // `length(col) <= N` (H2/H3/H9, non vérifiée — parse_check_length_limit
            // reste un parsing best-effort), stabilité du format pg_get_constraintdef
            // inter-versions majeures (H4). Aucune heuristique supplémentaire n'est
            // ajoutée dans cette PR — un échec de parsing devient None, pas un panic
            // ni un fallback numérique.
            let check_row = sqlx::query(
                "SELECT pg_get_constraintdef(con.oid)::text
                 FROM   pg_constraint  con
                 JOIN   pg_class       cls ON cls.oid = con.conrelid
                 JOIN   pg_namespace   ns  ON ns.oid  = cls.relnamespace
                 WHERE  ns.nspname  = $1
                   AND  cls.relname = $2
                   AND  con.contype = 'c'
                   AND  (pg_get_constraintdef(con.oid) LIKE '%length(' || $3 || ')%'
                      OR pg_get_constraintdef(con.oid) LIKE '%char_length(' || $3 || ')%')",
            )
            .bind(schema)
            .bind(table)
            .bind(&name)
            .fetch_optional(pool)
            .await?;

            match check_row {
                Some(check_r) => {
                    let consrc: String = check_r.get(0);
                    match parse_check_length_limit(&consrc) {
                        Some(n) => Some(n),
                        None => {
                            println!(
                                "cargo:warning=DB-Forge [{schema}.{table}.{name}]: \
                                 CHECK trouvé mais longueur non parsable : `{consrc}`. \
                                 Traité comme non borné (max_len=None) — voir ADR-007."
                            );
                            None
                        }
                    }
                }
                None => {
                    // Cas 3 : TEXT sans contrainte. Le champ N'EST PLUS exclu du
                    // Vec<VarlenField> (changement ADR-007) : il reste visible pour
                    // que resolve_and_measure puisse le classer Cold (non référencé,
                    // aucun impact) ou échouer explicitement avec UnboundedField
                    // (référencé sans borne) plutôt que de disparaître silencieusement
                    // ou de subir un fallback arbitraire.
                    println!(
                        "cargo:warning=DB-Forge [{schema}.{table}.{name}]: \
                         TEXT sans contrainte de longueur — champ non borné (Cold sauf \
                         si référencé par un template, alors erreur de compilation)."
                    );
                    None
                }
            }
        };

        // ── Validation AOT : seuil absolu 64 KB ──────────────────────────────
        // Seulement si une borne existe — un champ non borné n'a rien à valider
        // ici ; sa validation est différée à resolve_and_measure (Étape 3).
        //
        // TODO du 22/07/2026 — RÉSOLU par CONTRAT-implementation-projection-
        // segmentee.md (session du 23/07/2026) : un champ marius:large_content
        // (is_segment == true) ne traverse jamais buf, donc ce seuil ne
        // s'applique pas à lui — cf. branche dédiée ci-dessous. Le TODO
        // d'origine évoquait un chunking côté PostgreSQL ; la décision retenue
        // (ADR-010) a été de résoudre au niveau du rendu (projection
        // segmentée), pas du stockage — TOAST gère déjà le stockage physique
        // de champs volumineux, cf. ADR-010 §2.
        if is_segment {
            // Contribution nulle par construction (VarlenField::max_escaped_len()
            // renvoie Some(0) pour is_segment == true) — aucune vérification de
            // seuil n'a de sens ici, le champ ne dimensionne jamais buf.
        } else if let Some(n) = max_len {
            // Étape 3 (CONTRAT-implementation-varlena-raw.md) : PreEscaped et
            // Raw partagent le même facteur de capacité (1) — la différence
            // entre les deux n'est jamais dans ce calcul, seulement dans le
            // comportement runtime (échappé quand même vs jamais échappé),
            // décidé plus loin dans le pipeline (codegen, fragment-forge).
            let escape_factor = match escape_policy {
                EscapePolicy::Escaped => VarlenField::HTML_ESCAPE_FACTOR,
                EscapePolicy::PreEscaped | EscapePolicy::Raw => 1,
            };
            let max_escaped = n * escape_factor;
            if max_escaped > 65_536 {
                panic!(
                    "DB-Forge [{schema}.{table}.{name}]: \
                     max_escaped_len ({max_escaped}B) > 64 KB. \
                     Réduire la contrainte VARCHAR/CHECK, ou tagger la colonne \
                     'marius:large_content' si le contenu doit légitimement \
                     dépasser cette borne (projection segmentée)."
                );
            }
        }

        // ── Validation AOT : pression avg_width → DYNAMIC_CAP ────────────────
        // Sans objet si max_len est None — pas de borne contre laquelle mesurer
        // la pression statistique.
        if let Some(n) = max_len {
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
                if avg_width as usize > n * 8 / 10 {
                    println!(
                        "cargo:warning=DB-Forge [{schema}.{table}.{name}]: \
                         avg_width observé ({avg_width}B) > 80% de max_len ({n}B). \
                         Pression sur DYNAMIC_CAP."
                    );
                }
            }
        }

        // nullable=true : toujours le cas en v1, LEFT JOIN peut produire NULL.
        // max_escaped_len_override=None : valeur calculée (max_len × facteur),
        // pas de surcharge manuelle pour l'instant.
        // ref_schema/ref_table : provenance du champ (CONTRAT-implementation-
        // multi-slot-varlena.md, Étape 2) — schema/table sont ici les
        // paramètres de fetch_varlena_cols, c'est-à-dire la table jointe
        // elle-même, pas le composant appelant.
        fields.push(VarlenField {
            name,
            ref_schema: schema.to_string(),
            ref_table: table.to_string(),
            max_len,
            escape_policy,
            is_segment,
            nullable: true,
            max_escaped_len_override: None,
        });
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
        assert_eq!(
            parse_check_length_limit("(length(label) <= 255)"),
            Some(255)
        );
    }

    #[test]
    fn parse_check_char_length() {
        assert_eq!(
            parse_check_length_limit("(char_length(bio) <= 1000)"),
            Some(1000)
        );
    }

    #[test]
    fn parse_check_unknown() {
        assert_eq!(parse_check_length_limit("(label IS NOT NULL)"), None);
    }

    #[test]
    fn parse_check_reversed_operands_not_supported() {
        // H2 (ADR-007) : forme inversée non reconnue par construction — la
        // fonction ne cherche que "<=", jamais ">=". Dégrade vers None, ne
        // produit jamais une valeur incorrecte à partir d'une lecture inversée.
        assert_eq!(parse_check_length_limit("(255 >= length(label))"), None);
    }

    #[test]
    fn parse_check_non_literal_expression_rejected() {
        // H9 (ADR-007) : N doit être un entier littéral nu. parse::<usize>()
        // échoue nativement sur "2*1000" — dégrade vers None, jamais vers un
        // calcul silencieusement erroné de l'expression.
        assert_eq!(parse_check_length_limit("(length(label) <= 2*1000)"), None);
    }

    #[test]
    fn parse_check_negative_bound_rejected() {
        // Défense en profondeur : un CHECK absurde (N négatif) ne doit jamais
        // produire un usize incorrect via cast implicite — parse::<usize>
        // échoue nativement sur le signe, dégrade vers None.
        assert_eq!(parse_check_length_limit("(length(label) <= -5)"), None);
    }

    #[test]
    fn parse_check_tolerates_whitespace_variance() {
        // Contre-exemple positif : une reformulation bénigne d'un CHECK
        // existant (espacement) ne doit jamais devenir une régression
        // silencieuse — sinon un simple reformattage SQL non fonctionnel
        // ferait basculer un champ Hot vers Erreur au prochain build.
        assert_eq!(
            parse_check_length_limit("(length(label)   <=   255 )"),
            Some(255)
        );
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
        sqlx::PgPool::connect(&url)
            .await
            .expect("Connexion PgPool échouée")
    }

    /// fetch_component_list() retourne au moins 2 composants.
    /// Invariant : meta.containment_intent contient au minimum
    /// content.core et commerce.product_core.
    #[tokio::test]
    #[ignore]
    async fn fetch_component_list_returns_at_least_two() {
        let pool = connect().await;
        let comps = fetch_component_list(&pool)
            .await
            .expect("fetch_component_list échoué");
        assert!(
            comps.len() >= 2,
            "Moins de 2 composants — meta.containment_intent vide ou incomplet : {:?}",
            comps
                .iter()
                .map(|c| format!("{}.{}", c.schema, c.table))
                .collect::<Vec<_>>()
        );
    }

    /// fetch_columns() pour content.core retourne les colonnes triées attnum ASC.
    /// Invariant de Symétrie Mécanique : l'ordre attnum == l'ordre StorageRow.
    #[tokio::test]
    #[ignore]
    async fn fetch_columns_content_core_ordered_by_attnum() {
        let pool = connect().await;
        let cols = fetch_columns(&pool, "content", "core")
            .await
            .expect("fetch_columns échoué");
        assert!(
            !cols.is_empty(),
            "Aucune colonne pour content.core — table absente ou vide"
        );
        for w in cols.windows(2) {
            assert!(
                w[0].attnum < w[1].attnum,
                "Colonnes non triées par attnum : {} ({}) >= {} ({})",
                w[0].name,
                w[0].attnum,
                w[1].name,
                w[1].attnum,
            );
        }
    }

    /// validate_layout() passe pour tous les composants enregistrés
    /// avec intent_density != 0.
    /// Régression directe de Phase 2 : aucune divergence tolérée post-correction.
    #[tokio::test]
    #[ignore]
    async fn validate_layout_passes_for_all_registered_components() {
        let pool = connect().await;
        let comps = fetch_component_list(&pool)
            .await
            .expect("fetch_component_list échoué");

        for comp in &comps {
            if comp.intent_density == 0 {
                continue;
            }

            let cols = fetch_columns(&pool, &comp.schema, &comp.table)
                .await
                .expect(&format!(
                    "fetch_columns échoué pour {}.{}",
                    comp.schema, comp.table
                ));

            validate_layout(&cols, comp.intent_density)
                .unwrap_or_else(|msg| panic!("{}.{} : {}", comp.schema, comp.table, msg));
        }
    }

    /// Non-régression du passage au match tri-état (CONTRAT-implementation-
    /// projection-segmentee.md, Étape 1) : content.body.content porte
    /// aujourd'hui le tag 'marius:raw' (posé par le Contrat varlena-raw,
    /// migration 04) — PAS ENCORE 'marius:large_content' (Étape 7 de CE
    /// Contrat, non exécutée à ce jour). Le nouveau match doit continuer à
    /// résoudre ce cas exactement comme avant : Raw, is_segment == false.
    /// À METTRE À JOUR après l'Étape 7 : is_segment devra alors être true.
    #[tokio::test]
    #[ignore]
    async fn content_body_content_is_raw_not_yet_segment() {
        let pool = connect().await;
        let fields = fetch_varlena_cols(&pool, "content", "body")
            .await
            .expect("fetch_varlena_cols échoué pour content.body");

        let content_field = fields
            .iter()
            .find(|f| f.name == "content")
            .expect("colonne 'content' absente de content.body");

        assert_eq!(
            content_field.escape_policy,
            EscapePolicy::Raw,
            "content.body.content devrait rester Raw (tag marius:raw, migration 04)"
        );
        assert!(
            !content_field.is_segment,
            "content.body.content ne devrait pas encore être is_segment=true \
             (Étape 7 de CONTRAT-implementation-projection-segmentee.md non exécutée)"
        );
    }
}
