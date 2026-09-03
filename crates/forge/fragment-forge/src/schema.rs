// crates/forge/fragment-forge/src/schema.rs

//! Socle DDL → layout Rust : catégorisation des champs fixed-length et
//! varlena, politique d'échappement HTML, index de schéma passé aux phases
//! de résolution et de génération. Zéro dépendance amont dans ce crate.

// crates/forge/fragment-forge/src/lib.rs

//! # Marius Fragment Forge
//!
//! Génération AOT (`build.rs`) du corps de `render()` pour les tables surveillées.
//!
//! Produit une séquence déterministe d'appels (`push_str`, `write_fmt`, `marius_html_escape`)
//! s'exécutant sur une capacité statiquement bornée (`STATIC_CAP + DYNAMIC_CAP`),
//! garantissant l'**absence totale de réallocation** sur le chemin critique.
//!
//! ## Taxonomie des Structures Générées
//!
//! | Structure | Layout | Rôle Mémoire & Invariants | Durée de vie |
//! | :--- | :--- | :--- | :--- |
//! | `{Name}Row` | Non-`repr(C)` | Transport `sqlx` (Base $\rightarrow$ Site de projection). Varlenas portées via `Option<String>` (allocations heap). | Éphémère (détruite après `render()`) |
//! | `{Name}StorageRow` | `#[repr(C)]` | Stockage contigu en mémoire. Types à taille fixe uniquement (alignés sur DDL). Exclut les varlenas (incompatibles : *fat pointer* de 16 B). | Persistante (cache CPU-friendly) |
//! | `{Name}RenderPayload` | Non-`repr(C)` | Struct de rendu éphémère. Emprunte les varlenas (`&'a str`) depuis la `Row` sans copie ni allocation. | Limitée à `render()` (`'a`) |
//!
//! ## Chemin Critique & Invariants (`no_std` attitude)
//!
//! ```text
//! StorageRow (repr(C)) + RenderPayload (&'a str)  ==>  render()  ==>  buf: &mut String
//! ```
//!
//! - **Garantie de capacité :** `buf.capacity()` doit rester strictement identique avant et après `render()`.
//! - **Zéro logique dynamique :** Aucun branchement (`if`/`match`) dans le template généré.
//! - **Borne exacte :** `STATIC_CAP` (octets fixes) + `DYNAMIC_CAP` (largeurs pires cas).
//!
//! ## Politique d'Échappement HTML (`EscapePolicy`)
//!
//! - `FieldPolicy::Normal` : Taille $\times 5$ (pire cas : `&` $\rightarrow$ `&amp;`).
//! - `FieldPolicy::PreEscaped` : Taille $\times 1$ (Tag DDL `marius:pre_escaped`).
//! - `FieldPolicy::Raw` : Taille $\times 1$, injecté sans passage par l'échappeur runtime (Tag DDL `marius:raw`).
//!
//! *Référence : ADR-002 (`no_std-attitude-within-marius.md`)*

/// Catégorie d'un champ à taille fixe pour le calcul de capacité dynamique à la compilation.
///
/// Encodes les bornes pires cas d'affichage textuel pour déduire `DYNAMIC_CAP` dans `build.rs`.
///
/// ### Mappage DDL
/// - `I64`  : `TIMESTAMPTZ`, `TIMESTAMP`, `BIGINT`
/// - `I32`  : `INTEGER`, `INT`, `SERIAL`, `DATE`
/// - `I16`  : `SMALLINT`
/// - `Bool` : `BOOLEAN`
/// - `F32`  : `REAL`
/// - `F64`  : `DOUBLE PRECISION`
///
/// *Note : Les types varlena (`TEXT`, `VARCHAR`, `JSONB`) sont gérés séparément via `VarlenField`.*
#[derive(Debug, Clone, Copy)]
pub enum FieldKind {
    /// `i64::MIN` = `-9223372036854775808` (20 chars)
    I64,
    /// `i32::MIN` = `-2147483648` (11 chars)
    I32,
    /// `i16::MIN` = `-32768` (6 chars)
    I16,
    /// `"false"` (5 chars)
    Bool,
    /// Pire cas flottant 32-bit (`-3.40282347e38` $\approx$ 14 chars)
    F32,
    /// Pire cas flottant 64-bit ($\approx$ 24 chars)
    F64,
}

impl FieldKind {
    /// Largeur d'affichage maximale du champ, en octets (pire cas, pas de padding HTML).
    ///
    /// Ces valeurs constituent la composante fixe de DYNAMIC_CAP.
    /// Une sous-estimation ici provoque un realloc au runtime → violation de l'invariant
    /// no-realloc → détecté par test_{name}_no_realloc().
    pub const fn max_display_width(self) -> usize {
        match self {
            Self::I64 => 20, // "-9223372036854775808"
            Self::I32 => 11, // "-2147483648"
            Self::I16 => 6,  // "-32768"
            Self::Bool => 5, // "false"
            Self::F32 => 14, // "-3.40282347e38" (approximation conservative)
            Self::F64 => 24, // représentation longue
        }
    }

    /// Construit un FieldKind depuis le type SQL retourné par format_type().
    ///
    /// Retourne None pour les types varlena ou inconnus.
    /// None signifie : ce champ sort du pipeline fixed-length et sera traité
    /// par VarlenField si applicable, ou exclu de la projection si inconnu.
    pub fn from_sql_type(sql_type: &str) -> Option<Self> {
        // Normalise "character varying(255)" → "character varying"
        // pour que le match soit insensible à la précision.
        let t = sql_type
            .split('(')
            .next()
            .unwrap_or(sql_type)
            .trim()
            .to_lowercase();
        match t.as_str() {
            "int8"
            | "bigint"
            | "timestamptz"
            | "timestamp with time zone"
            | "timestamp"
            | "timestamp without time zone" => Some(Self::I64),
            "int4" | "integer" | "int" | "serial" | "date" => Some(Self::I32),
            "int2" | "smallint" => Some(Self::I16),
            "bool" | "boolean" => Some(Self::Bool),
            "float4" | "real" => Some(Self::F32),
            "float8" | "double precision" => Some(Self::F64),
            // Varlena, pg_lsn, types inconnus → exclu du pipeline fixed-length.
            _ => None,
        }
    }
}

/// Spécification d'un champ fixed-length pour Fragment-Forge.
///
/// Produit par build.rs à partir de pg_attribute, dans l'ordre attnum.
/// L'ordre est l'invariant de Symétrie Mécanique : il garantit la cohérence
/// entre le layout PostgreSQL (heap tuple) et la struct #[repr(C)] générée.
#[derive(Debug, Clone)]
pub struct FieldSpec {
    /// Nom de la colonne (attname).
    pub name: String,
    /// Catégorie de type (détermine max_display_width).
    pub kind: FieldKind,
    /// Numéro d'attribut physique dans le heap PostgreSQL (pg_attribute.attnum).
    /// Strictement positif (les colonnes système ont attnum <= 0).
    pub attnum: i16,
}

/// Spécification d'un champ varlena d'une table jointe (LEFT JOIN).
///
/// ─── Politique de max_len ────────────────────────────────────────────────────
///
///   VARCHAR(N)            → max_len = N  (depuis atttypmod - 4)
///   TEXT sans contrainte  → exclu du listing render (build.rs émet cargo:warning)
///   TEXT avec CHECK       → build.rs extrait N de la contrainte CHECK (length(col) <= N)
///
/// ─── Politique d'escape HTML ─────────────────────────────────────────────────
///
///   Facteur normal     : HTML_ESCAPE_FACTOR = 6
///     Pire cas : '"' → "&quot;" (6 chars). C'est le pire de tous les escapes.
///     S'applique à tout champ sans annotation.
///
///   Facteur pre_escaped : 1
///     Le commentaire SQL `COMMENT ON COLUMN ... IS 'marius:pre_escaped'`
///     certifie que le contenu est déjà sanitisé (slugs, titres normalisés…).
///     introspect.rs lit pg_description pour détecter ce tag.
///     Un facteur de 1 évite la sur-estimation de DYNAMIC_CAP pour ces champs.
///     Échappé quand même au runtime (défense en profondeur) — seule la
///     capacité déclarée change, pas le comportement d'échappement.
///
///   Facteur raw : 1, ET jamais échappé au runtime
///     Le commentaire SQL `COMMENT ON COLUMN ... IS 'marius:raw'` certifie que
///     le contenu est du HTML déjà constitué, à injecter tel quel — distinct
///     de pre_escaped : le contenu contient potentiellement beaucoup de
///     caractères spéciaux intentionnels (balises), ce n'est pas leur absence
///     qui justifie l'exemption, c'est leur nature de balisage voulu tel quel.
///     Voir EscapePolicy.
///
/// ─── Ownership des données ───────────────────────────────────────────────────
///
///   Dans {Name}Row           : Option<String>  (allocation sqlx, durée éphémère)
///   Dans {Name}RenderPayload : Option<&'a str> (emprunt, zéro copie, durée render())
///
///   Le passage de String → &str se fait dans le site d'appel (Dispatcher),
///   via payload.field = row.field.as_deref().
#[derive(Debug, Clone)]
pub struct VarlenField {
    /// Nom de la colonne dans la table jointe.
    pub name: String,
    /// Schéma de la table jointe source de ce champ (ex: "content").
    ///
    /// CONTRAT-implementation-multi-slot-varlena.md, Étape 2 : nécessaire dès
    /// qu'un composant porte plusieurs joins varlena (join_slot_idx > 0) — sans
    /// cette provenance, codegen/projection.rs ne peut pas qualifier
    /// correctement chaque colonne dans un SELECT multi-JOIN (un seul `vt`
    /// capturé hors boucle appliqué à tort à tous les champs, bug corrigé à
    /// l'Étape 4). Sert aussi de matière première aux messages de collision
    /// de l'Étape 3 (nommer les deux tables sources en conflit).
    pub ref_schema: String,
    /// Table jointe source de ce champ (ex: "body"). Voir `ref_schema`.
    pub ref_table: String,
    /// Borne supérieure en octets, si elle existe dans le schéma PostgreSQL
    /// (VARCHAR(N) via atttypmod, ou TEXT avec CHECK(length(col) <= N) parsable).
    ///
    /// `None` (ADR-007) : la colonne est un TEXT sans contrainte exploitable —
    /// ni VARCHAR(N), ni CHECK reconnu. Ce n'est PAS une erreur en soi : la
    /// classification Hot/Cold/Erreur est tranchée par resolve_and_measure
    /// selon que le champ est référencé ou non par le template résolu.
    /// Aucun fallback numérique n'est jamais substitué à None — une absence
    /// de borne reste une absence de borne jusqu'à la frontière de résolution.
    pub max_len: Option<usize>,
    /// Politique d'échappement — état fermé, aucune combinaison invalide
    /// représentable (CONTRAT-implementation-varlena-raw.md, Étape 2,
    /// arbitrage du 22/07/2026 : option (b), enum plutôt que deux booléens
    /// couplés `pre_escaped`/`raw` dont l'état simultané `true`/`true` serait
    /// une aberration sémantique sans le typage fermé). Voir `EscapePolicy`.
    pub escape_policy: EscapePolicy,
    /// true si ce champ est un contenu volumineux (tag SQL
    /// `COMMENT ON COLUMN ... IS 'marius:large_content'`), destiné à devenir
    /// un `Segment::Borrowed` autonome plutôt qu'à être concaténé dans le
    /// buffer partagé — CONTRAT-implementation-projection-segmentee.md,
    /// Étape 1. Implique toujours `escape_policy == EscapePolicy::Raw` (un
    /// champ segmenté est emprunté zéro-copie, incompatible avec un passage
    /// par `marius_html_escape`) — invariant vérifié à la construction dans
    /// introspect.rs, pas ici (VarlenField reste une structure de données
    /// passive, sans logique de validation).
    pub is_segment: bool,
    /// true si la colonne DDL est nullable (Option<String> dans VarlenOwned).
    /// En v1, toujours true (LEFT JOIN produit systématiquement Option).
    /// Réservé v2 : champ NOT NULL → String directe, court-circuite l'Option.
    pub nullable: bool,
    /// Surcharge manuelle de max_escaped_len. None = calculé (max_len × facteur).
    /// Utile quand la borne théorique est trop conservative pour un champ donné.
    /// Sans effet si `max_len` est également `None` — il n'y a alors rien à
    /// surcharger, `max_escaped_len()` retourne None indépendamment de ce champ.
    /// Sans effet non plus si `is_segment == true` : un champ segmenté ne
    /// contribue jamais à `DYNAMIC_CAP`, indépendamment de cette surcharge.
    pub max_escaped_len_override: Option<usize>,
}

/// Politique d'échappement d'un champ varlena au runtime — trois états
/// mutuellement exclusifs par construction (enum fermé, pas deux booléens
/// couplés). CONTRAT-implementation-varlena-raw.md, Étape 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscapePolicy {
    /// Texte quelconque, cas par défaut. Échappé au runtime
    /// (`marius_html_escape`), facteur de capacité `HTML_ESCAPE_FACTOR` (6).
    Escaped,
    /// Contenu certifié sans caractères spéciaux à échapper
    /// (tag SQL `COMMENT ON COLUMN ... IS 'marius:pre_escaped'`) — titres,
    /// slugs normalisés. Échappé quand même au runtime (défense en profondeur
    /// — le contenu réel n'est jamais vérifié mécaniquement, seulement
    /// déclaré sans danger par le schéma), facteur de capacité 1.
    PreEscaped,
    /// HTML déjà constitué, à injecter tel quel (tag SQL
    /// `COMMENT ON COLUMN ... IS 'marius:raw'`) — ex. `content.body.content`.
    /// JAMAIS échappé au runtime (`buf.push_str` direct, aucun appel à
    /// `marius_html_escape`), facteur de capacité 1. Distinct de `PreEscaped` :
    /// le contenu contient au contraire potentiellement beaucoup de
    /// caractères spéciaux intentionnels (balises) — ce n'est pas leur absence
    /// qui justifie l'exemption d'échappement, c'est leur nature de balisage
    /// déjà voulu tel quel.
    Raw,
}

impl VarlenField {
    /// Facteur d'escape HTML pire cas (champ non annoté).
    ///
    /// '"' → '&quot;' = 1 char source → 6 chars HTML. Pire cas parmi
    /// les 5 caractères escapés (&, <, >, ", '). Garantit l'invariant
    /// no-realloc même si le contenu est rempli de guillemets.
    pub const HTML_ESCAPE_FACTOR: usize = 6;

    /// Longueur maximale après escape HTML, en octets — si elle est connue.
    ///
    /// Composante varlena de DYNAMIC_CAP.
    /// Priorité : is_segment > max_escaped_len_override > escape_policy.
    ///
    /// Un champ segmenté (`is_segment == true`, CONTRAT-implementation-
    /// projection-segmentee.md Étape 1) ne contribue jamais à DYNAMIC_CAP —
    /// il ne traverse jamais `buf` — sauf s'il n'a aucune borne connue
    /// (`max_len == None`), auquel cas il reste soumis à la même table de
    /// vérité Hot/Cold/Erreur que tout autre champ (ADR-007) : cette méthode
    /// retourne alors `None`, à charge de `resolve_and_measure` de lever
    /// `UnboundedField` si le champ est référencé.
    ///
    /// Correction (23/07/2026) : la phrase précédente affirmait à tort que
    /// `max_escaped_len_override` était sans effet quand `max_len` est
    /// `None` — faux, le code retourne l'override avant même de consulter
    /// `max_len`. Comportement préexistant, non modifié ici ; seule la
    /// documentation était incorrecte.
    pub fn max_escaped_len(&self) -> Option<usize> {
        if self.is_segment {
            return self.max_len.map(|_| 0);
        }
        if let Some(override_len) = self.max_escaped_len_override {
            return Some(override_len);
        }
        let n = self.max_len?;
        // match exhaustif — un futur variant d'EscapePolicy casserait la
        // compilation ici plutôt que de tomber silencieusement dans un
        // mauvais facteur par défaut (garantie du typage fermé, Étape 2).
        Some(match self.escape_policy {
            EscapePolicy::Escaped => n * Self::HTML_ESCAPE_FACTOR,
            EscapePolicy::PreEscaped | EscapePolicy::Raw => n,
        })
    }
}

/// Index du schéma passé à resolve_and_measure et generate_aot_snippet.
///
/// Permet la résolution et la validation des tokens Field/IfBool du template
/// contre le schéma réel de la table. Passé par référence — zéro copie.
///
/// Recherche O(n) sur les slices : les tables ont < 30 colonnes en pratique,
/// ce qui rend un HashMap superflu et moins cache-friendly.
pub struct SchemaIndex<'a> {
    /// Champs fixed-length du StorageRow, dans l'ordre attnum.
    pub fixed: &'a [FieldSpec],
    /// Champs varlena de la table jointe.
    pub varlena: &'a [VarlenField],
}

impl<'a> SchemaIndex<'a> {
    /// Recherche un champ fixed-length par nom. Retourne None si absent.
    #[inline]
    pub fn find_fixed(&self, name: &str) -> Option<&'a FieldSpec> {
        self.fixed.iter().find(|f| f.name == name)
    }

    /// Recherche un champ varlena par nom. Retourne None si absent.
    #[inline]
    pub fn find_varlena(&self, name: &str) -> Option<&'a VarlenField> {
        self.varlena.iter().find(|v| v.name == name)
    }
}
