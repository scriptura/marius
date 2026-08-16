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

// =============================================================================
// I.bis Utilitaires de chemin — préparation I/O disque (templates .marius)
// =============================================================================

/// Convertit un chemin de fichier en identifiant Rust valide pour une
/// constante statique (`static_partials::IDENT`).
///
/// Règles :
///   - Tout caractère non [A-Za-z0-9_] devient '_'.
///   - Le résultat est mis en SCREAMING_SNAKE_CASE.
///   - Un préfixe '_' est ajouté si le premier caractère est un chiffre
///     (un identifiant Rust ne peut pas commencer par un chiffre).
///
/// Exemples :
///   "partials/nav.html"      → "PARTIALS_NAV_HTML"
///   "fragments/2024/foo.htm" → "_FRAGMENTS_2024_FOO_HTM"
pub fn static_const_ident(path: &str) -> String {
    let mut ident: String = path
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect::<String>()
        .to_uppercase();

    if ident.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        ident.insert(0, '_');
    }

    ident
}

/// Calcule le chemin à utiliser dans `include_str!()` pour un template
/// référencé depuis le fichier généré (`{out_dir}/{table}_render.rs`).
///
/// `include_str!` résout ses chemins relativement au fichier source qui
/// contient la macro, pas relativement à `CARGO_MANIFEST_DIR`. Le plus
/// robuste est donc d'émettre un chemin absolu : `include_str!` l'accepte
/// aussi bien qu'un chemin relatif, et ça évite tout calcul de profondeur
/// fragile entre OUT_DIR (profond, sous target/debug/build/.../out) et
/// la racine du manifeste.
///
/// `manifest_dir`       : CARGO_MANIFEST_DIR du crate appelant build.rs.
/// `rel_from_manifest`  : chemin du template relatif au manifeste
///                        (ex: "templates/page.marius").
pub fn relative_path_for_include_str(manifest_dir: &str, rel_from_manifest: &str) -> String {
    use std::path::Path;
    Path::new(manifest_dir)
        .join(rel_from_manifest)
        .to_string_lossy()
        .into_owned()
}

// =============================================================================
// II. Calcul de capacité
// =============================================================================

/// Somme des octets HTML statiques entourant les valeurs dynamiques.
///
/// Calcul purement arithmétique — zéro allocation, zéro construction de String.
/// Appelé au build-time (build.rs), résultat émis comme constante dans le code généré.
///
/// ─── Structure HTML générée ──────────────────────────────────────────────────
///
///   <article class="{schema}-{table}" data-id="{pk}">
///     <dl>
///       <dt>{field.name}</dt><dd>{field.value}</dd>
///       ...  (champs fixed-length)
///       <dt>{varlena.name}</dt><dd>{varlena.value}</dd>
///       ...  (champs varlena)
///     </dl>
///   </article>
///
/// ─── Décomposition comptable ─────────────────────────────────────────────────
///
///   Tag ouvrant : `<article class="` (16) + schema + `-` (1) + table + `" data-id="` (11)
///   Après PK    : `"><dl>` (6)  [ferme data-id + ouvre dl]
///   Par champ   : `<dt>` (4) + nom + `</dt><dd>` (9) + `</dd>` (5)
///   Tag fermant : `</dl></article>` (15)
///
/// Note : les valeurs dynamiques (PK, champs) ne sont PAS comptées ici.
/// Leur capacité est désormais calculée par `resolve_and_measure` (Voie B),
/// pas par une fonction `dynamic_capacity` dédiée — supprimée avec ce patch
/// (ADR-007) : elle était un résidu Voie A sans appelant, et sa signature
/// (`.sum::<usize>()` sur `VarlenField::max_escaped_len()`) ne pouvait plus
/// compiler une fois cette méthode passée à `Option<usize>` (champs non
/// bornés). `TemplateMetrics::total_dynamic_bytes` est l'unique source de
/// vérité pour la capacité dynamique.
pub fn static_capacity(
    schema: &str,
    table: &str,
    fields: &[FieldSpec],
    varlena: &[VarlenField],
) -> usize {
    // `<article class="` = 16 octets
    // schema
    // `-` = 1 octet
    // table
    // `" data-id="` = 11 octets
    let open_tag = 16 + schema.len() + 1 + table.len() + 11;

    // `"><dl>` = 6 octets (ferme la valeur de data-id, ferme le tag article, ouvre dl)
    let after_id = 6;

    let mut cap = open_tag + after_id;

    // Par champ fixed-length : `<dt>` (4) + nom + `</dt><dd>` (9) + `</dd>` (5) = 18 + len(nom)
    for f in fields {
        cap += 4 + f.name.len() + 9 + 5;
    }

    // Par champ varlena : même structure de balisage.
    for v in varlena {
        cap += 4 + v.name.len() + 9 + 5;
    }

    // `</dl></article>` = 15 octets
    cap += 15;

    cap
}

// =============================================================================
// III. Génération du corps de render()
// =============================================================================

// =============================================================================
// V. En-tête du fichier généré
// =============================================================================

/// Retourne l'en-tête injecté en tête de generated_schema.rs.
///
/// Contient la fonction marius_html_escape(), inline dans le fichier généré
/// pour éviter toute dépendance externe depuis le chemin critique.
///
/// ─── Politique d'escape ──────────────────────────────────────────────────────
///
///   Seuls les 5 caractères dangereux en HTML sont transformés :
///     '&'  → "&amp;"   (doit être premier pour éviter le double-escape)
///     '<'  → "&lt;"
///     '>'  → "&gt;"
///     '"'  → "&quot;"  (attributs HTML)
///     '\'' → "&#39;"   (attributs non-quotés)
///
///   Tous les autres caractères (Unicode inclus) sont émis tels quels via push(ch).
///   L'itérateur chars() garantit que les séquences multi-octets UTF-8 sont
///   traitées correctement sans risque de corruption de la représentation.
///
/// ─── Invariant no-alloc ──────────────────────────────────────────────────────
///
///   marius_html_escape() n'alloue pas. Elle écrit dans buf (déjà réservé).
///   Si buf a été pré-alloué avec STATIC_CAP + DYNAMIC_CAP, et que VarlenField
///   a été configuré avec le bon max_escaped_len(), aucun realloc ne peut survenir.
///
/// ─── Absence de use std::path::PathBuf ───────────────────────────────────────
///
///   PathBuf est importé ici pour artifact_path() généré dans le même fichier.
///   Cet import couvre l'ensemble du module généré.
pub fn generated_file_header() -> &'static str {
    "// GÉNÉRÉ PAR DB-FORGE + FRAGMENT-FORGE — NE PAS MODIFIER MANUELLEMENT\n\
     // Régénérer via : cargo build (relit pg_attribute + pg_description)\n\n\
     use std::path::PathBuf;\n\
     // Import du trait Projection dans le scope du fichier généré.\n\
     // Requis pour que fetch_batch() et render() soient résolus sur les types\n\
     // de projection générés, aussi bien dans le code appelant que dans les tests.\n\
     #[allow(unused_imports)]\n\
     use crate::projection::Projection as _;\n\n\
     /// Échappe les caractères HTML dangereux dans `s` et pousse le résultat dans `buf`.\n\
     ///\n\
     /// Zéro allocation : opère directement sur buf (déjà réservé par render()).\n\
     /// Ordre des branches : '&' en premier pour éviter le double-escape de '&amp;'.\n\
     #[inline(always)]\n\
     fn marius_html_escape(s: &str, buf: &mut String) {\n\
         for ch in s.chars() {\n\
             match ch {\n\
                 '&'  => buf.push_str(\"&amp;\"),\n\
                 '<'  => buf.push_str(\"&lt;\"),\n\
                 '>'  => buf.push_str(\"&gt;\"),\n\
                 '\"' => buf.push_str(\"&quot;\"),\n\
                 '\\'' => buf.push_str(\"&#39;\"),\n\
                 _    => buf.push(ch),\n\
             }\n\
         }\n\
     }\n\n"
}

/// Token de l'AST d'un template `.marius`.
///
/// Le lifetime `'src` est lié à la durée de vie de la `String` source lue par
/// `std::fs::read_to_string` dans la fonction mère de `build.rs`.
/// L'AST ne sort jamais de cette portée : `'src` est localement borné,
/// jamais exposé à travers une frontière de thread ou de module.
///
/// Invariant : zéro allocation. Tous les champs texte sont des slices
/// pointant directement dans le buffer source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlatPageToken<'src> {
    /// Segment HTML verbatim.
    Static(&'src str),

    /// Interpolation de champ : `{{ entity.field }}`.
    Field { entity: &'src str, field: &'src str },

    /// Bloc conditionnel booléen : `{% if entity.field %}`.
    IfBool { entity: &'src str, field: &'src str },

    /// Fermeture de bloc : `{% endif %}`.
    EndIf,

    /// Référence à un artefact préparé par `marius-assets` : `{% asset key %}`
    /// (spec `marius-assets-specification.md` §9). `key` est l'identifiant
    /// logique écrit tel quel par le développeur (`main.css`), jamais une
    /// URL — la résolution vers le chemin public versionné n'a jamais lieu
    /// ici. `fragment-forge` ne lit jamais le manifeste d'assets lui-même
    /// (aucun I/O dans ce module) : `resolve_and_measure` et
    /// `generate_aot_snippet` reçoivent chacun une closure de résolution
    /// injectée par `build.rs`, même patron que `StaticInclude`/
    /// `get_file_size` ci-dessous — à la différence que la closure ne
    /// renvoie jamais le contenu d'un fichier (inadapté : un asset est une
    /// URL, pas du texte à inliner), seulement une longueur (mesure) puis
    /// une chaîne résolue (émission).
    ///
    /// Contrairement à `StaticInclude`, ce token ne porte aucun champ
    /// provisoire à patcher en place : `resolve_and_measure` accumule
    /// directement la longueur résolue dans `TemplateMetrics`, sans jamais
    /// muter l'AST pour cette variante.
    AssetRef(&'src str),

    /// `{% script %}` — ouvre une région de capture opaque pour le
    /// hoisting des `<script>` (session dédiée). Symétrique à `IfBool` par
    /// la forme (marqueur de bloc, validé par `validate_ast`), mais
    /// orthogonal par le fond : `IfBool` gate un rendu RUNTIME (dépend de
    /// la ligne), `ScriptStart`/`ScriptEnd` délimitent une région connue
    /// intégralement à la COMPILATION — d'où deux états de FSM indépendants
    /// dans `validate_ast`, jamais un seul état partagé.
    ///
    /// Le contenu entre `ScriptStart` et `ScriptEnd` (typiquement un tag
    /// `<script>` complet écrit par le développeur, avec les attributs de
    /// SON choix — `defer`, `id`, `integrity`... — ce Parser n'a et n'aura
    /// jamais de connaissance de la grammaire HTML `<script>` elle-même)
    /// est capturé verbatim par `hoist_and_dedupe_scripts`. Ces deux
    /// marqueurs eux-mêmes n'émettent jamais rien dans `generate_aot_snippet`
    /// (No-Op pur) : c'est `build.rs` qui décide, selon que la cible de
    /// compilation est une Page (layout avec `<head>`) ou un Fragment
    /// isolé, d'appeler ou non la passe de hoisting en amont — cette
    /// distinction ne vit jamais dans ce crate.
    ScriptStart,
    /// `{% endscript %}` — ferme la région ouverte par `ScriptStart`.
    ScriptEnd,

    /// Inclusion statique résolue au build-time : `{% include path %}`.
    ///
    /// `len` : longueur en octets du fichier inclus, connue à la compilation
    /// (via `std::fs::metadata`). Composante directe de `PAGE_STATIC_CAP`.
    StaticInclude {
        original_path: &'src str,
        rel_from_manifest: &'src str,
        len: usize,
    },

    /// Point d'extension textuel post-abaissement : `<!-- MARIUS_MODULES -->`
    /// dans `base.marius`, position sœur de `ScriptStart`/`ScriptEnd`
    /// (HANDOFF-js-deps-capacites-frontend-v2.md, § Lowering AOT de
    /// `js_deps`). Jamais produit par le scanner/parser — `{% %}`/`{{ }}`
    /// restent les seules syntaxes actives de ce crate. Injecté directement
    /// dans le flux de tokens par `build.rs`, après `lower`, par recherche
    /// de sous-chaîne dans un `Static` : même mécanisme que
    /// `SCRIPTS_PLACEHOLDER`/`splice_hoisted_scripts`, jamais un nouveau
    /// chemin de parsing.
    ///
    /// Ne porte aucune donnée propre : `build.rs` calcule intégralement, en
    /// amont, la vue de compilation `bit → (URL, activation)` (lecture de
    /// `theme.toml`, `scripts_registry.lock`, `AssetManifest`) et la fournit
    /// à `resolve_and_measure` sous forme d'une longueur (mesure), puis à
    /// `generate_aot_snippet`/`generate_segmented_snippet` sous forme d'une
    /// chaîne de code Rust déjà assemblée (émission) — `fragment-forge` ne
    /// connaît et ne doit connaître aucune des trois sources.
    ///
    /// Contexte de lowering dépendant de l'appelant — propriété du
    /// CONTEXTE, jamais de ce token lui-même : `resolve_page_template`
    /// (Mode Page, `record` réel) fournit la vue calculée ; `resolve_static_page`
    /// (`STATIC_PAGES`, aucun `record`) fournit systématiquement 0 octet /
    /// chaîne vide — un ensemble de capacités par définition vide pour une
    /// page sans état éditorial, jamais un cas d'erreur ni un no-op
    /// accidentel : c'est le comportement normal du lowering dans ce
    /// pipeline.
    ModulesPlaceholder,
}

#[cfg(test)]
mod tests_phase_1_1 {
    use super::FlatPageToken;

    /// Jalon Vert Phase 1.1 — compilation sans annotation de lifetime explicite.
    ///
    /// Le compilateur infère `FlatPageToken<'_>` depuis la durée de vie de `src`.
    /// Aucune annotation `<'static>` ni `<'_>` n'est requise au site de construction.
    #[test]
    fn static_variant_infers_lifetime() {
        let src: &str = "hello";
        let token = FlatPageToken::Static(src);
        match token {
            FlatPageToken::Static(s) => assert_eq!(s, "hello"),
            _ => unreachable!(),
        }
    }

    /// Vérifie que `Copy` est disponible sur tous les variants.
    ///
    /// Preuve : `tokens[0]` est réaffecté deux fois sans move.
    /// Si `Copy` manquait (champ non-Copy), ce test ne compilerait pas.
    #[test]
    fn all_variants_are_copy() {
        let tokens: [FlatPageToken<'_>; 6] = [
            FlatPageToken::Static("content"),
            FlatPageToken::Field {
                entity: "user",
                field: "name",
            },
            FlatPageToken::IfBool {
                entity: "user",
                field: "active",
            },
            FlatPageToken::EndIf,
            FlatPageToken::StaticInclude {
                original_path: "templates/header.html",
                rel_from_manifest: "../templates/header.html",
                len: 42,
            },
            FlatPageToken::ModulesPlaceholder,
        ];

        let _a = tokens[0]; // premier move apparent
        let _b = tokens[0]; // second : compile ssi Copy est implémenté
    }
}

// =============================================================================
// Phase 1.2 — Scanner Lexical Isolé
// =============================================================================
//
// Responsabilité unique : découper `&'src str` en sous-slices typées (RawSpan).
//
// Invariants stricts :
//   - Zéro allocation heap. Aucun Vec, String, Box dans le corps du scanner.
//   - Tous les RawSpan::slice pointent directement dans `src` (fat pointer).
//   - `Scanner::pos` est toujours sur une frontière de char UTF-8 valide.
//     → Garanti en Literal (find() retourne des offsets de frontières).
//     → Garanti en InExpr (seuls des bytes ASCII sont consommés : ident, `.`, `{{`, `}}`).
//     → Garanti en InBlock sous l'hypothèse ASCII (keywords, paths, identifiants SQL).
//       Un byte non-ASCII en InBlock interrompt le token ; Phase 1.4 remonte l'erreur.
//   - Aucune sémantique résolue ici : pas de distinction keyword vs ident, pas de lookup.

/// Catégorie syntaxique brute d'un span issu du scanner.
///
/// `Punct` est émis uniquement en mode `InExpr` pour le séparateur `.`.
/// En mode `InBlock`, `entity.field` est émis en un seul `Ident` — Phase 1.3
/// se charge de la découpe sur `.`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpanKind {
    Literal,    // Texte HTML verbatim hors délimiteurs
    ExprOpen,   // `{{`
    ExprClose,  // `}}`
    BlockOpen,  // `{%`
    BlockClose, // `%}`
    Ident,      // Identifiant (entity, field, keyword, chemin de fichier)
    Punct,      // `.` — séparateur entity.field dans {{ … }} uniquement
}

/// Sous-slice typée pointant directement dans la source brute du template.
///
/// `'src` est lié à la durée de vie de la `String` lue par `fs::read_to_string`
/// dans la fonction mère de `build.rs`. Le span ne survit jamais à cette portée.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawSpan<'src> {
    pub slice: &'src str,
    pub kind: SpanKind,
}

// ─── État interne ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Literal, // Entre les blocs : tout est HTML statique jusqu'au prochain délimiteur.
    InExpr,  // Intérieur de `{{ … }}` : Ident, Punct, ExprClose.
    InBlock, // Intérieur de `{% … %}` : Ident (token brut), BlockClose.
}

struct Scanner<'src> {
    src: &'src str,
    pos: usize, // Offset byte courant — toujours sur une frontière char valide.
    mode: Mode,
}

impl<'src> Scanner<'src> {
    fn new(src: &'src str) -> Self {
        Self {
            src,
            pos: 0,
            mode: Mode::Literal,
        }
    }

    /// Avance `pos` au-delà des espaces ASCII (U+0009, U+000A, U+000D, U+0020).
    ///
    /// Tous ces bytes sont des chars ASCII single-byte : `pos` reste sur une
    /// frontière valide après l'appel.
    #[inline]
    fn skip_ws(&mut self) {
        let b = self.src.as_bytes();
        while self.pos < self.src.len() {
            match b[self.pos] {
                b' ' | b'\t' | b'\n' | b'\r' => self.pos += 1,
                _ => break,
            }
        }
    }
}

impl<'src> Iterator for Scanner<'src> {
    type Item = RawSpan<'src>;

    fn next(&mut self) -> Option<Self::Item> {
        let src = self.src;

        if self.pos >= src.len() {
            return None;
        }

        match self.mode {
            // ─── Literal ───────────────────────────────────────────────────
            // Cherche le prochain `{{` ou `{%`.
            // Émet le Literal précédant le délimiteur, puis le délimiteur lui-même
            // (en deux appels distincts — pas de buffer intermédiaire).
            Mode::Literal => {
                let rest = &src[self.pos..];

                // `str::find` retourne des offsets sur des frontières char valides.
                let rel = match (rest.find("{{"), rest.find("{%")) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (Some(a), None) | (None, Some(a)) => Some(a),
                    (None, None) => None,
                };

                match rel {
                    // Pas de délimiteur : le reste est un unique Literal.
                    None => {
                        let span = RawSpan {
                            slice: &src[self.pos..],
                            kind: SpanKind::Literal,
                        };
                        self.pos = src.len();
                        Some(span)
                    }
                    // Délimiteur immédiat : l'émettre et basculer de mode.
                    Some(0) => {
                        let p = self.pos;
                        if src[p..].starts_with("{{") {
                            self.mode = Mode::InExpr;
                            self.pos = p + 2;
                            Some(RawSpan {
                                slice: &src[p..p + 2],
                                kind: SpanKind::ExprOpen,
                            })
                        } else {
                            // starts_with("{%") — seule autre option possible
                            self.mode = Mode::InBlock;
                            self.pos = p + 2;
                            Some(RawSpan {
                                slice: &src[p..p + 2],
                                kind: SpanKind::BlockOpen,
                            })
                        }
                    }
                    // Literal précède le délimiteur.
                    // On émet le Literal et on reste en mode Literal :
                    // le délimiteur sera émis au prochain appel.
                    Some(rel) => {
                        let end = self.pos + rel;
                        let span = RawSpan {
                            slice: &src[self.pos..end],
                            kind: SpanKind::Literal,
                        };
                        self.pos = end;
                        Some(span)
                    }
                }
            }

            // ─── InExpr ────────────────────────────────────────────────────
            // Produit : Ident ([a-zA-Z0-9_]+), Punct (`.`), ExprClose (`}}`).
            // Les espaces sont ignorés (skip_ws).
            // Un `{{` non fermé retourne None ; Phase 1.4 détecte le déséquilibre.
            Mode::InExpr => {
                self.skip_ws();
                if self.pos >= src.len() {
                    return None; // `{{` non fermé — invalide, catchable en Phase 1.4
                }

                let p = self.pos;

                if src[p..].starts_with("}}") {
                    self.mode = Mode::Literal;
                    self.pos = p + 2;
                    return Some(RawSpan {
                        slice: &src[p..p + 2],
                        kind: SpanKind::ExprClose,
                    });
                }

                if src[p..].starts_with('.') {
                    self.pos = p + 1;
                    return Some(RawSpan {
                        slice: &src[p..p + 1],
                        kind: SpanKind::Punct,
                    });
                }

                // Identifiant : séquence de bytes ASCII alphanumériques ou `_`.
                // Tous single-byte → `pos` reste sur une frontière valide.
                let start = p;
                let b = src.as_bytes();
                while self.pos < src.len()
                    && (b[self.pos].is_ascii_alphanumeric() || b[self.pos] == b'_')
                {
                    self.pos += 1;
                }

                if self.pos > start {
                    Some(RawSpan {
                        slice: &src[start..self.pos],
                        kind: SpanKind::Ident,
                    })
                } else {
                    // Byte inattendu (non-ASCII ou ponctuation inconnue).
                    // Avance d'un byte et relance : la récursion est bornée à 1 niveau
                    // car le byte suivant sera soit un char reconnu, soit `}}`.
                    self.pos += 1;
                    self.next()
                }
            }

            // ─── InBlock ───────────────────────────────────────────────────
            // Produit : Ident (token brut), BlockClose (`%}`).
            // Un token = séquence contiguë non-blanc non-`%}`.
            // Inclut les chemins (`dir/file.html`) et `entity.field` sans découpage.
            // Hypothèse : tout contenu de bloc est ASCII (identifiants SQL, paths, keywords).
            Mode::InBlock => {
                self.skip_ws();
                if self.pos >= src.len() {
                    return None; // `{%` non fermé — invalide, catchable en Phase 1.4
                }

                let p = self.pos;
                let b = src.as_bytes();

                if b[p] == b'%' && p + 1 < src.len() && b[p + 1] == b'}' {
                    self.mode = Mode::Literal;
                    self.pos = p + 2;
                    return Some(RawSpan {
                        slice: &src[p..p + 2],
                        kind: SpanKind::BlockClose,
                    });
                }

                // Scan byte par byte jusqu'à un espace ou `%}`.
                // Chaque byte consommé est ASCII (hypothèse documentée ci-dessus) :
                // `pos` reste sur une frontière char valide.
                let start = p;
                while self.pos < src.len() {
                    let byte = b[self.pos];
                    if matches!(byte, b' ' | b'\t' | b'\n' | b'\r') {
                        break;
                    }
                    // Détection de `%}` sur deux bytes ASCII consécutifs.
                    if byte == b'%' && self.pos + 1 < src.len() && b[self.pos + 1] == b'}' {
                        break;
                    }
                    self.pos += 1;
                }

                if self.pos > start {
                    Some(RawSpan {
                        slice: &src[start..self.pos],
                        kind: SpanKind::Ident,
                    })
                } else {
                    None // Byte non-ASCII isolé — Phase 1.4 remonte l'erreur structurelle.
                }
            }
        }
    }
}

/// Retourne un itérateur de `RawSpan<'src>` sur la source brute du template.
///
/// L'itérateur est alloué sur la pile (24 octets : `&str` fat pointer + `usize` + `Mode`).
/// Zéro allocation heap dans le corps du scanner.
pub fn scan(src: &str) -> impl Iterator<Item = RawSpan<'_>> {
    Scanner::new(src)
}

// =============================================================================
// Tests — Phase 1.2
// =============================================================================

#[cfg(test)]
mod tests_phase_1_2 {
    use super::{RawSpan, SpanKind, scan};

    // Helper : construit un RawSpan depuis une &'static str.
    // PartialEq sur &str compare le contenu, pas l'adresse.
    // Un RawSpan issu du scanner (slice dans `src`) égale un RawSpan construit
    // depuis un littéral statique si leurs contenus sont identiques.
    fn s(slice: &str, kind: SpanKind) -> RawSpan<'_> {
        RawSpan { slice, kind }
    }

    /// Jalon Vert Phase 1.2 — séquence exacte pour `"hello {{ user.name }} world"`.
    ///
    /// Vérifie : nombre, ordre, contenu textuel et catégorie de chaque span.
    /// Pas d'assertion sur les adresses mémoire (la découpe par contenu suffit).
    #[test]
    fn scan_expr_interpolation() {
        let src = "hello {{ user.name }} world";
        let got: Vec<_> = scan(src).collect();

        let expected = [
            s("hello ", SpanKind::Literal),
            s("{{", SpanKind::ExprOpen),
            s("user", SpanKind::Ident),
            s(".", SpanKind::Punct),
            s("name", SpanKind::Ident),
            s("}}", SpanKind::ExprClose),
            s(" world", SpanKind::Literal),
        ];

        assert_eq!(got.len(), expected.len(), "nombre de spans incorrect");
        for (i, (got_span, exp)) in got.iter().zip(expected.iter()).enumerate() {
            assert_eq!(got_span, exp, "span[{i}] incorrect");
        }
    }

    /// Bloc conditionnel complet avec Literal intercalé.
    /// Vérifie que InBlock scanne `entity.field` comme un seul Ident
    /// (la découpe sur `.` appartient à Phase 1.3).
    #[test]
    fn scan_block_if_endif() {
        let src = "{% if user.active %}oui{% endif %}";
        let got: Vec<_> = scan(src).collect();

        let expected = [
            s("{%", SpanKind::BlockOpen),
            s("if", SpanKind::Ident),
            s("user.active", SpanKind::Ident),
            s("%}", SpanKind::BlockClose),
            s("oui", SpanKind::Literal),
            s("{%", SpanKind::BlockOpen),
            s("endif", SpanKind::Ident),
            s("%}", SpanKind::BlockClose),
        ];

        assert_eq!(got.len(), expected.len(), "nombre de spans incorrect");
        for (i, (g, e)) in got.iter().zip(expected.iter()).enumerate() {
            assert_eq!(g, e, "span[{i}]");
        }
    }

    /// Cas limites : source vide et source sans délimiteur.
    #[test]
    fn scan_empty_and_literal_only() {
        assert!(
            scan("").next().is_none(),
            "source vide doit être épuisée immédiatement"
        );

        let got: Vec<_> = scan("<p>texte statique</p>").collect();
        assert_eq!(got, [s("<p>texte statique</p>", SpanKind::Literal)]);
    }

    /// Vérifie qu'un délimiteur en tête de source produit ExprOpen sans Literal vide.
    #[test]
    fn scan_delimiter_at_start() {
        let got: Vec<_> = scan("{{ x }}").collect();
        assert_eq!(
            got[0].kind,
            SpanKind::ExprOpen,
            "le premier span doit être ExprOpen, pas un Literal vide"
        );
    }
}

// =============================================================================
// Phase 1.3 — Classifieur de Tokens (Parseur Syntaxique)
// =============================================================================
//
// Responsabilité unique : traduire un flux de RawSpan en Vec<FlatPageToken>.
//
// Frontières strictes :
//   - Syntaxe uniquement. Pas de lookup dans SchemaContext (Phase 1.4).
//   - Pas d'équilibrage IfBool/EndIf (Phase 1.4).
//   - Pas d'I/O disque : StaticInclude::len = 0 (hack provisoire documenté).
//   - Fail-fast : première erreur syntaxique = retour immédiat.

/// Erreur syntaxique produite par `parse_tokens`.
///
/// Couvre uniquement les erreurs de structure token-niveau.
/// La validation sémantique (champ inconnu, déséquilibre if/endif)
/// est déléguée à Phase 1.4.
#[derive(Debug, PartialEq, Eq)]
pub enum PageParseError {
    /// Token reçu ≠ token attendu à cette position de l'automate.
    UnexpectedToken {
        expected: &'static str,
        got: SpanKind,
    },
    /// Itérateur épuisé alors qu'un token était requis pour compléter un pattern.
    UnexpectedEof,
    /// Séquence de bloc non reconnue :
    ///   keyword inconnu, ou `if entity.field` sans `.` dans l'ident bloc.
    InvalidBlockSequence,
}

/// Transforme un flux de `RawSpan<'src>` en AST `Vec<FlatPageToken<'src>>`.
///
/// Automate à états implicites : chaque appel à `next()` sur l'itérateur
/// consomme la tête de séquence, et les helpers consomment les spans suivants
/// selon le pattern du token courant.
///
/// `.peekable()` est créé ici pour que Phase 1.4 puisse étendre ce parseur
/// avec du lookahead sans changer la signature de `parse_tokens`.
///
/// Allocation : le `Vec` de sortie est build-time uniquement.
/// Il est consommé par les phases 2 et 3 et n'existe pas au runtime.
pub fn parse_tokens<'src>(
    spans: impl Iterator<Item = RawSpan<'src>>,
) -> Result<Vec<FlatPageToken<'src>>, PageParseError> {
    let mut iter = spans.peekable();
    let mut ast = Vec::new();

    while let Some(span) = iter.next() {
        let token = match span.kind {
            // Texte HTML verbatim → Static directement.
            SpanKind::Literal => FlatPageToken::Static(span.slice),

            // `{{ entity.field }}` → Field.
            SpanKind::ExprOpen => parse_expr(&mut iter)?,

            // `{% keyword … %}` → IfBool | EndIf | StaticInclude.
            SpanKind::BlockOpen => parse_block(&mut iter)?,

            // Tout autre span en position initiale est une erreur structurelle.
            // ExprClose, BlockClose, Ident, Punct ne peuvent pas ouvrir un token.
            got => {
                return Err(PageParseError::UnexpectedToken {
                    expected: "Literal | ExprOpen | BlockOpen",
                    got,
                });
            }
        };
        ast.push(token);
    }

    Ok(ast)
}

// ─── Parseurs de sous-séquences ──────────────────────────────────────────────

/// Consomme `Ident(entity) Punct(.) Ident(field) ExprClose` et produit `Field`.
///
/// Précondition : `ExprOpen` vient d'être consommé par `parse_tokens`.
fn parse_expr<'src, I>(iter: &mut I) -> Result<FlatPageToken<'src>, PageParseError>
where
    I: Iterator<Item = RawSpan<'src>>,
{
    let entity = expect_ident(iter, "Ident(entity)")?;
    expect_kind(iter, SpanKind::Punct, "Punct('.')")?;
    let field = expect_ident(iter, "Ident(field)")?;
    expect_kind(iter, SpanKind::ExprClose, "ExprClose('}}')")?;
    Ok(FlatPageToken::Field { entity, field })
}

/// Consomme `Ident(keyword) … BlockClose` et produit le token de bloc.
///
/// Précondition : `BlockOpen` vient d'être consommé par `parse_tokens`.
///
/// Pattern `if` : `Ident("entity.field")` est découpé sur `.` ici,
/// car le scanner InBlock le produit comme un seul Ident (contrairement
/// à InExpr qui émet `Ident Punct Ident`). Voir décision Phase 1.2.
///
/// Pattern `include` : `len = 0` et `rel_from_manifest = original_path`
/// sont des valeurs provisoires. L'orchestrateur (build.rs) injectera
/// la longueur réelle via `std::fs::metadata` après le parsing.
fn parse_block<'src, I>(iter: &mut I) -> Result<FlatPageToken<'src>, PageParseError>
where
    I: Iterator<Item = RawSpan<'src>>,
{
    let keyword = expect_ident(
        iter,
        "keyword (if | endif | include | asset | script | endscript)",
    )?;

    match keyword {
        "if" => {
            let raw = expect_ident(iter, "Ident(entity.field)")?;
            let (entity, field) = split_dotted(raw)?;
            expect_kind(iter, SpanKind::BlockClose, "BlockClose('%}')")?;
            Ok(FlatPageToken::IfBool { entity, field })
        }
        "endif" => {
            expect_kind(iter, SpanKind::BlockClose, "BlockClose('%}')")?;
            Ok(FlatPageToken::EndIf)
        }
        // `{% asset key %}` (spec §9) : capture brute de la clé logique,
        // zéro E/S, zéro résolution — même discipline que `include` :
        // la résolution (ici vers une URL, jamais un contenu) est différée
        // à `resolve_and_measure`/`generate_aot_snippet`.
        "asset" => {
            let key = expect_ident(iter, "Ident(key)")?;
            expect_kind(iter, SpanKind::BlockClose, "BlockClose('%}')")?;
            Ok(FlatPageToken::AssetRef(key))
        }
        // `{% script %}` / `{% endscript %}` (session dédiée au hoisting) :
        // valides dans les deux modes, comme `asset` — un Fragment inclus
        // peut légitimement porter son propre `<script>`, hissé si la
        // cible finale est une Page, laissé inline (No-Op) si la cible est
        // le Fragment lui-même résolu isolément (voir doc de la variante).
        "script" => {
            expect_kind(iter, SpanKind::BlockClose, "BlockClose('%}')")?;
            Ok(FlatPageToken::ScriptStart)
        }
        "endscript" => {
            expect_kind(iter, SpanKind::BlockClose, "BlockClose('%}')")?;
            Ok(FlatPageToken::ScriptEnd)
        }
        "include" => {
            let path = expect_ident(iter, "Ident(path)")?;
            expect_kind(iter, SpanKind::BlockClose, "BlockClose('%}')")?;
            Ok(FlatPageToken::StaticInclude {
                original_path: path,
                rel_from_manifest: path, // provisoire : sera résolu par l'orchestrateur
                len: 0,                  // provisoire : idem
            })
        }
        _ => Err(PageParseError::InvalidBlockSequence),
    }
}

// ─── Primitives de consommation ───────────────────────────────────────────────

/// Consomme le span suivant et retourne sa slice si c'est un `Ident`.
/// Retourne une erreur décrivant ce qui était attendu sinon.
#[inline]
fn expect_ident<'src, I>(iter: &mut I, expected: &'static str) -> Result<&'src str, PageParseError>
where
    I: Iterator<Item = RawSpan<'src>>,
{
    match iter.next() {
        Some(span) if span.kind == SpanKind::Ident => Ok(span.slice),
        Some(span) => Err(PageParseError::UnexpectedToken {
            expected,
            got: span.kind,
        }),
        None => Err(PageParseError::UnexpectedEof),
    }
}

/// Consomme le span suivant et vérifie qu'il a le `kind` attendu.
/// La slice n'est pas retournée (les délimiteurs ne portent pas de sémantique).
#[inline]
fn expect_kind<'src, I>(
    iter: &mut I,
    kind: SpanKind,
    expected: &'static str,
) -> Result<(), PageParseError>
where
    I: Iterator<Item = RawSpan<'src>>,
{
    match iter.next() {
        Some(span) if span.kind == kind => Ok(()),
        Some(span) => Err(PageParseError::UnexpectedToken {
            expected,
            got: span.kind,
        }),
        None => Err(PageParseError::UnexpectedEof),
    }
}

/// Coupe `"entity.field"` sur le premier `.` et retourne `("entity", "field")`.
///
/// Les sous-slices partagent le lifetime `'src` de `raw` :
/// elles pointent directement dans la source du template, sans allocation.
///
/// `.` est ASCII single-byte : `i` et `i+1` sont des frontières char valides.
#[inline]
fn split_dotted(raw: &str) -> Result<(&str, &str), PageParseError> {
    raw.find('.')
        .map(|i| (&raw[..i], &raw[i + 1..]))
        .ok_or(PageParseError::InvalidBlockSequence)
}

// =============================================================================
// Tests — Phase 1.3
// =============================================================================

#[cfg(test)]
mod tests_phase_1_3 {
    use super::{FlatPageToken, PageParseError, SpanKind, parse_tokens, scan};

    /// Jalon Vert Phase 1.3.
    ///
    /// Pipeline complet : scan() → parse_tokens() sur la chaîne de référence.
    ///
    /// Décompte : 8 tokens (et non 7 comme indiqué dans le prompt).
    /// Les 3 espaces inter-blocs (" ") produisent 3 Static("·") distincts
    /// car le scanner est en mode Literal entre chaque `%}` et le `{%` suivant.
    /// Supprimer ces espaces serait une décision sémantique qui appartient
    /// à l'orchestrateur ou à un éventuel pass de compression — pas au parseur.
    #[test]
    fn parse_full_template() {
        let src =
            "hello {{ user.name }} {% if user.active %} {% include fragment.html %} {% endif %}";
        let got = parse_tokens(scan(src)).expect("parsing doit réussir sur un template valide");

        // Note : FlatPageToken doit dériver PartialEq, Eq (ajout non-cassant sur Phase 1.1).
        let expected: &[FlatPageToken<'_>] = &[
            FlatPageToken::Static("hello "),
            FlatPageToken::Field {
                entity: "user",
                field: "name",
            },
            FlatPageToken::Static(" "),
            FlatPageToken::IfBool {
                entity: "user",
                field: "active",
            },
            FlatPageToken::Static(" "),
            FlatPageToken::StaticInclude {
                original_path: "fragment.html",
                rel_from_manifest: "fragment.html",
                len: 0,
            },
            FlatPageToken::Static(" "),
            FlatPageToken::EndIf,
        ];

        assert_eq!(
            got.len(),
            expected.len(),
            "nombre de tokens incorrect : got {}, expected {}",
            got.len(),
            expected.len()
        );

        for (i, (g, e)) in got.iter().zip(expected.iter()).enumerate() {
            assert_eq!(g, e, "token[{i}] incorrect");
        }
    }

    /// Erreur sur token inattendu en position initiale (ExprClose seul, sans ExprOpen).
    #[test]
    fn error_on_unexpected_top_level_span() {
        // Scanner ne peut pas produire un ExprClose seul en position initiale,
        // mais ce test vérifie le chemin d'erreur de parse_tokens directement.
        use super::RawSpan;
        let orphan = [RawSpan {
            slice: "}}",
            kind: SpanKind::ExprClose,
        }];
        let err = parse_tokens(orphan.into_iter()).unwrap_err();
        assert_eq!(
            err,
            PageParseError::UnexpectedToken {
                expected: "Literal | ExprOpen | BlockOpen",
                got: SpanKind::ExprClose,
            }
        );
    }

    /// Erreur sur `{% if active %}` sans préfixe `entity.` (pas de `.` dans l'ident).
    #[test]
    fn error_on_if_without_dot() {
        let src = "{% if active %}";
        let err = parse_tokens(scan(src)).unwrap_err();
        assert_eq!(err, PageParseError::InvalidBlockSequence);
    }

    /// Erreur sur `{% %}` (keyword manquant → BlockClose immédiat après BlockOpen).
    #[test]
    fn error_on_empty_block() {
        let src = "{% %}";
        let err = parse_tokens(scan(src)).unwrap_err();
        // Le scanner InBlock voit "%}" immédiatement → Ident attendu, BlockClose reçu.
        assert_eq!(
            err,
            PageParseError::UnexpectedToken {
                expected: "keyword (if | endif | include | asset | script | endscript)",
                got: SpanKind::BlockClose,
            }
        );
    }
}

// =============================================================================
// Phase 1.4 — Validateur Sémantique (Structural Validator)
// =============================================================================
//
// Responsabilité unique : valider les invariants structurels de la FSM de rendu.
//
// Frontières strictes :
//   - Lecture seule sur &[FlatPageToken<'src>] : aucune modification de l'AST.
//   - Accumulation exhaustive des erreurs : pas de fail-fast.
//   - Aucune consultation de SchemaContext : les champs entity/field ne sont
//     pas vérifiés contre le schéma BDD ici (périmètre Phase suivante).
//   - Zéro récursion. FSM linéaire : une seule variable d'état scalaire.
//
// Justification de l'interdiction d'imbrication (invariant DOD) :
//   Un moteur de rendu linéaire sans pile de récursion exige que le graphe
//   de contrôle soit un DAG plat. Un `if` imbriqué introduit un niveau de
//   call stack ou un compteur de profondeur au runtime — incompatible avec
//   l'invariant zéro-allocation du hot path.

/// Erreur sémantique produite par `validate_ast`.
///
/// Les champs `&'src str` pointent directement dans les tokens de l'AST,
/// eux-mêmes pointant dans le buffer source lu par `fs::read_to_string`.
/// Aucune allocation. Le Vec<SemanticError> lui-même est build-time uniquement.
#[derive(Debug, PartialEq, Eq)]
pub enum SemanticError<'src> {
    /// Un `{% endif %}` rencontré alors qu'aucun bloc n'était ouvert.
    UnexpectedEndIf,
    /// Un `{% if %}` rencontré alors qu'un bloc était déjà ouvert.
    /// L'ouverture imbriquée est ignorée (heuristique de récupération) :
    /// l'état courant est préservé, le prochain `EndIf` ferme le bloc externe.
    NestedIfNotSupported {
        nested_entity: &'src str,
        nested_field: &'src str,
    },
    /// Fin de l'AST atteinte alors qu'un bloc `if` était encore ouvert.
    UnclosedIf { entity: &'src str, field: &'src str },

    /// Un `{% endscript %}` rencontré alors qu'aucun bloc n'était ouvert.
    /// Symétrique à `UnexpectedEndIf` — FSM indépendante, voir doc de
    /// `validate_ast`.
    UnexpectedEndScript,
    /// Un `{% script %}` rencontré alors qu'un bloc était déjà ouvert.
    /// Même heuristique de récupération que `NestedIfNotSupported` :
    /// l'ouverture imbriquée est ignorée, l'état courant est préservé.
    /// Pas de champs (contrairement à `NestedIfNotSupported`) : `script`/
    /// `endscript` ne portent aucune donnée propre, à la différence d'`if`.
    NestedScriptNotSupported,
    /// Fin de l'AST atteinte alors qu'un bloc `script` était encore ouvert.
    UnclosedScript,
}

/// Parcourt l'AST et valide la machine à états des blocs conditionnels ET
/// des blocs de capture de scripts.
///
/// # Deux FSM indépendantes, jamais couplées
///
/// `{% if %}`/`{% endif %}` et `{% script %}`/`{% endscript %}` sont
/// structurellement identiques (marqueur de bloc, scalaire unique, pas de
/// pile, imbrication interdite) mais orthogonaux par le fond : l'un gate
/// un rendu RUNTIME (dépend de la ligne affichée), l'autre délimite une
/// région connue intégralement à la COMPILATION. Cette fonction ne les
/// fait jamais interagir — un `{% script %}` ouvert à l'intérieur d'un
/// `{% if %}` ouvert n'est PAS une erreur ICI (les deux FSM sont juste
/// indépendamment satisfaites) ; le rejet de ce cas précis est la
/// responsabilité de `hoist_and_dedupe_scripts` (une préoccupation de
/// hoisting, pas de forme d'AST — `validate_ast` reste borné à UNE seule
/// question par paire de marqueurs : est-elle bien équilibrée ?).
///
/// # FSM (`if`)
/// ```text
/// État : None | Some((entity, field))
///
/// None  + IfBool      → Some(entity, field)         [transition normale]
/// None  + EndIf       → None  + push UnexpectedEndIf [erreur, état inchangé]
/// Some  + IfBool      → Some  + push Nested          [erreur, état inchangé]
/// Some  + EndIf       → None                         [fermeture normale]
/// EOF   + Some(e, f)  → push UnclosedIf(e, f)        [erreur de parité]
/// ```
///
/// # FSM (`script`) — même forme, aucun champ à mémoriser
/// ```text
/// État : false | true
///
/// false + ScriptStart → true  + push UnexpectedEndScript si déjà ouvert
/// false + ScriptEnd   → false + push UnexpectedEndScript [erreur, état inchangé]
/// true  + ScriptStart → true  + push NestedScriptNotSupported [erreur, état inchangé]
/// true  + ScriptEnd   → false                        [fermeture normale]
/// EOF   + true        → push UnclosedScript           [erreur de parité]
/// ```
///
/// `*     + Static/Field/Include/Asset → état inchangé` pour les deux FSM
/// (neutre).
///
/// # Garantie de terminaison
/// Parcours linéaire de longueur `tokens.len()` : O(n), pas de récursion.
///
/// # Allocation
/// `Vec::new()` n'alloue pas avant le premier `push` :
/// un AST valide produit `Ok(())` sans allocation heap.
pub fn validate_ast<'src>(tokens: &[FlatPageToken<'src>]) -> Result<(), Vec<SemanticError<'src>>> {
    let mut errors: Vec<SemanticError<'src>> = Vec::new();

    // `None`              : pas de bloc conditionnel ouvert.
    // `Some((e, f))`      : dans un bloc `{% if e.f %}`, en attente de `{% endif %}`.
    let mut current_open_if: Option<(&'src str, &'src str)> = None;
    // État de la seconde FSM, entièrement indépendant de `current_open_if`
    // — pas de champ à mémoriser, `script`/`endscript` ne portent aucune
    // donnée propre.
    let mut current_open_script = false;

    for token in tokens {
        // `match *token` : FlatPageToken est Copy (Phase 1.1).
        // Donne des bindings `entity: &'src str` directs, sans double indirection.
        match *token {
            FlatPageToken::IfBool { entity, field } => match current_open_if {
                None => {
                    current_open_if = Some((entity, field));
                }
                Some(_) => {
                    // Imbrication interdite.
                    // Heuristique : l'ouverture imbriquée est ignorée.
                    // L'état reste sur le bloc externe : le prochain EndIf
                    // fermera correctement ce bloc plutôt que de le laisser ouvert.
                    errors.push(SemanticError::NestedIfNotSupported {
                        nested_entity: entity,
                        nested_field: field,
                    });
                }
            },

            FlatPageToken::EndIf => match current_open_if {
                Some(_) => {
                    // Fermeture normale.
                    current_open_if = None;
                }
                None => {
                    // EndIf sans bloc ouvert.
                    // L'état reste à None : les tokens suivants sont analysés
                    // comme s'ils étaient au niveau racine.
                    errors.push(SemanticError::UnexpectedEndIf);
                }
            },

            // Symétrique exact de IfBool/EndIf ci-dessus, FSM séparée.
            FlatPageToken::ScriptStart => {
                if current_open_script {
                    errors.push(SemanticError::NestedScriptNotSupported);
                } else {
                    current_open_script = true;
                }
            }

            FlatPageToken::ScriptEnd => {
                if current_open_script {
                    current_open_script = false;
                } else {
                    errors.push(SemanticError::UnexpectedEndScript);
                }
            }

            // Static, Field, StaticInclude, AssetRef : aucun effet sur les FSM.
            // ModulesPlaceholder : jamais produit à ce stade (injecté par
            // build.rs après validate_ast, même ordre que ScriptStart/
            // ScriptEnd hissés par hoist_and_dedupe_scripts) — présent ici
            // uniquement pour l'exhaustivité du match, pas parce que ce
            // point est atteignable en pratique.
            FlatPageToken::Static(_)
            | FlatPageToken::Field { .. }
            | FlatPageToken::StaticInclude { .. }
            | FlatPageToken::AssetRef(_)
            | FlatPageToken::ModulesPlaceholder => {}
        }
    }

    // Contrôle de parité final.
    // Si un bloc est resté ouvert, l'erreur est enregistrée après le parcours,
    // ce qui garantit que toutes les erreurs intra-parcours sont déjà dans `errors`.
    if let Some((entity, field)) = current_open_if {
        errors.push(SemanticError::UnclosedIf { entity, field });
    }
    if current_open_script {
        errors.push(SemanticError::UnclosedScript);
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

// =============================================================================
// Tests — Phase 1.4
// =============================================================================

#[cfg(test)]
mod tests_phase_1_4 {
    use super::{FlatPageToken, SemanticError, validate_ast};

    /// Jalon Vert — séquence valide : deux blocs if séquentiels non imbriqués.
    ///
    /// Vérifie que la FSM revient bien à l'état None après chaque EndIf,
    /// et que le second IfBool ne déclenche pas de NestedIfNotSupported.
    #[test]
    fn test_semantic_valid() {
        let tokens: &[FlatPageToken<'_>] = &[
            FlatPageToken::Static("avant"),
            FlatPageToken::IfBool {
                entity: "user",
                field: "active",
            },
            FlatPageToken::Field {
                entity: "user",
                field: "name",
            },
            FlatPageToken::EndIf,
            FlatPageToken::Static("entre"),
            FlatPageToken::IfBool {
                entity: "user",
                field: "admin",
            },
            FlatPageToken::Static("accès restreint"),
            FlatPageToken::EndIf,
            FlatPageToken::Static("après"),
        ];

        assert_eq!(validate_ast(tokens), Ok(()));
    }

    /// Jalon Vert — séquence invalide : 3 erreurs distinctes accumulées.
    ///
    /// Séquence construite pour produire exactement, dans l'ordre :
    ///   1. `UnexpectedEndIf`              — EndIf avant tout IfBool
    ///   2. `NestedIfNotSupported`         — IfBool dans un IfBool
    ///   3. `UnclosedIf { "user", "premium" }` — EOF avec bloc ouvert
    ///
    /// Trace de la FSM :
    ///   EndIf                           : état=None  → erreur [1], état reste None
    ///   IfBool { user, active }         : état=None  → état = Some(user, active)
    ///   IfBool { user, admin }          : état=Some  → erreur [2], état reste Some(user, active)
    ///   EndIf                           : état=Some  → état = None  [ferme le bloc externe]
    ///   IfBool { user, premium }        : état=None  → état = Some(user, premium)
    ///   EOF                             : état=Some  → erreur [3]
    #[test]
    fn test_semantic_errors() {
        let tokens: &[FlatPageToken<'_>] = &[
            // [1] EndIf orphelin
            FlatPageToken::EndIf,
            // Ouverture d'un bloc externe
            FlatPageToken::IfBool {
                entity: "user",
                field: "active",
            },
            // [2] Imbrication interdite — l'externe reste actif
            FlatPageToken::IfBool {
                entity: "user",
                field: "admin",
            },
            // Ferme le bloc externe (l'imbriqué a été ignoré)
            FlatPageToken::EndIf,
            // [3] Bloc non fermé à l'EOF
            FlatPageToken::IfBool {
                entity: "user",
                field: "premium",
            },
        ];

        let expected = vec![
            SemanticError::UnexpectedEndIf,
            SemanticError::NestedIfNotSupported {
                nested_entity: "user",
                nested_field: "admin",
            },
            SemanticError::UnclosedIf {
                entity: "user",
                field: "premium",
            },
        ];

        assert_eq!(validate_ast(tokens), Err(expected));
    }

    /// Cas limite : AST vide. Aucun token, aucune erreur.
    #[test]
    fn test_semantic_empty_ast() {
        assert_eq!(validate_ast(&[]), Ok(()));
    }
}

// =============================================================================
// Phase 2.1 — AOT Capacity Planner & I/O Resolver
// =============================================================================
//
// Responsabilité unique : résoudre les inclusions externes, muter l'AST en place,
// et calculer PAGE_STATIC_CAP (total_static_bytes).
//
// Frontières strictes :
//   - Mutation en place de &mut [FlatPageToken<'src>] : zéro nouvel arbre.
//   - Fail-slow : toutes les erreurs I/O sont accumulées avant de retourner Err.
//   - `get_file_size` est injecté : la phase est testable sans I/O réel.
//   - Field / IfBool / EndIf ne contribuent pas à PAGE_STATIC_CAP :
//     leur coût mémoire est runtime-dépendant (Phase 2.2).

/// Métriques calculées lors de la résolution AOT de l'AST.
///
/// `total_static_bytes` devient la constante `PAGE_STATIC_CAP` à la génération
/// de code. Elle représente la borne exacte des octets statiques, connue
/// analytiquement avant toute exécution du moteur de rendu.
#[derive(Debug, PartialEq, Eq)]
pub struct TemplateMetrics {
    /// Somme exacte en octets de tous les `Static` et `StaticInclude` résolus.
    pub total_static_bytes: usize,
    /// Somme des pires cas d'affichage de tous les champs Field du template.
    /// Fixed-length : FieldKind::max_display_width().
    /// Varlena : VarlenField::max_escaped_len() (facteur escape × max_len).
    pub total_dynamic_bytes: usize,
    /// Nombre de fichiers externes inclus résolus avec succès.
    pub include_count: usize,
}

/// Résultat de la résolution d'un `{% asset key %}` par la closure injectée
/// depuis `build.rs`. Pas un `Option<usize>` : le cas d'échec porte sa
/// propre suggestion diagnostique, calculée par l'appelant — seul à
/// posséder le registre d'assets (`fragment-forge` reste sans I/O, sans
/// connaissance du manifeste, invariant inchangé depuis la doc de
/// `resolve_asset_len` ci-dessous).
///
/// Test de substitution (retirer la fonctionnalité de suggestion) :
/// `Found(usize)` reste `Found(usize)` quoi qu'il arrive au diagnostic —
/// contrairement à un `Result<usize, String>` calqué sur `get_file_size`,
/// où le canal de succès aurait dû redevenir `Option<usize>` si un jour la
/// suggestion disparaissait. Le canal de succès n'est pas couplé au canal
/// diagnostique.
#[derive(Debug, PartialEq, Eq)]
pub enum AssetLookup {
    /// Clé trouvée dans le manifeste — longueur en octets de l'URL
    /// publique versionnée finale (voir doc de `resolve_asset_len`).
    Found(usize),
    /// Clé absente du manifeste. `suggestion`, si présente, est un message
    /// déjà formaté par l'appelant (casse différente ou plus proche
    /// voisin par distance d'édition) — `fragment-forge` ne la recalcule
    /// jamais, il n'a pas accès aux clés candidates pour le faire.
    NotFound { suggestion: Option<String> },
}

/// Erreur de résolution I/O produite par `resolve_and_measure`.
///
/// `path` emprunte `'src` depuis le token AST : zéro allocation pour
/// l'identifiant du fichier manquant. `details` est alloué : le message
/// d'erreur OS doit être formaté en `String` (build-time uniquement).
#[derive(Debug, PartialEq, Eq)]
pub enum ResolverError<'src> {
    /// Fichier inclus introuvable ou illisible.
    IoError { path: &'src str, details: String },
    /// Token Field ou IfBool référençant un champ absent du schéma.
    /// Erreur AOT fatale : cargo:error dans build.rs.
    UnknownField { entity: &'src str, field: &'src str },
    /// Champ varlena référencé par le template mais sans borne connue
    /// (`VarlenField.max_len == None`) — ADR-007, disjoncteur Hot/Cold/Erreur.
    ///
    /// Distinct de `UnknownField` : le champ existe bien dans le schéma
    /// PostgreSQL (visible dans `SchemaIndex.varlena`), mais aucune contrainte
    /// exploitable (VARCHAR(N) ou CHECK reconnu) ne borne sa longueur. Un champ
    /// non référencé avec la même absence de borne ne produit jamais cette
    /// erreur — il reste Cold, invisible au calcul de capacité.
    UnboundedField { entity: &'src str, field: &'src str },
    /// `{% asset key %}` référençant une clé absente du manifeste d'assets
    /// produit par `marius-assets` — spec §9 et §11 : `AssetNotFound` est un
    /// échec fatal de compilation, jamais un repli ou une résolution
    /// dynamique différée au runtime.
    ///
    /// `suggestion` transporte, sans le recalculer, le diagnostic déjà
    /// produit par `AssetLookup::NotFound` — `None` signifie qu'aucun
    /// candidat plausible n'a été trouvé dans le manifeste, pas qu'aucune
    /// tentative n'a eu lieu.
    AssetNotFound {
        key: &'src str,
        suggestion: Option<String>,
    },
}

/// Résout les inclusions, mute l'AST en place, calcule les métriques statiques.
///
/// # Stratégie fail-slow
/// Toutes les erreurs I/O sont accumulées avant de retourner `Err`.
/// Un build référençant N fichiers manquants remonte N erreurs en une seule passe.
///
/// # Mutation en place
/// `StaticInclude::len` passe de `0` (valeur provisoire Phase 1.3) à la taille
/// réelle — sans créer de nouvel arbre, sans déplacer l'AST.
///
/// # Allocation conditionnelle
/// `Vec::new()` n'alloue pas de bloc heap avant le premier `push`.
/// Un projet sans fichier manquant traverse cette fonction sans allocation.
///
/// # Résolution des assets
/// `resolve_asset_len` : injectée par `build.rs` après lecture du manifeste
/// d'assets — jamais d'I/O ici. Retourne la longueur en octets de l'URL
/// publique versionnée finale (celle qui sera effectivement écrite par
/// `generate_aot_snippet`), pas la taille du fichier lui-même : c'est ce
/// nombre d'octets, et lui seul, qui contribue à `PAGE_STATIC_CAP`.
/// `AssetLookup::NotFound` signifie clé absente du manifeste →
/// `ResolverError::AssetNotFound`, jamais une longueur par défaut devinée
/// — la suggestion diagnostique éventuelle est transportée telle quelle,
/// `fragment-forge` ne la recalcule jamais (il n'a pas les clés
/// candidates pour le faire, seul `build.rs` les possède).
pub fn resolve_and_measure<'src>(
    tokens: &mut [FlatPageToken<'src>],
    schema: &SchemaIndex<'_>,
    get_file_size: impl Fn(&str) -> Result<usize, String>,
    resolve_asset_len: impl Fn(&str) -> AssetLookup,
    // Contribution en octets de ModulesPlaceholder, calculée intégralement
    // par `build.rs` (voir doc du variant) — pire cas : somme des snippets
    // de TOUTES les capacités actives, jamais une hypothèse d'exclusivité
    // mutuelle entre bits du bitset `js_deps`. `0` pour tout appelant dont
    // le flux ne contient jamais ce token (Mode Fragment isolé,
    // STATIC_PAGES, tests sans capacité) — valeur alors simplement inerte.
    modules_static_bytes: usize,
) -> Result<TemplateMetrics, Vec<ResolverError<'src>>> {
    let mut metrics = TemplateMetrics {
        total_static_bytes: 0,
        total_dynamic_bytes: 0,
        include_count: 0,
    };
    let mut errors: Vec<ResolverError<'src>> = Vec::new();

    for token in tokens.iter_mut() {
        match token {
            // Octets HTML connus statiquement : contribution directe.
            FlatPageToken::Static(s) => {
                metrics.total_static_bytes += s.len();
            }

            // Inclusion externe : résolution I/O et mutation en place.
            FlatPageToken::StaticInclude {
                rel_from_manifest,
                len,
                ..
            } => {
                let path = *rel_from_manifest;
                match get_file_size(path) {
                    Ok(size) => {
                        *len = size;
                        metrics.total_static_bytes += size;
                        metrics.include_count += 1;
                    }
                    Err(details) => {
                        errors.push(ResolverError::IoError { path, details });
                    }
                }
            }

            // Champ dynamique : disjoncteur Hot / Cold / Erreur (ADR-007).
            //
            // Table de vérité (un champ varlena est visité ici uniquement s'il
            // est référencé par l'AST — un champ jamais référencé n'entre jamais
            // dans cette branche, il reste Cold par construction, sans code dédié) :
            //
            //   référencé + fixed-length            → Hot, max_display_width()
            //   référencé + varlena, max_len=Some(n) → Hot, max_escaped_len()
            //   référencé + varlena, max_len=None    → Erreur, UnboundedField
            //   absent du schéma (ni fixed ni varlena) → Erreur, UnknownField
            FlatPageToken::Field { entity, field } => {
                if let Some(f) = schema.find_fixed(field) {
                    metrics.total_dynamic_bytes += f.kind.max_display_width();
                } else if let Some(v) = schema.find_varlena(field) {
                    match v.max_escaped_len() {
                        Some(n) => metrics.total_dynamic_bytes += n,
                        None => errors.push(ResolverError::UnboundedField { entity, field }),
                    }
                } else {
                    errors.push(ResolverError::UnknownField { entity, field });
                }
            }

            // Bloc conditionnel : validation schéma uniquement.
            // Le champ sert de condition booléenne, il n'est pas affiché —
            // pas de contribution à total_dynamic_bytes.
            FlatPageToken::IfBool { entity, field } => {
                if schema.find_fixed(field).is_none() {
                    errors.push(ResolverError::UnknownField { entity, field });
                }
            }

            // EndIf : aucun effet sur les métriques.
            FlatPageToken::EndIf => {}

            // ScriptStart/ScriptEnd : marqueurs purs, aucune contribution
            // propre — comme EndIf. Le contenu CAPTURÉ entre les deux (ses
            // propres tokens Static/AssetRef) est mesuré normalement par
            // ses propres tokens, que la capture ait lieu ou non en aval
            // (hoisting Page vs passthrough Fragment isolé, décidé par
            // build.rs) — cette fonction n'a pas à le savoir.
            FlatPageToken::ScriptStart | FlatPageToken::ScriptEnd => {}

            // Asset : longueur de l'URL résolue, jamais celle du fichier
            // source — même famille que Static/StaticInclude (contribution
            // directe à total_static_bytes), aucune mutation en place (voir
            // doc de la variante : pas de champ provisoire à patcher ici).
            FlatPageToken::AssetRef(key) => match resolve_asset_len(key) {
                AssetLookup::Found(len) => metrics.total_static_bytes += len,
                AssetLookup::NotFound { suggestion } => {
                    errors.push(ResolverError::AssetNotFound { key, suggestion });
                }
            },

            // Contribution directe, valeur fournie par l'appelant — même
            // famille que Static/AssetRef, aucune mutation en place (le
            // token ne porte aucun champ propre à patcher).
            FlatPageToken::ModulesPlaceholder => {
                metrics.total_static_bytes += modules_static_bytes;
            }
        }
    }

    if errors.is_empty() {
        Ok(metrics)
    } else {
        Err(errors)
    }
}

// =============================================================================
// Tests — Phase 2.1
// =============================================================================

#[cfg(test)]
mod tests_phase_2_1 {
    use super::{
        AssetLookup, EscapePolicy, FieldKind, FieldSpec, FlatPageToken, ResolverError, SchemaIndex,
        TemplateMetrics, VarlenField, resolve_and_measure,
    };

    /// Construit un StaticInclude avec len = 0 (valeur provisoire Phase 1.3).
    /// Les deux paths sont identiques : l'orchestrateur n'a pas encore calculé
    /// le chemin relatif au manifest.
    fn make_include(path: &str) -> FlatPageToken<'_> {
        FlatPageToken::StaticInclude {
            original_path: path,
            rel_from_manifest: path,
            len: 0,
        }
    }

    /// Vérifie le chemin heureux : mutation en place + métriques correctes.
    ///
    /// total_static_bytes = 6 (Static "<html>") + 10 (StaticInclude "a.html") = 16.
    #[test]
    fn test_resolve_success() {
        let mut tokens = vec![FlatPageToken::Static("<html>"), make_include("a.html")];

        let schema = SchemaIndex {
            fixed: &[],
            varlena: &[],
        };
        let result = resolve_and_measure(
            &mut tokens,
            &schema,
            |path| match path {
                "a.html" => Ok(10),
                other => Err(format!("unknown : {other}")),
            },
            |_| unreachable!("aucun AssetRef dans ce test"),
            0, // aucune capacité js_deps dans cette fixture
        );

        // Métriques correctes.
        assert_eq!(
            result,
            Ok(TemplateMetrics {
                total_static_bytes: 16,
                total_dynamic_bytes: 0,
                include_count: 1
            }),
        );

        // Preuve de mutation en place : len vaut 10, pas 0.
        // Les slices &'src str sont inchangées — seul le scalaire `len` a évolué.
        match &tokens[1] {
            FlatPageToken::StaticInclude {
                len,
                original_path,
                rel_from_manifest,
            } => {
                assert_eq!(*len, 10, "len doit être muté de 0 à 10");
                assert_eq!(*original_path, "a.html", "original_path inchangé");
                assert_eq!(*rel_from_manifest, "a.html", "rel_from_manifest inchangé");
            }
            _ => panic!("tokens[1] doit être un StaticInclude"),
        }
    }

    /// Vérifie l'accumulation fail-slow des erreurs I/O.
    ///
    /// "a.html" est résolu avec succès (mutation effective à len=10).
    /// "missing.html" échoue (IoError accumulé).
    /// Le retour est Err même si une résolution partielle a eu lieu :
    /// la capacité PAGE_STATIC_CAP ne peut pas être garantie si un fichier
    /// inclus est absent.
    ///
    /// Note : la mutation de "a.html" est vérifiable même en cas d'Err —
    /// preuve que le parcours ne s'est pas arrêté à la première erreur.
    #[test]
    fn test_resolve_partial_error() {
        let mut tokens = vec![
            FlatPageToken::Static("<html>"),
            make_include("a.html"),
            make_include("missing.html"),
        ];

        let schema = SchemaIndex {
            fixed: &[],
            varlena: &[],
        };
        let result = resolve_and_measure(
            &mut tokens,
            &schema,
            |path| match path {
                "a.html" => Ok(10),
                other => Err(format!("introuvable : {other}")),
            },
            |_| unreachable!("aucun AssetRef dans ce test"),
            0, // aucune capacité js_deps dans cette fixture
        );

        // Exactement 1 erreur accumulée.
        let errors = result.expect_err("doit retourner Err pour un fichier manquant");
        assert_eq!(errors.len(), 1, "exactement 1 IoError attendu");

        match &errors[0] {
            ResolverError::IoError { path, details } => {
                assert_eq!(*path, "missing.html");
                assert!(
                    details.contains("missing.html"),
                    "details doit identifier le fichier : {details:?}",
                );
            }
            ResolverError::UnknownField { entity, field } => {
                panic!("UnknownField inattendu dans ce test : {entity}.{field}");
            }
            ResolverError::UnboundedField { entity, field } => {
                panic!("UnboundedField inattendu dans ce test : {entity}.{field}");
            }
            ResolverError::AssetNotFound { key, .. } => {
                panic!("AssetNotFound inattendu dans ce test : {key}");
            }
        }

        // Invariant fail-slow : "a.html" a bien été muté malgré l'erreur suivante.
        match &tokens[1] {
            FlatPageToken::StaticInclude { len, .. } => {
                assert_eq!(
                    *len, 10,
                    "la mutation de a.html doit survivre à l'erreur partielle"
                );
            }
            _ => panic!("tokens[1] doit rester un StaticInclude"),
        }
    }

    /// Cas limite : AST sans inclusion.
    /// `get_file_size` ne doit jamais être appelé — vérifié par `unreachable!`.
    /// total_static_bytes = 3 ("<p>") + 4 ("</p>") = 7.
    /// Field contribue désormais à total_dynamic_bytes (Phase 3 — SchemaIndex).
    #[test]
    fn test_resolve_no_includes() {
        let mut tokens = vec![
            FlatPageToken::Static("<p>"),
            FlatPageToken::Field {
                entity: "user",
                field: "name",
            },
            FlatPageToken::Static("</p>"),
        ];

        let fixed = vec![FieldSpec {
            name: "name".to_string(),
            kind: FieldKind::I32,
            attnum: 1,
        }];
        let schema = SchemaIndex {
            fixed: &fixed,
            varlena: &[],
        };

        assert_eq!(
            resolve_and_measure(
                &mut tokens,
                &schema,
                |_| unreachable!("get_file_size ne doit pas être appelé sans StaticInclude"),
                |_| unreachable!("aucun AssetRef dans ce test"),
                0, // aucune capacité js_deps dans cette fixture
            ),
            Ok(TemplateMetrics {
                total_static_bytes: 7,
                total_dynamic_bytes: FieldKind::I32.max_display_width(),
                include_count: 0,
            }),
        );
    }

    // =========================================================================
    // Disjoncteur Hot / Cold / Erreur — ADR-007
    //
    // Table de vérité complète d'un champ varlena, les trois lignes possibles.
    // Zéro dépendance PostgreSQL : SchemaIndex est construit à la main, comme
    // tous les autres tests de ce module. L'invariant central de l'ADR — "un
    // champ non borné référencé par une projection provoque un échec explicite
    // de résolution" — est une propriété pure de resolve_and_measure, qui ne
    // dépend en rien de la fidélité de l'introspection PostgreSQL elle-même
    // (fetch_varlena_cols, hors périmètre de ces trois tests).
    // =========================================================================

    fn unbounded_field(name: &str) -> VarlenField {
        VarlenField {
            name: name.to_string(),
            // Provenance non pertinente pour ces tests (Hot/Cold/Erreur, pas
            // qualification SQL) — placeholder neutre, jamais lu par
            // resolve_and_measure ni par les assertions de ce module.
            ref_schema: "test".to_string(),
            ref_table: "joined".to_string(),
            max_len: None,
            escape_policy: EscapePolicy::Escaped,
            is_segment: false,
            nullable: true,
            max_escaped_len_override: None,
        }
    }

    fn bounded_field(name: &str, max_len: usize) -> VarlenField {
        VarlenField {
            name: name.to_string(),
            ref_schema: "test".to_string(),
            ref_table: "joined".to_string(),
            max_len: Some(max_len),
            escape_policy: EscapePolicy::Escaped,
            is_segment: false,
            nullable: true,
            max_escaped_len_override: None,
        }
    }

    /// Ligne 1 de la table de vérité : champ non borné, RÉFÉRENCÉ par l'AST.
    /// → Err([UnboundedField]). C'est l'invariant central de l'ADR-007 :
    /// un champ sans borne connue ne peut jamais contribuer silencieusement
    /// à total_dynamic_bytes — la compilation doit échouer explicitement.
    #[test]
    fn unbounded_field_referenced_fails_resolution() {
        let mut tokens = vec![FlatPageToken::Field {
            entity: "record",
            field: "description",
        }];
        let varlena = vec![unbounded_field("description")];
        let schema = SchemaIndex {
            fixed: &[],
            varlena: &varlena,
        };

        let result = resolve_and_measure(
            &mut tokens,
            &schema,
            |_| unreachable!("aucun StaticInclude dans ce test"),
            |_| unreachable!("aucun AssetRef dans ce test"),
            0, // aucune capacité js_deps dans cette fixture
        );

        assert_eq!(
            result,
            Err(vec![ResolverError::UnboundedField {
                entity: "record",
                field: "description"
            }]),
        );
    }

    /// Ligne 2 de la table de vérité : champ borné, RÉFÉRENCÉ par l'AST.
    /// → Ok, contribue normalement à total_dynamic_bytes via max_escaped_len().
    /// Comportement Hot — inchangé depuis avant ADR-007, vérifié explicitement
    /// pour garantir qu'il n'a pas régressé avec le passage à Option<usize>.
    #[test]
    fn bounded_field_referenced_contributes_normally() {
        let mut tokens = vec![FlatPageToken::Field {
            entity: "record",
            field: "headline",
        }];
        let varlena = vec![bounded_field("headline", 100)];
        let schema = SchemaIndex {
            fixed: &[],
            varlena: &varlena,
        };

        let metrics = resolve_and_measure(
            &mut tokens,
            &schema,
            |_| unreachable!("aucun StaticInclude dans ce test"),
            |_| unreachable!("aucun AssetRef dans ce test"),
            0, // aucune capacité js_deps dans cette fixture
        )
        .expect("champ borné référencé : résolution attendue en succès");

        assert_eq!(
            metrics.total_dynamic_bytes,
            100 * VarlenField::HTML_ESCAPE_FACTOR
        );
        assert_eq!(metrics.total_static_bytes, 0);
    }

    /// Ligne 3 de la table de vérité : champ non borné, JAMAIS référencé.
    /// → Ok, comportement Cold. Le champ existe dans SchemaIndex.varlena
    /// (visible, comme le produit désormais fetch_varlena_cols depuis ADR-007 —
    /// il n'est plus exclu du Vec en amont) mais n'apparaît dans aucune erreur
    /// ni dans total_dynamic_bytes, précisément parce que l'AST ne le mentionne
    /// jamais. Seule la conjonction "non borné + référencé" déclenche l'erreur —
    /// "non borné" seul ne suffit jamais.
    #[test]
    fn unbounded_field_not_referenced_is_cold() {
        let mut tokens = vec![FlatPageToken::Static("<article></article>")];
        let varlena = vec![unbounded_field("description")];
        let schema = SchemaIndex {
            fixed: &[],
            varlena: &varlena,
        };

        let metrics = resolve_and_measure(
            &mut tokens,
            &schema,
            |_| unreachable!("aucun StaticInclude dans ce test"),
            |_| unreachable!("aucun AssetRef dans ce test"),
            0, // aucune capacité js_deps dans cette fixture
        )
        .expect("champ Cold non référencé : résolution attendue en succès");

        assert_eq!(
            metrics.total_dynamic_bytes, 0,
            "champ Cold ne doit jamais contribuer"
        );
        assert_eq!(metrics.total_static_bytes, 19); // "<article></article>".len()
    }

    // =========================================================================
    // `{% asset key %}` — AssetLookup / ResolverError::AssetNotFound.
    //
    // Ces trois tests documentent un invariant précis, pas seulement le
    // chemin heureux : `fragment-forge` transporte la `suggestion` fournie
    // par la closure `resolve_asset_len` TELLE QUELLE — il ne la calcule
    // jamais, ne la reformate jamais, ne la remplace jamais par `None` par
    // défaut. Le calcul réel (casse insensible, Levenshtein) vit côté
    // `build.rs` (`resolve_asset_lookup`/`suggest_asset_key`), hors de
    // portée de ce crate — ces tests n'exercent donc délibérément que le
    // passage à travers `resolve_and_measure`, pas l'algorithme de
    // suggestion lui-même (qui n'existe pas ici).
    // =========================================================================

    /// Chemin heureux : clé résolue, contribue à `total_static_bytes` au
    /// même titre qu'un `Static`/`StaticInclude` — jamais à
    /// `total_dynamic_bytes` (spec §9 : une URL versionnée est un segment
    /// figé au moment de la génération, pas une valeur runtime).
    #[test]
    fn asset_ref_found_contributes_static_bytes() {
        let mut tokens = vec![
            FlatPageToken::Static("<link href=\""),
            FlatPageToken::AssetRef("main.css"),
            FlatPageToken::Static("\">"),
        ];
        let schema = SchemaIndex {
            fixed: &[],
            varlena: &[],
        };

        let metrics = resolve_and_measure(
            &mut tokens,
            &schema,
            |_| unreachable!("aucun StaticInclude dans ce test"),
            |key| {
                assert_eq!(key, "main.css");
                AssetLookup::Found(20) // longueur de l'URL versionnée, pas du fichier
            },
            0, // aucune capacité js_deps dans cette fixture
        )
        .expect("clé présente : résolution attendue en succès");

        assert_eq!(metrics.total_static_bytes, 12 + 20 + 2);
        assert_eq!(
            metrics.total_dynamic_bytes, 0,
            "un asset résolu ne doit jamais contribuer à total_dynamic_bytes"
        );
    }

    /// Clé absente AVEC suggestion : `ResolverError::AssetNotFound` doit
    /// porter la `suggestion` fournie par la closure mot pour mot — aucune
    /// transformation, aucun recalcul. C'est le contrat qui permet à
    /// `build.rs` de calculer le diagnostic (il a les clés candidates,
    /// `fragment-forge` ne les a jamais).
    #[test]
    fn asset_not_found_carries_supplied_suggestion_unchanged() {
        let mut tokens = vec![FlatPageToken::AssetRef("util.svg")];
        let schema = SchemaIndex {
            fixed: &[],
            varlena: &[],
        };

        let result = resolve_and_measure(
            &mut tokens,
            &schema,
            |_| unreachable!("aucun StaticInclude dans ce test"),
            |key| {
                assert_eq!(key, "util.svg");
                AssetLookup::NotFound {
                    suggestion: Some("vouliez-vous dire 'utils.svg' ?".to_string()),
                }
            },
            0, // aucune capacité js_deps dans cette fixture
        );

        let errors = result.expect_err("clé absente : Err attendu");
        assert_eq!(errors.len(), 1);
        match &errors[0] {
            ResolverError::AssetNotFound { key, suggestion } => {
                assert_eq!(*key, "util.svg");
                assert_eq!(
                    suggestion.as_deref(),
                    Some("vouliez-vous dire 'utils.svg' ?"),
                    "la suggestion doit traverser resolve_and_measure sans altération"
                );
            }
            other => panic!("AssetNotFound attendu, obtenu : {other:?}"),
        }
    }

    /// Clé absente SANS suggestion plausible : `suggestion` doit rester
    /// `None`, jamais remplacé par un texte générique fabriqué par
    /// `fragment-forge` — ce cas (aucun candidat proche dans le manifeste,
    /// ex. `silos/195v.svg`) doit rester silencieusement `None` jusqu'au
    /// site d'affichage, qui décide seul comment le présenter.
    #[test]
    fn asset_not_found_without_suggestion_stays_none() {
        let mut tokens = vec![FlatPageToken::AssetRef("silos/195v.svg")];
        let schema = SchemaIndex {
            fixed: &[],
            varlena: &[],
        };

        let result = resolve_and_measure(
            &mut tokens,
            &schema,
            |_| unreachable!("aucun StaticInclude dans ce test"),
            |_| AssetLookup::NotFound { suggestion: None },
            0, // aucune capacité js_deps dans cette fixture
        );

        let errors = result.expect_err("clé absente : Err attendu");
        match &errors[0] {
            ResolverError::AssetNotFound { key, suggestion } => {
                assert_eq!(*key, "silos/195v.svg");
                assert_eq!(
                    *suggestion, None,
                    "aucun candidat plausible : None attendu, pas un texte générique"
                );
            }
            other => panic!("AssetNotFound attendu, obtenu : {other:?}"),
        }
    }
}

// =============================================================================
// Phase 2.2 — Générateur AOT (Transpileur)
// =============================================================================
//
// Responsabilité unique : transpiler &[FlatPageToken<'src>] → String de code Rust.
//
// Frontières strictes :
//   - Aucune validation sémantique ici. L'AST est supposé correct (Phases 1.3+1.4).
//   - L'indentation est plate (2 niveaux max) : garanti par l'invariant Phase 1.4.
//   - Le code généré est autonome : `buf`, les variables d'entité et leurs champs
//     sont supposés dans le scope de la fonction encapsulante (build.rs).
//   - `{:?}` sur &str délègue l'échappement au Debug de Rust.
//     Zéro escaper maison. Résultat : un littéral Rust syntaxiquement valide.
//
// Invariant de pré-allocation (DOD) :
//   La première instruction du snippet est toujours `buf.reserve(N)`.
//   N = metrics.total_static_bytes (mesuré exactement en Phase 2.1).
//   Cette instruction garantit que le vecteur sous-jacent au `buf: &mut String`
//   du runtime ne réalloue jamais pour les octets HTML statiques.

/// Transpile l'AST en un bloc d'instructions Rust natif.
///
/// N'émet PAS `buf.reserve()` — c'est la responsabilité de l'orchestrateur
/// qui référence PAGE_TOTAL_CAP (calculé depuis les métriques).
///
/// Délègue le choix d'émission à SchemaIndex :
///   Field fixe   → write_fmt (pas d'allocation).
///   Field varlena → html_escape via ref locale as_deref().
///   IfBool        → `if record.field != 0` (u8 dans StorageRow, pas bool).
///
/// # Résolution des assets
/// `resolve_asset_url` : supposée infaillible à ce stade — toute clé absente
/// du manifeste a déjà fait échouer la compilation via
/// `ResolverError::AssetNotFound` dans `resolve_and_measure`, appelé
/// obligatoirement avant cette fonction (même précédent que `StaticInclude`,
/// dont l'existence est vérifiée par `get_file_size` avant que
/// `include_str!` ne soit émis ici). Un panic ici signale une violation de
/// cet ordonnancement par l'appelant (`build.rs`), jamais une clé
/// utilisateur invalide.
///
/// `'r` distinct de `'src` et de la lifetime (anonyme, par argument) de
/// `key` dans la closure : sans ce paramètre nommé, `impl Fn(&str) -> &str`
/// s'élide en `for<'a> Fn(&'a str) -> &'a str` (HRTB — la sortie liée à
/// l'entrée). Une closure réelle capturant `&HashMap` (build.rs) renvoie un
/// emprunt sur la durée de vie de la map, jamais sur celle de `key` : elle
/// ne peut satisfaire cette borne que si la map vit `'static`, ce qui n'est
/// pas le cas. `'r` découple la sortie de l'entrée et se résout, à l'appel,
/// sur la durée de vie réelle capturée par la closure.
pub fn generate_aot_snippet<'src, 'r>(
    tokens: &[FlatPageToken<'src>],
    schema: &SchemaIndex<'_>,
    resolve_asset_url: impl Fn(&str) -> &'r str,
    // Code Rust déjà assemblé par `build.rs` pour ModulesPlaceholder — une
    // ligne `if record.js_deps & BIT != 0 { buf.push_str(...); }` par
    // capacité active, chaîne vide si aucune. Inséré verbatim (ce N'EST PAS
    // un littéral à échapper comme AssetRef/StaticInclude : c'est déjà du
    // code source, pas une valeur) — voir doc du variant.
    modules_snippet: &str,
) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(25 + tokens.len() * 60);

    // ── Déclarations de références varlena ────────────────────────────────────
    let mut varlena_seen: Vec<&str> = tokens
        .iter()
        .filter_map(|t| match t {
            FlatPageToken::Field { field, .. } if schema.find_varlena(field).is_some() => {
                Some(*field)
            }
            _ => None,
        })
        .collect();
    varlena_seen.sort_unstable();
    varlena_seen.dedup();
    for name in &varlena_seen {
        writeln!(
            out,
            "let {name}_ref: Option<&str> = varlena.{name}.as_deref();"
        )
        .unwrap();
    }

    let mut indent: &str = "";

    for token in tokens {
        match token {
            FlatPageToken::Static(s) => {
                if s.len() == 1 {
                    let c = s.chars().next().unwrap();
                    writeln!(out, "{}buf.push({:?});", indent, c).unwrap();
                } else {
                    writeln!(out, "{}buf.push_str({:?});", indent, s).unwrap();
                }
            }

            FlatPageToken::Field { field, .. } => {
                if let Some(v) = schema.find_varlena(field) {
                    // CONTRAT-implementation-varlena-raw.md, Étape 4 : match
                    // exhaustif sur EscapePolicy — Raw ne passe JAMAIS par
                    // marius_html_escape (contenu HTML déjà constitué, à
                    // injecter tel quel) ; Escaped et PreEscaped conservent le
                    // comportement existant (l'échappement runtime ne dépend
                    // que du contenu réel étant du texte, pas de la capacité
                    // déclarée — seul PreEscaped change le facteur de
                    // capacité, jamais le comportement d'échappement lui-même).
                    match v.escape_policy {
                        EscapePolicy::Raw => {
                            writeln!(
                                out,
                                "{}if let Some(s) = {field}_ref {{ buf.push_str(s); }}",
                                indent,
                            )
                            .unwrap();
                        }
                        EscapePolicy::Escaped | EscapePolicy::PreEscaped => {
                            writeln!(
                                out,
                                "{}if let Some(s) = {field}_ref {{ marius_html_escape(s, buf); }}",
                                indent,
                            )
                            .unwrap();
                        }
                    }
                } else {
                    writeln!(
                        out,
                        r#"{}::std::fmt::Write::write_fmt(buf, format_args!("{{}}", record.{field})).ok();"#,
                        indent,
                    ).unwrap();
                }
            }

            FlatPageToken::IfBool { field, .. } => {
                // u8 dans StorageRow (bytemuck::Pod interdit bool).
                writeln!(out, "{}if record.{field} != 0 {{", indent).unwrap();
                indent = "    ";
            }

            FlatPageToken::EndIf => {
                indent = "";
                out.push_str("}\n");
            }

            // ScriptStart/ScriptEnd : jamais émis eux-mêmes (No-Op pur,
            // mission §2, cible Fragment isolé) — le contenu capturé entre
            // les deux continue d'être émis normalement à sa position
            // d'origine par ses propres tokens si `hoist_and_dedupe_scripts`
            // n'a pas tourné en amont (build.rs, jamais ici).
            FlatPageToken::ScriptStart | FlatPageToken::ScriptEnd => {}

            FlatPageToken::StaticInclude {
                rel_from_manifest, ..
            } => {
                writeln!(
                    out,
                    "{}buf.push_str(include_str!({:?}));",
                    indent, rel_from_manifest,
                )
                .unwrap();
            }

            // Asset : URL versionnée gravée en dur, exactement comme un
            // segment Static — zéro indirection, zéro allocation au runtime
            // (spec §9). Pas d'`include_str!` : ce n'est pas un contenu de
            // fichier à inliner, c'est une chaîne déjà connue au moment de
            // la génération.
            FlatPageToken::AssetRef(key) => {
                let url = resolve_asset_url(key);
                writeln!(out, "{}buf.push_str({:?});", indent, url).unwrap();
            }

            // Insertion verbatim — `modules_snippet` est déjà du code Rust
            // complet (0 à N lignes `if record.js_deps & BIT != 0 { ... }`),
            // jamais une valeur à formater/échapper comme les autres
            // variantes de cette fonction.
            FlatPageToken::ModulesPlaceholder => {
                out.push_str(modules_snippet);
            }
        }
    }

    out
}

/// Génère le corps de `render_segments()` pour un composant portant au moins
/// un champ `is_segment == true` — CONTRAT-implementation-projection-
/// segmentee.md, Étape 5. Appelée par `build.rs` à la place de
/// `generate_aot_snippet` uniquement quand `varlena.iter().any(|v|
/// v.is_segment)` — jamais les deux pour le même composant.
///
/// ── Algorithme (arbitré en session, 23/07/2026) ───────────────────────────
///
/// Identique à `generate_aot_snippet` pour tout token qui n'est pas un champ
/// `is_segment` — même émission `buf.push_str`/`marius_html_escape`/etc.,
/// dans `buf`. La seule différence : un champ `is_segment` clôt le « run »
/// `Buffered` courant (`segments.push(Segment::Buffered { start, end })`),
/// pousse sa valeur comme `Segment::Borrowed` autonome (jamais concaténée
/// dans `buf`), puis rouvre un nouveau run pour ce qui suit.
///
/// `seg_start` est une variable Rust générée, déclarée une seule fois en tête
/// de fonction (`let mut seg_start: usize = buf.len();` — vaut 0 en pratique,
/// `buf` arrivant vide par contrat, mais recalculé dynamiquement plutôt que
/// supposé pour rester robuste à toute évolution future du contrat), puis
/// réassignée (jamais re-`let`) à chaque réouverture de run — y compris à
/// l'intérieur d'un bloc `{% if %}` généré : la réassignation à l'intérieur
/// d'un bloc conditionnel est correcte par construction, puisque le bloc
/// entier est sauté à l'exécution si la condition est fausse, laissant
/// `seg_start` intact avec sa valeur d'avant le bloc — le run englobant se
/// poursuit alors sans discontinuité, exactement comme si le champ segmenté
/// n'existait pas pour cet enregistrement.
///
/// Ce raisonnement a été vérifié à la main sur le cas d'un champ segmenté
/// unique à l'intérieur d'un `{% if %}` avant d'écrire cette fonction — les
/// deux branches d'exécution (condition vraie/fausse) produisent un état de
/// `segments` cohérent dans les deux cas.
pub fn generate_segmented_snippet<'src, 'r>(
    tokens: &[FlatPageToken<'src>],
    schema: &SchemaIndex<'_>,
    resolve_asset_url: impl Fn(&str) -> &'r str,
    // Voir doc du paramètre homonyme de `generate_aot_snippet` — même
    // contrat : code Rust déjà assemblé, inséré verbatim, jamais une valeur
    // à formater.
    modules_snippet: &str,
) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(25 + tokens.len() * 60);

    // ── Déclarations de références varlena — identique à generate_aot_snippet ──
    let mut varlena_seen: Vec<&str> = tokens
        .iter()
        .filter_map(|t| match t {
            FlatPageToken::Field { field, .. } if schema.find_varlena(field).is_some() => {
                Some(*field)
            }
            _ => None,
        })
        .collect();
    varlena_seen.sort_unstable();
    varlena_seen.dedup();
    for name in &varlena_seen {
        writeln!(
            out,
            "let {name}_ref: Option<&str> = varlena.{name}.as_deref();"
        )
        .unwrap();
    }

    // Ouverture du premier run — toujours à la racine de la fonction, jamais
    // à l'intérieur d'un bloc généré (les tokens IfBool ne peuvent apparaître
    // qu'après ce point dans la boucle ci-dessous).
    writeln!(out, "let mut seg_start: usize = buf.len();").unwrap();

    let mut indent = String::new();

    for token in tokens {
        match token {
            FlatPageToken::Static(s) => {
                if s.len() == 1 {
                    let c = s.chars().next().unwrap();
                    writeln!(out, "{indent}buf.push({c:?});").unwrap();
                } else {
                    writeln!(out, "{indent}buf.push_str({s:?});").unwrap();
                }
            }

            FlatPageToken::Field { field, .. } => {
                if let Some(v) = schema.find_varlena(field) {
                    if v.is_segment {
                        // Clôture du run courant — toujours valide même si
                        // ce token est le tout premier de la fonction
                        // (seg_start == 0 == buf.len() à cet instant, run
                        // vide légitimement poussé, aucun octet perdu).
                        writeln!(
                            out,
                            "{indent}segments.push(marius_projection::Segment::Buffered {{ start: seg_start, end: buf.len() }});"
                        )
                        .unwrap();
                        writeln!(
                            out,
                            "{indent}if let Some(s) = {field}_ref {{ segments.push(marius_projection::Segment::Borrowed(s)); }}"
                        )
                        .unwrap();
                        writeln!(out, "{indent}seg_start = buf.len();").unwrap();
                    } else {
                        match v.escape_policy {
                            EscapePolicy::Raw => {
                                writeln!(
                                    out,
                                    "{indent}if let Some(s) = {field}_ref {{ buf.push_str(s); }}"
                                )
                                .unwrap();
                            }
                            EscapePolicy::Escaped | EscapePolicy::PreEscaped => {
                                writeln!(
                                    out,
                                    "{indent}if let Some(s) = {field}_ref {{ marius_html_escape(s, buf); }}"
                                )
                                .unwrap();
                            }
                        }
                    }
                } else {
                    writeln!(
                        out,
                        r#"{indent}::std::fmt::Write::write_fmt(buf, format_args!("{{}}", record.{field})).ok();"#,
                    )
                    .unwrap();
                }
            }

            FlatPageToken::IfBool { field, .. } => {
                writeln!(out, "{indent}if record.{field} != 0 {{").unwrap();
                indent.push_str("    ");
            }

            FlatPageToken::EndIf => {
                let new_len = indent.len().saturating_sub(4);
                indent.truncate(new_len);
                writeln!(out, "{indent}}}").unwrap();
            }

            FlatPageToken::ScriptStart | FlatPageToken::ScriptEnd => {}

            FlatPageToken::StaticInclude {
                rel_from_manifest, ..
            } => {
                writeln!(
                    out,
                    "{indent}buf.push_str(include_str!({rel_from_manifest:?}));",
                )
                .unwrap();
            }

            FlatPageToken::AssetRef(key) => {
                let url = resolve_asset_url(key);
                writeln!(out, "{indent}buf.push_str({url:?});").unwrap();
            }

            // Insertion verbatim, même contrat que generate_aot_snippet —
            // `modules_snippet` n'est jamais imbriqué dans un run segmenté
            // (le marqueur vit dans <head>, hors de tout champ is_segment).
            FlatPageToken::ModulesPlaceholder => {
                out.push_str(modules_snippet);
            }
        }
    }

    // Clôture du dernier run — toujours émise, qu'il y ait eu 0 ou N champs
    // segmentés (si 0, ce run couvre tout buf, comportement équivalent à
    // l'implémentation par défaut de render_segments — mais cette fonction
    // n'est de toute façon appelée par build.rs que si has_segment == true).
    writeln!(
        out,
        "segments.push(marius_projection::Segment::Buffered {{ start: seg_start, end: buf.len() }});"
    )
    .unwrap();

    out
}

// =============================================================================
// Hoisting + déduplication des `{% script %}...{% endscript %}` — passe de
// capture de bloc (révision de session : remplace l'ancienne approche par
// clé `AssetRef(*.js)` seule).
//
// Rappel de la correction architecturale déjà actée (inchangée) : cette
// passe tourne UNE FOIS à la compilation (`build.rs`), jamais par requête —
// aucun `HashSet` ne survit au-delà de la génération du fichier `.rs`
// source.
//
// Changement de grammaire (session dédiée) : le développeur écrit
// désormais son tag `<script>` complet, avec les attributs de SON choix
// (`defer`, `id`, `integrity`...), entouré de `{% script %}`/
// `{% endscript %}` — ce crate n'a et n'aura jamais de connaissance de la
// grammaire HTML `<script>` elle-même (pas de couplage présentation/
// compilateur). La région entière est capturée verbatim comme une
// sous-séquence opaque de `FlatPageToken`, jamais reconstruite depuis du
// texte.
//
// Modèle physique du moteur (précisé en session) : un fichier `.marius`
// peut être compilé comme composant d'une Page complète (layout avec
// `<head>`) ou comme Partial autonome (Fragment isolé, résolu directement
// par `resolve_template`, sans layout). Le hoisting n'est donc PAS une
// propriété de l'AST — c'est une propriété de la CIBLE de compilation :
//   - Cible Page   : `build.rs` appelle cette passe, extrait et dédup-
//     lique les blocs, les réinjecte au marqueur `<head>`.
//   - Cible Fragment isolé : `build.rs` n'appelle jamais cette passe.
//     `ScriptStart`/`ScriptEnd` traversent `generate_aot_snippet` comme de
//     purs No-Op (voir leurs bras dédiés plus haut) — le contenu capturé
//     reste alors inline, à sa position d'origine, exactement comme si les
//     deux marqueurs n'existaient pas.
// Cette distinction ne vit jamais dans ce crate : `hoist_and_dedupe_scripts`
// n'est JAMAIS appelée pour une cible Fragment isolé, décision prise
// entièrement par l'orchestrateur.
// =============================================================================

// =============================================================================
// Scan statique des marqueurs `class` — HANDOFF-js-deps-capacites-frontend-v2.md,
// addendum « MARIUS_MODULES agrège deux sources ».
// =============================================================================

/// Extrait l'ensemble des tokens `class` présents dans le HTML **statique**
/// d'un flux déjà abaissé (post-`lower()` — parent+enfant fusionnés, avant
/// splice de `ModulesPlaceholder`).
///
/// Scanne EXCLUSIVEMENT `FlatPageToken::Static` — jamais l'intérieur d'un
/// `{{ champ }}`/`{% if %}` : une classe qui dépend d'une donnée runtime
/// (`class="{{ some_class }}"`) n'est structurellement pas détectable ici,
/// et ne doit jamais l'être — c'est précisément la frontière entre ce que
/// `fragment-forge` peut savoir à la compilation et ce que seul
/// `content.compute_js_deps` (SQL, à l'écriture) peut savoir.
///
/// Contrat lexical partagé avec `content.compute_js_deps`
/// (`db/05_content/02_systems.sql`) — même DÉFINITION du marqueur (token
/// exact d'un attribut `class`, délimiteur `'` ou `"`, tokenisation sur les
/// espaces), deux implémentations INDÉPENDANTES, aucune ne dérive de
/// l'autre. Jamais une sous-chaîne, jamais un attribut `data-*`.
pub fn extract_static_class_tokens<'src>(
    tokens: &[FlatPageToken<'src>],
) -> std::collections::HashSet<String> {
    use std::sync::OnceLock;

    // Ancrage de frontière sur le nom d'attribut ((?:^|[\s<])class=) — même
    // principe que la regex PL/pgSQL, transposé : évite de matcher un
    // attribut dont le nom se TERMINE par "class" (ex: "data-class=").
    static CLASS_ATTR_RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = CLASS_ATTR_RE.get_or_init(|| {
        regex::Regex::new(r#"(?:^|[\s<])class=(?:"([^"]*)"|'([^']*)')"#)
            .expect("regex statique — motif fixe, jamais construit depuis une entrée externe")
    });

    let mut out = std::collections::HashSet::new();
    for token in tokens {
        if let FlatPageToken::Static(s) = token {
            for caps in re.captures_iter(s) {
                let value = caps
                    .get(1)
                    .or_else(|| caps.get(2))
                    .map(|m| m.as_str())
                    .unwrap_or("");
                for tok in value.split_whitespace() {
                    if !tok.is_empty() {
                        out.insert(tok.to_string());
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests_extract_static_class_tokens {
    use super::FlatPageToken;
    use super::extract_static_class_tokens;

    #[test]
    fn finds_double_quoted_class() {
        let tokens = vec![FlatPageToken::Static(r#"<pre class="add-line-marks">"#)];
        let found = extract_static_class_tokens(&tokens);
        assert!(found.contains("add-line-marks"));
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn finds_single_quoted_class() {
        let tokens = vec![FlatPageToken::Static("<div class='map'>")];
        let found = extract_static_class_tokens(&tokens);
        assert!(found.contains("map"));
    }

    #[test]
    fn splits_multiple_tokens_in_one_class_attr() {
        let tokens = vec![FlatPageToken::Static(
            r#"<div class="range range-multithumb extra">"#,
        )];
        let found = extract_static_class_tokens(&tokens);
        assert!(found.contains("range"));
        assert!(found.contains("range-multithumb"));
        assert!(found.contains("extra"));
        assert_eq!(found.len(), 3);
    }

    #[test]
    fn never_matches_attribute_ending_in_class() {
        // Ancrage de frontière : "data-class=" ne doit jamais être confondu
        // avec "class=" — même piège que la regex SQL doit éviter.
        let tokens = vec![FlatPageToken::Static(r#"<div data-class="not-a-marker">"#)];
        let found = extract_static_class_tokens(&tokens);
        assert!(found.is_empty());
    }

    #[test]
    fn ignores_non_static_tokens() {
        // Field/IfBool/etc. ne sont jamais scannés — seul le HTML
        // véritablement statique participe à cette détection.
        let tokens = vec![
            FlatPageToken::Field {
                entity: "record",
                field: "class",
            },
            FlatPageToken::Static(r#"<div class="map">"#),
        ];
        let found = extract_static_class_tokens(&tokens);
        assert_eq!(found.len(), 1);
        assert!(found.contains("map"));
    }

    #[test]
    fn empty_when_no_static_class_present() {
        let tokens = vec![FlatPageToken::Static("<div>sans classe ici</div>")];
        let found = extract_static_class_tokens(&tokens);
        assert!(found.is_empty());
    }

    #[test]
    fn scans_across_multiple_static_tokens() {
        let tokens = vec![
            FlatPageToken::Static(r#"<div class="map">"#),
            FlatPageToken::Static(r#"<pre class="add-line-marks">"#),
        ];
        let found = extract_static_class_tokens(&tokens);
        assert_eq!(found.len(), 2);
        assert!(found.contains("map"));
        assert!(found.contains("add-line-marks"));
    }
}

/// Erreur de la passe de hoisting/déduplication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoistError {
    /// Un bloc `{% script %}...{% endscript %}` trouvé À L'INTÉRIEUR d'un
    /// bloc `{% if %}...{% endif %}` ouvert — non supporté par cette passe
    /// (restriction explicitement validée en session : "l'arbre des
    /// dépendances doit rester prévisible à la compilation"). Son
    /// inclusion dépendrait d'une donnée RUNTIME (la ligne effectivement
    /// rendue), alors que cette passe s'exécute UNE FOIS pour tout le
    /// template, indépendamment des données — le hisser quand même le
    /// rendrait inconditionnel, un vrai bug de correction, pas une
    /// simplification acceptable.
    ConditionalScript,
    /// `{% endscript %}` sans `{% script %}` ouvert correspondant, ou fin
    /// de flux avec un bloc encore ouvert. Ne devrait structurellement
    /// jamais se produire si `validate_ast` a déjà validé le flux (sa
    /// propre FSM garantit cet équilibre) — cette fonction ne SUPPOSE pas
    /// cette précondition pour autant : elle reste défensive plutôt que de
    /// paniquer si elle est un jour appelée directement sur un flux non
    /// validé (tests compris, voir plus bas).
    UnbalancedScriptBlock,
}

impl std::fmt::Display for HoistError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HoistError::ConditionalScript => write!(
                f,
                "hoisting : un bloc {{% script %}}...{{% endscript %}} à l'intérieur d'un \
                 bloc {{% if %}} n'est pas supporté (portée conditionnelle non résolvable \
                 à la compilation) — voir doc de HoistError::ConditionalScript"
            ),
            HoistError::UnbalancedScriptBlock => write!(
                f,
                "hoisting : bloc {{% script %}}/{{% endscript %}} déséquilibré (devrait \
                 avoir été détecté par validate_ast en amont)"
            ),
        }
    }
}

impl std::error::Error for HoistError {}

/// Extrait, déduplique et retire du flux les blocs `{% script %}...
/// {% endscript %}` INCONDITIONNELS d'une page déjà linéarisée
/// (post-`link`/`lower`, post-`validate_ast`). La duplication vient de
/// `{% include %}` : chaque occurrence d'un fragment copie l'intégralité
/// de ses tokens dans le flux de la page, y compris ses éventuels blocs
/// `script` — sans cette passe, un fragment inclus trois fois produit
/// trois `<script>` identiques.
///
/// Déduplication par ÉGALITÉ DE CONTENU (comparaison structurelle de la
/// sous-séquence de tokens capturée), pas par une clé synthétique — deux
/// blocs capturés IDENTIQUES (mêmes tokens, dans le même ordre) sont
/// nécessairement la même répétition accidentelle d'un fragment inclus
/// plusieurs fois ; deux blocs qui DIFFÈRENT (attributs différents sur le
/// même asset, par exemple) sont considérés distincts et tous deux
/// conservés — jamais de suppression silencieuse d'un attribut que le
/// développeur a écrit explicitement sur l'une des deux occurrences.
/// Comparaison en O(n²) sur le nombre de blocs DISTINCTS déjà vus (une
/// poignée par page en pratique) : la structure de données la plus simple
/// suffit, pas la peine d'imposer `Hash` à `FlatPageToken` pour un
/// `HashSet` dont le gain serait invisible à cette échelle.
///
/// Retourne `(flux_sans_les_blocs_script, blocs_uniques_dans_l'ordre_de_
/// première_apparition)` — chaque bloc est une sous-séquence de tokens
/// verbatim (le tag `<script>` complet écrit par le développeur), prête à
/// être réinjectée telle quelle par `splice_hoisted_scripts`.
pub fn hoist_and_dedupe_scripts<'src>(
    tokens: Vec<FlatPageToken<'src>>,
) -> Result<(Vec<FlatPageToken<'src>>, Vec<Vec<FlatPageToken<'src>>>), HoistError> {
    let mut output = Vec::with_capacity(tokens.len());
    let mut captured_blocks: Vec<Vec<FlatPageToken<'src>>> = Vec::new();
    let mut if_depth: u32 = 0;

    let mut iter = tokens.into_iter();
    while let Some(token) = iter.next() {
        match token {
            FlatPageToken::IfBool { .. } => {
                if_depth += 1;
                output.push(token);
            }
            FlatPageToken::EndIf => {
                if_depth = if_depth.saturating_sub(1);
                output.push(token);
            }
            FlatPageToken::ScriptStart => {
                if if_depth > 0 {
                    return Err(HoistError::ConditionalScript);
                }

                let mut block: Vec<FlatPageToken<'src>> = Vec::new();
                loop {
                    match iter.next() {
                        Some(FlatPageToken::ScriptEnd) => break,
                        Some(inner) => block.push(inner),
                        None => return Err(HoistError::UnbalancedScriptBlock),
                    }
                }

                // Extraction : ni le marqueur, ni son contenu, ni un
                // doublon détecté ne rejoignent jamais `output` — "zéro
                // trace locale" (mission précédente §1), à l'échelle du
                // bloc entier cette fois, pas d'un seul token `AssetRef`.
                if !captured_blocks.iter().any(|seen| seen == &block) {
                    captured_blocks.push(block);
                }
            }
            FlatPageToken::ScriptEnd => {
                // Rencontré hors capture : aucun ScriptStart ouvert à ce
                // niveau n'a consommé ce token via la boucle interne
                // ci-dessus.
                return Err(HoistError::UnbalancedScriptBlock);
            }
            other => output.push(other),
        }
    }

    Ok((output, captured_blocks))
}

/// Réinjecte les blocs de scripts hissés à une position déjà déterminée
/// par l'appelant (`build.rs` — voir note d'intégration en tête de
/// section : cette fonction reste délibérément agnostique de la façon
/// dont cette position est repérée, pour ne pas exiger de modification de
/// l'AST gelé de ce crate).
///
/// Simple concaténation, dans l'ordre reçu (déjà déterministe : ordre de
/// première apparition, si issu de `hoist_and_dedupe_scripts`) — aucune
/// balise n'est SYNTHÉTISÉE ici : chaque bloc capturé contient déjà,
/// verbatim, le tag `<script>` complet écrit par le développeur
/// (attributs compris). Cette fonction assemble des blocs déjà résolus,
/// elle ne génère jamais de HTML elle-même.
///
/// `at_index` : position dans `tokens` où insérer le bloc assemblé — le
/// token initialement à cette position est décalé après, jamais écrasé.
pub fn splice_hoisted_scripts<'src>(
    mut tokens: Vec<FlatPageToken<'src>>,
    hoisted_blocks: &[Vec<FlatPageToken<'src>>],
    at_index: usize,
) -> Vec<FlatPageToken<'src>> {
    if hoisted_blocks.is_empty() {
        return tokens; // rien à hisser : flux inchangé.
    }

    let mut block: Vec<FlatPageToken<'src>> = Vec::new();
    for captured in hoisted_blocks {
        block.extend(captured.iter().copied());
    }

    let insert_at = at_index.min(tokens.len());
    let tail = tokens.split_off(insert_at);
    tokens.extend(block);
    tokens.extend(tail);
    tokens
}

// =============================================================================
// Tests — Hoisting + déduplication des scripts (capture de bloc).
// =============================================================================

#[cfg(test)]
mod tests_hoist_scripts {
    use super::{FlatPageToken, HoistError, hoist_and_dedupe_scripts, splice_hoisted_scripts};

    /// Reproduit exactement l'exemple `core.marius` de la mission : le
    /// MÊME bloc `<script src="{% asset map.js %}" type="module">
    /// </script>` écrit deux fois d'affilée.
    #[test]
    fn hoist_removes_block_entirely_and_dedupes_identical_repeats() {
        let tokens = vec![
            FlatPageToken::Static("<p>1</p>"),
            FlatPageToken::ScriptStart,
            FlatPageToken::Static("<script src=\""),
            FlatPageToken::AssetRef("map.js"),
            FlatPageToken::Static("\" type=\"module\"></script>"),
            FlatPageToken::ScriptEnd,
            FlatPageToken::ScriptStart,
            FlatPageToken::Static("<script src=\""),
            FlatPageToken::AssetRef("map.js"),
            FlatPageToken::Static("\" type=\"module\"></script>"),
            FlatPageToken::ScriptEnd,
            FlatPageToken::Static("<p>2</p>"),
        ];

        let (output, blocks) = hoist_and_dedupe_scripts(tokens).unwrap();

        // Une seule occurrence malgré deux blocs sources identiques.
        assert_eq!(blocks.len(), 1);
        assert_eq!(
            blocks[0],
            vec![
                FlatPageToken::Static("<script src=\""),
                FlatPageToken::AssetRef("map.js"),
                FlatPageToken::Static("\" type=\"module\"></script>"),
            ]
        );
        // Zéro trace locale : ni marqueurs, ni contenu, ni doublon.
        assert_eq!(
            output,
            vec![
                FlatPageToken::Static("<p>1</p>"),
                FlatPageToken::Static("<p>2</p>"),
            ]
        );
    }

    /// Deux blocs référençant le MÊME asset mais avec des attributs
    /// DIFFÉRENTS (ex. un `id` sur l'un, pas sur l'autre) ne doivent
    /// jamais fusionner — l'un des deux attributs serait silencieusement
    /// perdu si la déduplication se faisait par clé d'asset plutôt que par
    /// égalité de contenu complet.
    #[test]
    fn hoist_keeps_distinct_blocks_on_same_asset_as_separate_entries() {
        let tokens = vec![
            FlatPageToken::ScriptStart,
            FlatPageToken::Static("<script src=\""),
            FlatPageToken::AssetRef("map.js"),
            FlatPageToken::Static("\" type=\"module\"></script>"),
            FlatPageToken::ScriptEnd,
            FlatPageToken::ScriptStart,
            FlatPageToken::Static("<script src=\""),
            FlatPageToken::AssetRef("map.js"),
            FlatPageToken::Static("\" type=\"module\" id=\"map-loader\"></script>"),
            FlatPageToken::ScriptEnd,
        ];

        let (_, blocks) = hoist_and_dedupe_scripts(tokens).unwrap();

        assert_eq!(
            blocks.len(),
            2,
            "deux tags distincts, aucun attribut ne doit disparaître"
        );
    }

    #[test]
    fn hoist_preserves_first_occurrence_order_across_distinct_blocks() {
        let tokens = vec![
            FlatPageToken::ScriptStart,
            FlatPageToken::AssetRef("b.js"),
            FlatPageToken::ScriptEnd,
            FlatPageToken::ScriptStart,
            FlatPageToken::AssetRef("a.js"),
            FlatPageToken::ScriptEnd,
        ];

        let (_, blocks) = hoist_and_dedupe_scripts(tokens).unwrap();

        assert_eq!(
            blocks,
            vec![
                vec![FlatPageToken::AssetRef("b.js")],
                vec![FlatPageToken::AssetRef("a.js")]
            ]
        );
    }

    #[test]
    fn hoist_unconditional_script_outside_any_if_is_captured() {
        let tokens = vec![
            FlatPageToken::IfBool {
                entity: "record",
                field: "is_published",
            },
            FlatPageToken::Static("<p>x</p>"),
            FlatPageToken::EndIf,
            FlatPageToken::ScriptStart,
            FlatPageToken::AssetRef("nav.js"),
            FlatPageToken::ScriptEnd,
        ];

        let (output, blocks) = hoist_and_dedupe_scripts(tokens).unwrap();

        assert_eq!(blocks, vec![vec![FlatPageToken::AssetRef("nav.js")]]);
        assert_eq!(
            output,
            vec![
                FlatPageToken::IfBool {
                    entity: "record",
                    field: "is_published"
                },
                FlatPageToken::Static("<p>x</p>"),
                FlatPageToken::EndIf,
            ]
        );
    }

    /// Restriction explicitement validée en session : un bloc `script` à
    /// l'intérieur d'un `if` doit échouer, jamais être hissé de façon
    /// inconditionnelle.
    #[test]
    fn hoist_conditional_script_is_a_hard_error() {
        let tokens = vec![
            FlatPageToken::IfBool {
                entity: "record",
                field: "is_published",
            },
            FlatPageToken::ScriptStart,
            FlatPageToken::AssetRef("extra.js"),
            FlatPageToken::ScriptEnd,
            FlatPageToken::EndIf,
        ];

        assert_eq!(
            hoist_and_dedupe_scripts(tokens),
            Err(HoistError::ConditionalScript)
        );
    }

    #[test]
    fn hoist_unterminated_script_block_is_an_error() {
        let tokens = vec![FlatPageToken::ScriptStart, FlatPageToken::AssetRef("x.js")];
        assert_eq!(
            hoist_and_dedupe_scripts(tokens),
            Err(HoistError::UnbalancedScriptBlock)
        );
    }

    #[test]
    fn hoist_end_script_without_start_is_an_error() {
        let tokens = vec![FlatPageToken::ScriptEnd];
        assert_eq!(
            hoist_and_dedupe_scripts(tokens),
            Err(HoistError::UnbalancedScriptBlock)
        );
    }

    #[test]
    fn splice_inserts_hoisted_blocks_verbatim_in_order() {
        let tokens = vec![
            FlatPageToken::Static("<head>"),
            FlatPageToken::Static("</head>"),
        ];
        let blocks = vec![
            vec![FlatPageToken::AssetRef("main.js")],
            vec![FlatPageToken::AssetRef("more.js")],
        ];

        let result = splice_hoisted_scripts(tokens, &blocks, 1);

        assert_eq!(
            result,
            vec![
                FlatPageToken::Static("<head>"),
                FlatPageToken::AssetRef("main.js"),
                FlatPageToken::AssetRef("more.js"),
                FlatPageToken::Static("</head>"),
            ]
        );
    }

    #[test]
    fn splice_with_no_hoisted_blocks_leaves_stream_unchanged() {
        let tokens = vec![FlatPageToken::Static("<head></head>")];
        let result = splice_hoisted_scripts(tokens.clone(), &[], 0);
        assert_eq!(result, tokens);
    }

    /// Bout-en-bout : hoist puis splice reproduit le scénario complet de
    /// la mission — fragment de nav inclus deux fois (contenu identique)
    /// et fragment "map" inclus deux fois également, un seul exemplaire
    /// de chacun dans le flux final, à l'emplacement du marqueur.
    #[test]
    fn hoist_then_splice_end_to_end() {
        let nav_block = vec![
            FlatPageToken::ScriptStart,
            FlatPageToken::Static("<script src=\""),
            FlatPageToken::AssetRef("nav.js"),
            FlatPageToken::Static("\" type=\"module\"></script>"),
            FlatPageToken::ScriptEnd,
        ];

        let mut tokens = vec![
            FlatPageToken::Static("<head>"),
            FlatPageToken::Static("</head>"),
        ];
        tokens.push(FlatPageToken::Static("<body>"));
        tokens.extend(nav_block.clone()); // 1ère inclusion du fragment de nav
        tokens.push(FlatPageToken::Static("<hr>"));
        tokens.extend(nav_block); // 2ème inclusion, contenu identique
        tokens.push(FlatPageToken::Static("</body>"));

        let (mut output, blocks) = hoist_and_dedupe_scripts(tokens).unwrap();
        assert_eq!(blocks.len(), 1); // dédupliqué malgré deux inclusions

        let head_close = output
            .iter()
            .position(|t| matches!(t, FlatPageToken::Static(s) if *s == "</head>"))
            .unwrap();
        output = splice_hoisted_scripts(output, &blocks, head_close);

        assert_eq!(
            output,
            vec![
                FlatPageToken::Static("<head>"),
                FlatPageToken::Static("<script src=\""),
                FlatPageToken::AssetRef("nav.js"),
                FlatPageToken::Static("\" type=\"module\"></script>"),
                FlatPageToken::Static("</head>"),
                FlatPageToken::Static("<body>"),
                FlatPageToken::Static("<hr>"),
                FlatPageToken::Static("</body>"),
            ]
        );
    }
}

// =============================================================================
// Tests — Phase 2.2
// =============================================================================

#[cfg(test)]
mod tests_phase_2_2 {
    use super::{
        EscapePolicy, FieldKind, FieldSpec, FlatPageToken, SchemaIndex, VarlenField,
        generate_aot_snippet, generate_segmented_snippet,
    };

    fn make_schema<'a>(fixed: &'a [FieldSpec], varlena: &'a [VarlenField]) -> SchemaIndex<'a> {
        SchemaIndex { fixed, varlena }
    }

    /// Snippet avec champ fixed (write_fmt) et champ varlena (html_escape).
    /// IfBool émet != 0 (u8 dans StorageRow).
    /// Aucun buf.reserve dans le snippet — c'est la responsabilité de l'orchestrateur.
    #[test]
    fn test_generate_aot_snippet_typed() {
        let fixed = vec![
            FieldSpec {
                name: "title".to_string(),
                kind: FieldKind::I32,
                attnum: 1,
            },
            FieldSpec {
                name: "is_published".to_string(),
                kind: FieldKind::Bool,
                attnum: 2,
            },
        ];
        let varlena = vec![VarlenField {
            name: "body".to_string(),
            // Provenance non pertinente ici — generate_aot_snippet ne lit
            // jamais ref_schema/ref_table (seulement .name, via find_varlena).
            ref_schema: "test_schema".to_string(),
            ref_table: "test_table".to_string(),
            max_len: Some(1000),
            escape_policy: EscapePolicy::Escaped,
            is_segment: false,
            nullable: true,
            max_escaped_len_override: None,
        }];
        let schema = make_schema(&fixed, &varlena);

        let tokens: &[FlatPageToken<'_>] = &[
            FlatPageToken::Static("<article>"),
            FlatPageToken::Field {
                entity: "record",
                field: "title",
            },
            FlatPageToken::Field {
                entity: "varlena",
                field: "body",
            },
            FlatPageToken::IfBool {
                entity: "record",
                field: "is_published",
            },
            FlatPageToken::Static("<span>publié</span>"),
            FlatPageToken::EndIf,
            FlatPageToken::StaticInclude {
                original_path: "...",
                rel_from_manifest: "frag.html",
                len: 42,
            },
        ];

        let got = generate_aot_snippet(
            tokens,
            &schema,
            |_| unreachable!("aucun AssetRef dans ce test"),
            "",
        );

        // Varlena ref déclarée en tête, triée.
        assert!(
            got.contains("let body_ref: Option<&str> = varlena.body.as_deref();"),
            "déclaration varlena absente:\n{got}"
        );
        // Fixed → write_fmt.
        assert!(
            got.contains(
                r#"::std::fmt::Write::write_fmt(buf, format_args!("{}", record.title)).ok();"#
            ),
            "write_fmt absent:\n{got}"
        );
        // Varlena → html_escape.
        assert!(
            got.contains("if let Some(s) = body_ref { marius_html_escape(s, buf); }"),
            "html_escape absent:\n{got}"
        );
        // IfBool → != 0 (u8).
        assert!(
            got.contains("if record.is_published != 0 {"),
            "condition u8 absente:\n{got}"
        );
        // StaticInclude.
        assert!(
            got.contains(r#"buf.push_str(include_str!("frag.html"));"#),
            "include_str absent:\n{got}"
        );
        // Pas de buf.reserve dans le snippet.
        assert!(
            !got.contains("buf.reserve"),
            "buf.reserve ne doit pas être dans le snippet:\n{got}"
        );
    }

    /// CONTRAT-implementation-varlena-raw.md, Étape 4 : un champ
    /// EscapePolicy::Raw produit `buf.push_str(s)` direct, JAMAIS
    /// `marius_html_escape` — HTML déjà constitué, injecté tel quel.
    #[test]
    fn test_generate_aot_snippet_raw_field_bypasses_html_escape() {
        let fixed: Vec<FieldSpec> = vec![];
        let varlena = vec![VarlenField {
            name: "content".to_string(),
            ref_schema: "content".to_string(),
            ref_table: "body".to_string(),
            max_len: Some(32_000),
            escape_policy: EscapePolicy::Raw,
            is_segment: false,
            nullable: true,
            max_escaped_len_override: None,
        }];
        let schema = make_schema(&fixed, &varlena);

        let tokens: &[FlatPageToken<'_>] = &[FlatPageToken::Field {
            entity: "varlena",
            field: "content",
        }];

        let got = generate_aot_snippet(
            tokens,
            &schema,
            |_| unreachable!("aucun AssetRef dans ce test"),
            "",
        );

        assert!(
            got.contains("let content_ref: Option<&str> = varlena.content.as_deref();"),
            "déclaration varlena absente:\n{got}"
        );
        assert!(
            got.contains("if let Some(s) = content_ref { buf.push_str(s); }"),
            "buf.push_str direct absent (Raw ne doit jamais échapper):\n{got}"
        );
        assert!(
            !got.contains("marius_html_escape"),
            "marius_html_escape ne doit JAMAIS apparaître pour un champ Raw:\n{got}"
        );
    }

    /// Snippet sans varlena : aucune déclaration de ref.
    #[test]
    fn test_generate_aot_snippet_no_varlena() {
        let fixed = vec![FieldSpec {
            name: "id".to_string(),
            kind: FieldKind::I64,
            attnum: 1,
        }];
        let schema = make_schema(&fixed, &[]);
        let tokens: &[FlatPageToken<'_>] = &[
            FlatPageToken::Static("<p>"),
            FlatPageToken::Field {
                entity: "record",
                field: "id",
            },
            FlatPageToken::Static("</p>"),
        ];
        let got = generate_aot_snippet(
            tokens,
            &schema,
            |_| unreachable!("aucun AssetRef dans ce test"),
            "",
        );
        assert!(
            !got.contains("_ref"),
            "pas de déclaration ref sans varlena:\n{got}"
        );
        assert!(got.contains("record.id"), "champ id absent:\n{got}");
        assert!(
            !got.contains("buf.reserve"),
            "buf.reserve hors scope:\n{got}"
        );
    }

    // ── generate_segmented_snippet — CONTRAT-implementation-projection-segmentee.md, Étape 5 ──

    fn segment_field(name: &str) -> VarlenField {
        VarlenField {
            name: name.to_string(),
            ref_schema: "content".to_string(),
            ref_table: "body".to_string(),
            max_len: Some(32_000),
            escape_policy: EscapePolicy::Raw,
            is_segment: true,
            nullable: true,
            max_escaped_len_override: None,
        }
    }

    #[test]
    fn generate_segmented_snippet_splits_around_segment_field() {
        let fixed: Vec<FieldSpec> = vec![];
        let varlena = vec![segment_field("content")];
        let schema = make_schema(&fixed, &varlena);

        let tokens: &[FlatPageToken<'_>] = &[
            FlatPageToken::Static("<article>"),
            FlatPageToken::Field {
                entity: "varlena",
                field: "content",
            },
            FlatPageToken::Static("</article>"),
        ];

        let got = generate_segmented_snippet(
            tokens,
            &schema,
            |_| unreachable!("aucun AssetRef dans ce test"),
            "",
        );

        assert!(
            got.contains("let mut seg_start: usize = buf.len();"),
            "déclaration seg_start absente:\n{got}"
        );
        assert!(
            got.contains("segments.push(marius_projection::Segment::Buffered"),
            "push Buffered absent:\n{got}"
        );
        assert!(
            got.contains(
                "if let Some(s) = content_ref { segments.push(marius_projection::Segment::Borrowed(s)); }"
            ),
            "push Borrowed absent ou mal formé:\n{got}"
        );
        assert!(
            !got.contains("marius_html_escape"),
            "un champ segmenté ne doit jamais passer par marius_html_escape:\n{got}"
        );
        // Deux runs Buffered : avant et après le champ segmenté.
        assert_eq!(
            got.matches("Segment::Buffered").count(),
            2,
            "deux runs Buffered attendus (avant/après le champ segmenté):\n{got}"
        );
    }

    #[test]
    fn generate_segmented_snippet_handles_segment_inside_if_block() {
        let fixed: Vec<FieldSpec> = vec![];
        let varlena = vec![segment_field("content")];
        let schema = make_schema(&fixed, &varlena);

        let tokens: &[FlatPageToken<'_>] = &[
            FlatPageToken::Static("<p>"),
            FlatPageToken::IfBool {
                entity: "record",
                field: "is_readable",
            },
            FlatPageToken::Field {
                entity: "varlena",
                field: "content",
            },
            FlatPageToken::EndIf,
            FlatPageToken::Static("</p>"),
        ];

        let got = generate_segmented_snippet(
            tokens,
            &schema,
            |_| unreachable!("aucun AssetRef dans ce test"),
            "",
        );

        // Le push Buffered/Borrowed à l'intérieur du if doit être indenté —
        // preuve qu'il est bien conditionnel, pas exécuté inconditionnellement.
        assert!(
            got.contains("    segments.push(marius_projection::Segment::Buffered"),
            "le push à l'intérieur du bloc if devrait être indenté :\n{got}"
        );
        assert!(
            got.contains("if record.is_readable != 0 {"),
            "bloc if absent:\n{got}"
        );
        // Le dernier push (clôture finale) est à l'indentation racine (pas de
        // préfixe 4-espaces), après la fermeture du bloc if.
        let last_push_line = got
            .lines()
            .filter(|l| l.trim_start().starts_with("segments.push"))
            .next_back()
            .expect("au moins un push attendu");
        assert!(
            !last_push_line.starts_with(' '),
            "le push final doit être à la racine, pas à l'intérieur du if:\n{got}"
        );
    }

    #[test]
    fn generate_segmented_snippet_final_close_always_emitted() {
        // Aucun champ segmenté référencé dans les tokens (cas dégénéré,
        // jamais déclenché en pratique par build.rs — has_segment serait
        // false — mais la fonction ne doit pas paniquer ni produire un état
        // incohérent si elle est appelée quand même).
        let fixed: Vec<FieldSpec> = vec![];
        let varlena: Vec<VarlenField> = vec![];
        let schema = make_schema(&fixed, &varlena);

        let tokens: &[FlatPageToken<'_>] = &[FlatPageToken::Static("<p>Rien à segmenter</p>")];

        let got = generate_segmented_snippet(
            tokens,
            &schema,
            |_| unreachable!("aucun AssetRef dans ce test"),
            "",
        );

        assert_eq!(
            got.matches("Segment::Buffered").count(),
            1,
            "un seul run Buffered attendu, couvrant tout buf:\n{got}"
        );
    }
}

// =============================================================================
// Phase 3.0 — Mode Page, brique structurelle (additif, non câblé)
// =============================================================================
//
// Portée de cette section : trois ajouts de données pures, zéro logique de
// résolution, zéro appel entrant depuis scan / parse_tokens / validate_ast /
// resolve_and_measure / generate_aot_snippet. Ces cinq fonctions restent
// gelées dans cette session — voir HANDOFF-mode-page-brique-structurelle.md.
//
// ─── Décision actée — Bool vs u8 (préalable bloquant du handoff) ─────────────
//
//   Tranché : u8-sentinelle, pas bool natif. `StorageRow` est `#[repr(C)]` et
//   contraint `bytemuck::Pod` ; `bool` n'est pas `Pod` (représentation non
//   garantie sur tous les bits), `u8` l'est. `FlatPageToken::IfBool` a déjà
//   acté ce choix pour le mode fragment (`generate_aot_snippet` émet
//   `if record.{field} != 0`). Le mode page hérite du même contrat : toute
//   condition portée par un futur token de bloc sera un `u8` testé `!= 0`,
//   jamais un `bool`. La spécification v1.1 §8 décrit le type DDL conceptuel
//   (BOOLEAN côté SQL) ; la représentation mémoire côté moteur reste u8.
//   Ce choix est documenté ici et non ré-ouvert au niveau du codegen.
//
// ─── Décision de câblage — pourquoi ces types ne vivent PAS dans FlatPageToken ─
//
//   Le handoff propose un mirror direct de IfBool/EndIf *dans* FlatPageToken.
//   Cette session ne le fait pas : FlatPageToken est un enum matché de façon
//   exhaustive (sans arm `_`) dans validate_ast, resolve_and_measure et
//   generate_aot_snippet — les trois fonctions explicitement gelées. Ajouter
//   une variante à FlatPageToken casse leur exhaustivité et force une édition
//   de ces fonctions pour recompiler, ce qui viole la contrainte de méthode.
//   Les nouveaux types ci-dessous sont donc des types frères, isomorphes à ce
//   qu'exigerait un mirror interne, mais hors de l'enum gelé. La fusion —
//   soit par variante additionnelle sur FlatPageToken, soit par un type de
//   token englobant paramétré sur les deux enums — est une décision de
//   câblage réservée à la session qui écrira le parseur mode page.

/// Marqueur de bloc `{% block %}` / `{% endblock %}` (Mode Page).
///
/// Isomorphe à `FlatPageToken::IfBool` / `EndIf` : ouverture nommée puis
/// fermeture, sans nom porté sur la fermeture (la FSM à un niveau d'état
/// suffit — cf. `current_open_if` dans `validate_ast`, dont le principe se
/// généralise directement à une paire `current_open_block: Option<&str>`
/// dans la validation sémantique mode page, à écrire dans une session
/// ultérieure).
///
/// ─── Invariant de platitude ───────────────────────────────────────────────
///
///   Aucune variante ici ne porte de `Vec<FlatPageToken>` ni de `Vec<Self>`
///   imbriqué. Le contenu d'un bloc reste une plage linéaire d'indices dans
///   l'AST plat existant (`Vec<FlatPageToken<'src>>`), bornée par la position
///   de `BlockOpen` et de `BlockEnd` correspondant. C'est le même principe
///   que la plage implicite entre `IfBool` et `EndIf` : aucun nouvel arbre,
///   aucune récursion de structure.
///
/// ─── Conditions embarquées : tranché "non" pour cette forme ────────────────
///
///   Le handoff pose la question : si `{% block %}` porte lui-même une
///   condition, sa forme dépend du choix bool/u8 ci-dessus. Cette session
///   tranche la question amont : `BlockOpen` ne porte PAS de condition.
///   Un bloc conditionnel s'exprime en composant un `FlatPageToken::IfBool`
///   à l'intérieur de la plage du bloc (ou en l'englobant), pas en dupliquant
///   la sémantique conditionnelle sur le marqueur de bloc lui-même. Séparation
///   stricte des responsabilités : `BlockOpen`/`BlockEnd` nomment une région
///   de fusion, `IfBool`/`EndIf` conditionnent un rendu — deux axes orthogonaux
///   qui ne doivent pas fusionner dans un seul token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageBlockToken<'src> {
    /// `{% block header %}` — ouverture nommée. `name` est le seul identifiant
    /// de correspondance parent/enfant : la fusion (session ultérieure)
    /// recherchera ce nom dans le template parent pour substituer la plage.
    BlockOpen { name: &'src str },

    /// `{% endblock %}` — fermeture symétrique. Pas de champ `name` : comme
    /// `EndIf`, la FSM de validation associe la fermeture au dernier bloc
    /// ouvert par position, pas par ré-affirmation du nom (moins de surface
    /// d'erreur utilisateur — un `{% endblock wrongname %}` n'existe pas
    /// dans cette forme, donc ne peut pas désynchroniser silencieusement).
    BlockEnd,
}

/// Handle opaque identifiant l'arène d'origine d'une plage de tokens.
///
/// `Copy`, sans lifetime, sans indirection — un `u32` nu. Choix DOD délibéré :
/// l'alternative (porter un `&'ast [FlatPageToken<'src>]` directement dans
/// `NamedBlockRange`) rendrait `ChildTemplateSpec` auto-référentiel vis-à-vis
/// de son propre `Vec<FlatPageToken<'src>>` — struct qui s'emprunte
/// elle-même, incompatible avec la construction en une passe et avec la
/// contrainte `Copy` recherchée pour ce type de handle. `TemplateId` reporte
/// la vérification d'arène sur la valeur elle-même plutôt que sur une
/// relation d'emprunt : une plage sait de quel template elle vient, sans
/// dépendre du contexte d'appel pour rester valide après une copie.
///
/// Assignation : responsabilité du Linker (session ultérieure), qui tiendra
/// l'arène (`Vec<Vec<FlatPageToken<'src>>>` ou équivalent) indexée par cet
/// id. Aucune hypothèse sur la forme de cette arène n'est prise ici — ce
/// type fixe uniquement sa fonction de tag d'origine.
///
/// Vérification : à l'exécution (assert au point de déréférencement dans le
/// Linker), pas à la compilation. Le borrow checker ne peut pas garantir
/// statiquement qu'un `TemplateId` correspond à l'arène qu'on lui présente
/// sans emprunt réel — c'est le tradeoff explicite du choix Index+Tag par
/// rapport à un slice emprunté. Acceptable tant qu'une seule passe de
/// linking traite les templates séquentiellement ; à réévaluer si une passe
/// future traite plusieurs arènes en parallèle dans le même scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TemplateId(pub u32);

/// Correspondance nom-de-bloc → plage de tokens, taguée par arène d'origine.
///
/// `start`/`end` sont des indices dans le `Vec<FlatPageToken<'src>>` identifié
/// par `template`, exclusifs des marqueurs `PageBlockToken` eux-mêmes : la
/// plage couvre le *contenu* du bloc, pas ses délimiteurs. Convention
/// `[start, end)` (`end` exclusif), cohérente avec les conventions de slice
/// Rust — la fusion consommera `&arena[range.template][range.start..range.end]`.
///
/// Le champ `template` élimine par construction la confusion d'arène : une
/// plage extraite d'un enfant A et appliquée par erreur au token-vec d'un
/// enfant B produit un id qui ne correspond pas à l'arène consultée — donc
/// une valeur détectable (assert du Linker), plutôt qu'un résultat
/// silencieusement incohérent (troncature ou contenu halluciné) que
/// produirait un couple `(usize, usize)` nu appliqué au mauvais `Vec`.
///
/// Type de données pur : aucune méthode de validation ou de fusion. La
/// construction de ces plages (parcours de l'AST enfant pour repérer les
/// paires BlockOpen/BlockEnd, assignation du `TemplateId` courant) est un
/// algorithme de la session parseur, pas de celle-ci.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NamedBlockRange<'src> {
    /// Nom déclaré par `{% block name %}`. Clé de correspondance avec le
    /// parent — deux blocs de même nom dans un même enfant sont une erreur
    /// de linking (`PageLinkError::OrphanBlock` ou variante dédiée future),
    /// pas une responsabilité de ce type.
    pub name: &'src str,
    /// Arène d'origine des indices `start`/`end` ci-dessous.
    pub template: TemplateId,
    /// Index de début de la plage de contenu (inclusif), dans l'AST référencé
    /// par `template`.
    pub start: usize,
    /// Index de fin de la plage de contenu (exclusif), dans l'AST référencé
    /// par `template`.
    pub end: usize,
}

/// Template enfant, forme pré-fusion.
///
/// Porte le chemin `extends` déclaré par `{% extends "parent.marius" %}` et
/// l'ensemble des blocs que l'enfant définit, sous forme de plages dans son
/// propre AST. Ne porte PAS l'AST lui-même (celui-ci vit déjà comme
/// `Vec<FlatPageToken<'src>>`, produit par le futur parseur mode page, non
/// dupliqué ici) — uniquement la métadonnée de structure que la fusion
/// consommera aux côtés de cet AST.
///
/// ─── `extends` comme champ, pas comme token : états illégaux
///     irreprésentables ────────────────────────────────────────────────────
///
///   `extends` est un champ obligatoire de la struct, pas une variante dans
///   un flux de tokens. Conséquence directe : on ne peut pas construire un
///   `ChildTemplateSpec` sans avoir déjà tranché la valeur d'`extends`, et on
///   ne peut pas en construire un qui en porterait deux ou zéro — les deux
///   états qu'un token `Extends` répété ou absent rendrait représentables
///   dans un `Vec<FlatPageToken>` classique. La contrainte "un seul extends,
///   en tête de template" est donc garantie par la forme du type au moment
///   de sa construction, et non vérifiée après coup par une variante
///   d'erreur dédiée (`ExtendsNotFirst` reste nécessaire, mais côté Parser :
///   c'est l'erreur levée en tentant de *construire* ce champ à partir d'un
///   flux qui contredit la grammaire, jamais une invariante à revérifier une
///   fois le type déjà construit). Ce choix explique pourquoi `blocks`, lui,
///   reste un `Vec` : c'est la partie réellement répétable de la grammaire —
///   zéro, un ou N blocs sont tous des états légitimes.
///
/// ─── Rôle dans le futur pipeline de fusion ───────────────────────────────
///
///   La fusion (Normalization, hors périmètre ici) parcourra l'AST du
///   parent, et à chaque `PageBlockToken::BlockOpen { name }` rencontré,
///   cherchera `name` dans `self.blocks`. Si trouvé : substitution de la
///   plage. Si absent : le bloc par défaut du parent est conservé
///   (comportement Jinja2-like standard, à confirmer/trancher explicitement
///   à l'écriture de la fusion — non tranché ici). Un bloc de l'enfant qui
///   ne correspond à aucun `BlockOpen` du parent est orphelin : c'est le
///   rôle prévu de `PageLinkError::OrphanBlock`.
///
/// ─── Allocation ───────────────────────────────────────────────────────────
///
///   `blocks` est un `Vec` : allocation build-time uniquement, comme tous
///   les AST de ce module. Structure jamais exposée au runtime du moteur.
#[derive(Debug, Clone)]
pub struct ChildTemplateSpec<'src> {
    /// Chemin déclaré par `{% extends %}`, tel qu'écrit dans le template —
    /// résolution en chemin manifeste (via `relative_path_for_include_str`,
    /// fonction existante et déjà réutilisable telle quelle) différée à la
    /// session de câblage. Voir doc de tête de struct : la présence et
    /// l'unicité de ce champ ne sont pas revérifiées à l'usage, elles sont
    /// garanties par la construction du type.
    pub extends: &'src str,
    /// Un élément par bloc défini dans cet enfant, dans l'ordre d'apparition
    /// dans l'AST enfant (pas trié par nom : l'ordre d'apparition n'a pas de
    /// signification pour la fusion, mais le préserver évite un tri
    /// superflu tant qu'aucun consommateur n'en a besoin).
    pub blocks: Vec<NamedBlockRange<'src>>,
}

// ─── Erreurs par phase (Action 2) ──────────────────────────────────────────
//
// Une phase ne peut pas retourner une erreur d'un domaine qu'elle ne connaît
// pas — le typage doit refléter le pipeline (Lowering) et non un panier
// d'erreurs partagé. Trois types, un par phase amont du pipeline de
// composition. Nommage : préfixe `Page` conservé (et non les noms nus
// `ParseError`/`LinkError`/`ValidationError`) pour deux raisons — éviter la
// collision de lecture avec `PageParseError` déjà existant et frozen (mode
// fragment, jamais concerné par extends/block), et éviter des noms de type
// pub génériques à un seul mot dans une bibliothèque qui exporte aussi
// `SemanticError`/`ResolverError` sous le même schéma de nommage préfixé. Si
// ce choix de nommage ne convient pas, il se renomme en un remplacement pur
// (aucune des variantes ni leur regroupement par phase n'en dépend).

/// Sortie complète du Parser Mode Page pour **un** fichier (Document 1 §2.2,
/// scaffoldée en Phase 4.6 quand `extends` devient calculable — cf. doc de
/// `parse_page_tokens`).
///
/// ─── Pourquoi `extends` n'est pas une variante de `PageSourceToken` ────────
///
/// Une déclaration `{% extends "path" %}` est une propriété du *fichier*
/// (une seule par fichier, position figée en tête), pas un élément d'un flux
/// homogène de tokens de contenu — la porter comme variante de
/// `PageSourceToken` obligerait tout consommateur de `tokens` (Document 2 :
/// `collect_blocks`, la Validation) à re-vérifier au runtime qu'elle
/// n'apparaît qu'en position 0, alors que ce champ séparé le rend
/// impossible par construction : `tokens` ne contient jamais de déclaration
/// `extends`, point final.
///
/// ─── Pas de `TemplateId` ni de `NamedBlockRange` résolus ───────────────────
///
/// L'assignation d'identité d'arène est une responsabilité de l'admission
/// en arène (Document 2, Phase 5.1) : un fichier parsé isolément n'a pas
/// encore d'arène à laquelle appartenir.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedPageTemplate<'src> {
    /// Chemin déclaré par `{% extends %}`, si présent en tête de fichier.
    /// `None` : ce fichier est un parent (aucun `extends` rencontré) — un
    /// fichier hors mode page n'atteint jamais cette structure, ce cas est
    /// écarté en amont par `detect_extends` (précondition d'appel, cf. doc
    /// de `parse_page_tokens`).
    pub extends: Option<&'src str>,
    /// Flux de tokens de contenu, dans l'ordre du fichier source. Ne
    /// contient jamais de déclaration `extends` (voir note ci-dessus).
    pub tokens: Vec<PageSourceToken<'src>>,
}

/// Arène de templates Mode Page — donne à chaque fichier admis une identité
/// stable et vérifiable par égalité de valeur (Document 2, §2 ; Phase 5.1).
///
/// ─── Ce que cette phase élimine ─────────────────────────────────────────
///
///   « fichier isolé, sans identité stable ». Un `ParsedPageTemplate<'src>`
///   sorti du Parser (Document 1) n'a pas de position dans une chaîne
///   d'héritage — l'admettre en arène lui attribue un `TemplateId` que les
///   phases suivantes (`NamedBlockRange`, Linker, Lowering — sessions
///   ultérieures) pourront porter sans emprunt auto-référentiel sur le
///   `Vec<ParsedPageTemplate<'src>>` lui-même.
///
/// ─── Ce que cette phase ne touche pas ───────────────────────────────────
///
///   Le contenu des tokens : `admit` ne fait aucune E/S (le fichier est déjà
///   lu et parsé au moment de l'appel) et ne copie aucune donnée — l'arène
///   prend possession du `ParsedPageTemplate<'src>` fourni, qui porte déjà
///   ses propres emprunts sur la source. Aucune logique de blocs
///   (`collect_blocks`) ni de liens (`link`) n'est introduite ici.
///
/// ─── Invariant d'identité ────────────────────────────────────────────────
///
///   `TemplateId` est `Copy`/`Eq` (défini en Phase 3.0, gelé) : deux appels à
///   `admit` produisent deux identifiants distincts (assignation par index
///   de poussée dans `templates`), et `get` sur un identifiant déjà admis
///   retourne exactement le contenu inséré — aucune mutation intermédiaire
///   n'est possible, `templates` n'expose aucune méthode de retrait ou de
///   modification en place.
///
/// ─── Allocation ──────────────────────────────────────────────────────────
///
///   `Vec` de croissance linéaire, une entrée par fichier admis (2 dans le
///   cas courant : enfant + parent — voir Document 2 §6.1 pour le point
///   ouvert sur l'héritage multi-niveaux, non traité par cette structure).
///   Build-time uniquement, jamais exposée au runtime du moteur.
#[derive(Debug, Default)]
pub struct PageArena<'src> {
    templates: Vec<ParsedPageTemplate<'src>>,
}

impl<'src> PageArena<'src> {
    /// Admet un template déjà parsé et lui attribue un `TemplateId` stable.
    ///
    /// Prend possession de `parsed` — aucune copie. L'identifiant retourné
    /// est l'index de poussée dans `templates` : strictement croissant à
    /// chaque appel, jamais réutilisé (pas de méthode de retrait exposée).
    pub fn admit(&mut self, parsed: ParsedPageTemplate<'src>) -> TemplateId {
        let id = TemplateId(self.templates.len() as u32);
        self.templates.push(parsed);
        id
    }

    /// Déréférence un `TemplateId` déjà admis vers son template.
    ///
    /// Précondition : `id` provient d'un appel à `admit` sur cette même
    /// arène. Violation : `panic!` par indexation hors bornes — cohérent
    /// avec la doc de `TemplateId` (vérification à l'exécution, pas de
    /// variante d'erreur dédiée à ce stade squelette ; le Linker, session
    /// ultérieure, décidera s'il faut absorber ce cas en `Result`).
    pub fn get(&self, id: TemplateId) -> &ParsedPageTemplate<'src> {
        &self.templates[id.0 as usize]
    }
}

/// Erreur du Parser Front-end mode page : construction de `PageBlockToken`
/// et `ChildTemplateSpec` à partir du flux de tokens source. Distincte de
/// `PageParseError` (Parser mode fragment, gelé, ne connaît ni `extends` ni
/// `block`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageComposeParseError {
    /// `{% extends %}` rencontré ailleurs qu'en tête de template, ou en
    /// double. Erreur de grammaire, pas de sémantique : sous la grammaire
    /// mode page, `extends` n'est valide qu'en position de tête — une
    /// occurrence supplémentaire ou déplacée ne correspond à aucune
    /// production valide, au même titre qu'un `UnexpectedToken` de
    /// `PageParseError`. Cf. doc de `ChildTemplateSpec::extends`.
    ExtendsNotFirst,
    /// Token reçu ≠ token attendu à cette position de l'automate. Symétrique
    /// de `PageParseError::UnexpectedToken` (Phase 1.3, gelé) — même forme,
    /// domaine d'erreur distinct (Document 1 §0 : `PageComposeParseError` ≠
    /// `PageParseError`, un appelant ne peut pas confondre les deux échecs).
    /// Introduit en Phase 4.3 : nécessaire dès que `parse_page_tokens`
    /// consomme un flux de `RawSpan` structuré, pas seulement pour le
    /// sous-ensemble `Runtime`.
    UnexpectedToken {
        expected: &'static str,
        got: SpanKind,
    },
    /// Itérateur épuisé alors qu'un token était requis pour compléter un
    /// pattern. Symétrique de `PageParseError::UnexpectedEof`.
    UnexpectedEof,
    /// Séquence de bloc non reconnue : `{% if entity.field %}` sans `.` dans
    /// l'ident bloc, ou tout autre motif structurellement invalide pour un
    /// mot-clé par ailleurs traité comme grammaticalement significatif à ce
    /// stade. Symétrique de `PageParseError::InvalidBlockSequence`.
    ///
    /// Portée Phase 4.7 : cette variante couvrait, temporairement (Phases
    /// 4.3 à 4.6), tout mot-clé de bloc encore non représentable par
    /// `PageSourceToken` à ce stade du classifieur. `block` et `endblock`
    /// en sont sortis en Phase 4.4 (branche `Block` dédiée) ; `static` en
    /// est sorti en Phase 4.5 (branche `Static` dédiée) ; `extends` en est
    /// sorti en Phase 4.6 (sa forme structurelle — `Ident(path) BlockClose`
    /// — reste jugée ici, seule sa *position* relève d'`ExtendsNotFirst`,
    /// domaine disjoint, cf. doc de `parse_page_tokens`). Le catch-all
    /// `Unsupported` a clos la grammaire en Phase 4.7 : tout mot-clé de
    /// bloc syntaxiquement bien formé (`Ident … BlockClose`) mais hors
    /// grammaire runtime connue est désormais capturé, jamais rejeté ici.
    /// Cette variante d'erreur n'est pas retirée pour autant : elle reste,
    /// à titre définitif, le domaine des erreurs de grammaire structurelle
    /// des mots-clés déjà reconnus (`if`/`endif`/`block`/`endblock`/
    /// `static`/`extends` — par exemple un `{% if %}` sans point dans
    /// l'identifiant, cf. `split_dotted_page`) **et** celui d'`include`,
    /// explicitement exclu du catch-all `Unsupported` (Phase 4.7, cf. doc
    /// de `parse_page_block`) : structurellement absent de la grammaire
    /// Mode Page, `include` n'est jamais « non supporté », il est rejeté.
    InvalidBlockSequence,
}

/// Erreur du Linker : résolution de dépendances entre templates
/// (`{% extends %}`, `{% static %}`) et correspondance des blocs enfant
/// contre les `BlockOpen` du parent référencé. Connaît l'existence d'autres
/// templates ; le Parser, phase amont, ne le peut pas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageLinkError<'src> {
    /// Chemin déclaré par `{% extends "path" %}` introuvable au moment de la
    /// résolution. Distinct de `ResolverError::IoError` (mode fragment) :
    /// contexte de phase différent (Linker vs résolution de capacité), pas
    /// une duplication.
    ExtendsNotFound { path: &'src str },
    /// `{% block name %}` défini dans un enfant sans `BlockOpen` de même nom
    /// dans l'AST du parent référencé par `extends`. Cf. doc de
    /// `ChildTemplateSpec`.
    OrphanBlock { name: &'src str },
    /// `{% static path %}` référence un fichier introuvable. Distinct
    /// d'`ExtendsNotFound` (chemin de template) et de
    /// `ResolverError::IoError` (mode fragment, `StaticInclude`) : trois
    /// erreurs de fichier manquant, trois contextes syntaxiques et trois
    /// phases différentes — pas de mutualisation prématurée avant d'avoir
    /// écrit les trois call-sites.
    StaticFileNotFound { path: &'src str },
}

/// Erreur de Validation sémantique mode page : propriétés vérifiables sur un
/// unique template déjà syntaxiquement valide, sans connaissance des autres
/// templates. Symétrique de `SemanticError` (mode fragment, Phase 1.4), sur
/// l'axe composition plutôt que sur l'axe condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageValidationError<'src> {
    /// Condition de `{% if %}` (mode page) qui ne référence pas un champ
    /// u8-sentinelle attendu par le contrat acté ci-dessus (Bool vs u8).
    /// Distincte d'une erreur Parser : le champ existe et est bien formé
    /// syntaxiquement, seul son type ne satisfait pas le contrat `Pod` du
    /// moteur — erreur sémantique, pas structurelle.
    NonBoolIfCondition { entity: &'src str, field: &'src str },
    /// `{% for %}` rencontré dans un template mode page. La spécification
    /// v1.1 exclut les boucles du modèle de rendu déterministe AOT (capacité
    /// non bornée statiquement) — variante nommée plutôt qu'un rejet
    /// générique qui masquerait la raison précise.
    ForLoopDetected,
    /// Mot-clé de requête relationnelle (jointure, filtre, tri…) détecté
    /// dans un template. Le mode page reste un langage de présentation :
    /// toute logique relationnelle appartient à la couche SQL/schema.
    RelationalKeyword { keyword: &'src str },
    /// `{% block %}` imbriqué dans un autre `{% block %}`. Symétrique de
    /// `SemanticError::NestedIfNotSupported` : même contrainte de platitude,
    /// appliquée à l'axe "bloc de fusion" plutôt qu'à l'axe "condition".
    /// Vérifiable sur l'AST d'un seul template, sans résolution externe —
    /// c'est pourquoi cette variante est Validation et non Link, malgré son
    /// lien thématique avec `PageBlockToken`.
    NestedBlock { name: &'src str },
}

/// Référence à une inclusion statique déduplique-able : futur `{% static %}`
/// du mode page.
///
/// ─── Contrat, distinct de `FlatPageToken::StaticInclude` ─────────────────
///
///   `StaticInclude` (mode fragment, existant, inchangé) : chaque occurrence
///   dans l'AST porte son propre `len`, mesuré et sommé indépendamment par
///   `resolve_and_measure`. Un fichier inclus à N endroits coûte N × len()
///   dans `TemplateMetrics::total_static_bytes` — correct pour le mode
///   fragment, où deux inclusions du même chemin sont deux `include_str!`
///   distincts sans lien entre eux.
///
///   `StaticPartialRef` (mode page, ce type) : le fichier référencé est
///   matérialisé une seule fois comme constante partagée
///   `static_partials::{IDENT}` (voir `static_const_ident`, fonction
///   existante, réutilisable sans modification pour calculer `IDENT` depuis
///   le chemin). Le coût en octets de ce fichier doit être compté UNE fois
///   dans les métriques globales, quel que soit le nombre d'occurrences de
///   `StaticPartialRef` qui le référencent dans l'AST fusionné parent+enfant.
///
///   Conséquence directe sur le futur calcul de capacité : la somme ne peut
///   pas être `Σ occurrences.len()` (ce que fait `resolve_and_measure` pour
///   `StaticInclude`) — elle doit être `Σ chemins_uniques.len()`. C'est
///   pourquoi ce type ne porte délibérément PAS de champ `len` par
///   occurrence : le coder ici inviterait à sommer par occurrence par
///   symétrie avec `StaticInclude`, exactement l'erreur que ce type doit
///   rendre impossible par construction. La taille du fichier sera portée
///   par un registre séparé, keyé par `const_ident`, écrit dans la session
///   qui résoudra `{% static %}` — hors périmètre ici.
///
/// ─── Pourquoi une variante distincte de `StaticInclude` plutôt qu'une
///     réutilisation avec déduplication ajoutée dans le résolveur ──────────
///
///   Réutiliser `StaticInclude` et déduplicer au niveau de
///   `resolve_and_measure` masquerait la distinction dans le type : rien
///   n'empêcherait alors un futur appelant de construire un `StaticInclude`
///   pour du contenu partagé et un autre pour du contenu non partagé, sans
///   qu'aucune signature ne le distingue. En portant la distinction dans le
///   type (`StaticPartialRef` ≠ `StaticInclude`), le compilateur rend la
///   confusion impossible : un `match` exhaustif sur le futur enum englobant
///   devra traiter les deux cas séparément, ce qui est le point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StaticPartialRef<'src> {
    /// Chemin tel qu'écrit dans le template, avant résolution manifeste.
    /// Miroir de `StaticInclude::original_path` — même convention, même
    /// lifetime, pour que la session de câblage puisse réutiliser
    /// `relative_path_for_include_str` sans adaptation. Non quoté (Phase
    /// 4.5, cf. doc `PageSourceToken::Static`) : slice brute issue du
    /// scanner, identique en forme au `path` d'`{% include %}`.
    pub original_path: &'src str,
}

#[cfg(test)]
mod tests_phase_3_0_page_mode_types {
    use super::{
        ChildTemplateSpec, NamedBlockRange, PageBlockToken, PageComposeParseError, PageLinkError,
        PageValidationError, StaticPartialRef, TemplateId,
    };

    /// Jalon Vert — les nouveaux types sont Copy/Clone/PartialEq comme leurs
    /// équivalents mode fragment (FlatPageToken l'est déjà pour IfBool/EndIf).
    /// Preuve par réaffectation sans move, comme `all_variants_are_copy`
    /// (Phase 1.1) — même style de preuve, cohérence de méthode.
    #[test]
    fn page_block_token_is_copy() {
        let a = PageBlockToken::BlockOpen { name: "header" };
        let _b = a;
        let _c = a;
        assert_eq!(a, PageBlockToken::BlockOpen { name: "header" });
    }

    /// Jalon Vert — le handle `TemplateId` distingue deux plages
    /// structurellement identiques (mêmes `start`/`end`) mais d'arènes
    /// différentes : c'est exactement l'invariant que ce type doit rendre
    /// vérifiable, ici prouvé par simple inégalité de valeur.
    #[test]
    fn named_block_range_is_copy_half_open_and_arena_tagged() {
        let child_a = TemplateId(0);
        let child_b = TemplateId(1);
        let r_a = NamedBlockRange {
            name: "header",
            template: child_a,
            start: 3,
            end: 7,
        };
        let r_b = NamedBlockRange {
            name: "header",
            template: child_b,
            start: 3,
            end: 7,
        };
        let _copy = r_a; // Copy, pas de move

        assert_eq!(
            r_a.end - r_a.start,
            4,
            "plage [start, end) : 4 tokens couverts"
        );
        assert_ne!(
            r_a, r_b,
            "même range, arène différente : distinguable par valeur"
        );
    }

    /// Jalon Vert — ChildTemplateSpec ne porte que de la métadonnée, jamais
    /// l'AST lui-même : ce test construit la forme attendue sans dépendance
    /// à un futur parseur mode page (zéro couplage avec le pipeline gelé).
    #[test]
    fn child_template_spec_shape() {
        let this_child = TemplateId(0);
        let spec = ChildTemplateSpec {
            extends: "layouts/base.marius",
            blocks: vec![
                NamedBlockRange {
                    name: "header",
                    template: this_child,
                    start: 0,
                    end: 2,
                },
                NamedBlockRange {
                    name: "body",
                    template: this_child,
                    start: 3,
                    end: 9,
                },
            ],
        };
        assert_eq!(spec.blocks.len(), 2);
        assert_eq!(spec.blocks[0].name, "header");
        assert!(spec.blocks.iter().all(|b| b.template == this_child));
    }

    /// Jalon Vert — StaticPartialRef ne porte pas de `len` (contrairement à
    /// StaticInclude) : ce test documente l'absence de champ par sa forme,
    /// pas par une assertion runtime (le compilateur est la preuve).
    #[test]
    fn static_partial_ref_has_no_len_field() {
        let r = StaticPartialRef {
            original_path: "partials/nav.html",
        };
        assert_eq!(r.original_path, "partials/nav.html");
    }

    /// Jalon Vert — les trois erreurs de phase sont des types distincts :
    /// une fonction de signature `Result<_, PageLinkError>` ne peut
    /// physiquement pas retourner `PageComposeParseError::ExtendsNotFirst`.
    /// Preuve par construction indépendante, pas par introspection runtime —
    /// l'invariant recherché est justement de ne pas être testable au sens
    /// classique : il est vérifié par le compilateur à chaque call-site
    /// futur, pas ici.
    #[test]
    fn phase_errors_are_distinct_types() {
        let _parse: PageComposeParseError = PageComposeParseError::ExtendsNotFirst;
        let _link: PageLinkError<'_> = PageLinkError::OrphanBlock { name: "sidebar" };
        let _validation: PageValidationError<'_> = PageValidationError::ForLoopDetected;
    }
}

// =============================================================================
// Phase 4.1 — Parser Mode Page : type `PageSourceToken<'src>`
// =============================================================================
//
// Portée de cette phase (roadmap §4.1) : définition de type seule, aucune
// fonction. `PageSourceToken` est l'alphabet englobant du Parser Mode Page
// (Document 1 §2.1) — l'unique nouveau type introduit ici.
//
// Diff nul garanti sur `FlatPageToken` (Phase 1.1, gelé) : cette section
// n'ajoute, ne modifie et ne supprime aucune variante de cet enum. Vérifiable
// par revue de la section Phase 1.1 ci-dessus, non retouchée.

/// Alphabet unique du Parser Mode Page — un fichier `.marius` composé se
/// représente entièrement comme `Vec<PageSourceToken<'src>>`.
///
/// ─── Pourquoi un enum englobant, pas une variante ajoutée à `FlatPageToken`,
///     pas une union de deux `Vec` séparés (Document 1 §2.1) ────────────────
///
///   Option écartée 1 — variante additionnelle sur `FlatPageToken` : cet enum
///   est matché de façon exhaustive, sans arm `_`, dans `validate_ast`,
///   `resolve_and_measure` et `generate_aot_snippet` (trois fonctions gelées
///   depuis Phase 1–2). Y ajouter une variante casserait leur exhaustivité et
///   forcerait leur édition pour recompiler — violation directe de la
///   contrainte de méthode de cette session (« ne modifier aucune fonction
///   existante »).
///
///   Option écartée 2 — deux `Vec` séparés (un `Vec<FlatPageToken>`, un
///   `Vec<PageBlockToken>` tenus en parallèle) : romprait le modèle « un
///   fichier source = un flux plat unique », commun à tout le pipeline depuis
///   le Scanner (`scan` produit un seul `impl Iterator<Item = RawSpan>`,
///   `parse_tokens` un seul `Vec<FlatPageToken>`). Deux flux séparés
///   réintroduiraient un problème de synchronisation d'ordre que le modèle à
///   flux unique élimine par construction.
///
///   Retenu — enum englobant paramétré sur les deux enums existants
///   (`FlatPageToken`, `PageBlockToken`) plus deux variantes propres au
///   Parser Mode Page (`Static`, `Unsupported`). Un seul `Vec`, un seul ordre
///   d'apparition, cohérent avec le reste du pipeline.
///
/// ─── Invariant de platitude ───────────────────────────────────────────────
///
///   Aucune variante ne porte de `Vec<Self>` ni de `Vec<FlatPageToken>`
///   imbriqué — cohérent avec `PageBlockToken` (Phase 3.0) et avec
///   `NamedBlockRange`, qui représentent déjà le contenu d'un bloc comme une
///   plage d'indices dans un `Vec` plat plutôt que comme une sous-structure
///   récursive. Ce type suit le même principe : aucun nouvel arbre.
///
/// ─── `Copy`, zéro indirection supplémentaire ──────────────────────────────
///
///   Agrégat de types déjà `Copy` (`FlatPageToken<'src>`, `PageBlockToken<'src>`,
///   `StaticPartialRef<'src>`, `&'src str`) : `PageSourceToken` est `Copy` par
///   construction, sans `Box`, `Rc` ni indirection ajoutée par l'enum
///   englobant lui-même. Coût mémoire par token : celui de la plus grande
///   variante — voir `page_source_token_layout_is_frozen` ci-dessous pour la
///   valeur figée et sa justification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageSourceToken<'src> {
    /// Opérateur de projection, identique au Mode Fragment : `{{ }}`,
    /// `{% if %}` / `{% endif %}`, segment HTML verbatim.
    ///
    /// N'émet jamais `FlatPageToken::StaticInclude` — cette variante de
    /// `FlatPageToken` reste strictement liée à `{% include %}`, absent de
    /// la grammaire Mode Page (Document 1 §2.1). Cette contrainte n'est pas
    /// vérifiable par le système de types à ce stade (elle porte sur la
    /// *construction* du token, réservée au classifieur — Phase 4.3+, hors
    /// périmètre de cette phase) ; elle est documentée ici comme contrat
    /// d'usage du champ.
    Runtime(FlatPageToken<'src>),

    /// Opérateur de composition : `{% block %}` / `{% endblock %}`.
    Block(PageBlockToken<'src>),

    /// Opérateur de composition : `{% static path %}`.
    ///
    /// Forme lexicale actée en Phase 4.5 : `path` est un `Ident` de bloc nu,
    /// sans guillemets — symétrique de `{% include path %}` (Mode Fragment,
    /// gelé, cf. `parse_block`), pas de la notation à guillemets utilisée à
    /// titre illustratif dans Document 1 §2.1/§6. Le scanner (`InBlock`,
    /// Phase 1.2, gelé) n'a aucune notion de littéral de chaîne : il n'existe
    /// pas de `SpanKind` dédié aux guillemets, qui seraient sinon capturés
    /// tels quels dans la slice. Introduire un tel support serait une
    /// extension du scanner gelé, hors périmètre de cette phase (§3
    /// roadmap : « aucune dépendance à `std::fs` introduite », pas
    /// d'extension de grammaire lexicale). `original_path` est donc
    /// directement la slice brute retournée par le scanner, sans étape de
    /// dépouillement.
    Static(StaticPartialRef<'src>),

    /// Mot-clé de bloc reconnu syntaxiquement mais non supporté par la
    /// grammaire runtime (`for`, `join`, `where`, `filter`, `group`, ou tout
    /// mot-clé inconnu). Capturé plutôt que rejeté à ce stade : le Parser ne
    /// décide pas *pourquoi* c'est interdit — le fait brut est transmis à la
    /// Validation (Document 2), qui produira le message d'erreur nommé et
    /// précis (`PageValidationError::ForLoopDetected`,
    /// `RelationalKeyword`) plutôt qu'un rejet générique.
    Unsupported { keyword: &'src str, tail: &'src str },
}

#[cfg(test)]
mod tests_phase_4_1_page_source_token {
    use super::{FlatPageToken, PageBlockToken, PageSourceToken, StaticPartialRef};

    /// Jalon Vert — construction des 4 variantes.
    #[test]
    fn constructs_all_four_variants() {
        let runtime = PageSourceToken::Runtime(FlatPageToken::Static("hello"));
        let block = PageSourceToken::Block(PageBlockToken::BlockOpen { name: "header" });
        let static_ref = PageSourceToken::Static(StaticPartialRef {
            original_path: "partials/nav.html",
        });
        let unsupported = PageSourceToken::Unsupported {
            keyword: "for",
            tail: " item in items",
        };

        match runtime {
            PageSourceToken::Runtime(FlatPageToken::Static(s)) => assert_eq!(s, "hello"),
            _ => unreachable!(),
        }
        match block {
            PageSourceToken::Block(PageBlockToken::BlockOpen { name }) => {
                assert_eq!(name, "header")
            }
            _ => unreachable!(),
        }
        match static_ref {
            PageSourceToken::Static(StaticPartialRef { original_path }) => {
                assert_eq!(original_path, "partials/nav.html")
            }
            _ => unreachable!(),
        }
        match unsupported {
            PageSourceToken::Unsupported { keyword, tail } => {
                assert_eq!(keyword, "for");
                assert_eq!(tail, " item in items");
            }
            _ => unreachable!(),
        }
    }

    /// Jalon Vert — `match` exhaustif sans arm `_` compile : preuve que
    /// l'enum est fermé sur exactement 4 variantes, aucune de plus, aucune
    /// de moins. Si une variante est ajoutée ou retirée sans mise à jour de
    /// ce `match`, ce test ne compile plus (erreur `non-exhaustive
    /// patterns`) — la garantie est portée par le compilateur, pas par une
    /// assertion runtime.
    #[test]
    fn match_is_exhaustive_without_wildcard() {
        fn classify(t: PageSourceToken<'_>) -> &'static str {
            match t {
                PageSourceToken::Runtime(_) => "runtime",
                PageSourceToken::Block(_) => "block",
                PageSourceToken::Static(_) => "static",
                PageSourceToken::Unsupported { .. } => "unsupported",
            }
        }
        assert_eq!(
            classify(PageSourceToken::Runtime(FlatPageToken::EndIf)),
            "runtime"
        );
        assert_eq!(
            classify(PageSourceToken::Block(PageBlockToken::BlockEnd)),
            "block"
        );
    }

    /// Jalon Vert — `Copy` disponible sur toutes les variantes (agrégat de
    /// types `Copy`). Preuve par réaffectation sans move, même style que
    /// `all_variants_are_copy` (Phase 1.1) et `page_block_token_is_copy`
    /// (Phase 3.0) — cohérence de méthode entre phases.
    #[test]
    fn all_variants_are_copy() {
        let tokens: [PageSourceToken<'_>; 4] = [
            PageSourceToken::Runtime(FlatPageToken::Static("content")),
            PageSourceToken::Block(PageBlockToken::BlockOpen { name: "body" }),
            PageSourceToken::Static(StaticPartialRef {
                original_path: "partials/foot.html",
            }),
            PageSourceToken::Unsupported {
                keyword: "where",
                tail: " x > 1",
            },
        ];

        let _a = tokens[0]; // premier move apparent
        let _b = tokens[0]; // second : compile ssi Copy est implémenté
    }

    /// Jalon Vert — layout figé. `size_of::<PageSourceToken>()` est verrouillé
    /// à sa valeur actuelle : toute variation future (ajout de champ, nouvelle
    /// variante plus large) doit être une décision explicite qui fait échouer
    /// ce test, jamais un effet de bord silencieux d'un changement ailleurs
    /// dans le fichier (ex. un champ ajouté à `FlatPageToken::StaticInclude`).
    ///
    /// Valeur observée sur cible 64 bits : la plus grande variante est
    /// `FlatPageToken::StaticInclude { original_path: &str, rel_from_manifest:
    /// &str, len: usize }`, portée via `Runtime`. Trois mots de 8 octets
    /// (2 fat pointers `&str` = 16 octets chacun, `usize` = 8 octets) plus le
    /// tag de discriminant de l'enum englobant, aligné sur 8 octets.
    #[test]
    fn page_source_token_layout_is_frozen() {
        assert_eq!(
            std::mem::size_of::<PageSourceToken<'_>>(),
            48,
            "layout de PageSourceToken modifié — variation à documenter explicitement, \
             pas à laisser passer silencieusement"
        );
    }
}

// =============================================================================
// Phase 4.2 — `detect_extends`
// =============================================================================
// Responsabilité unique (Document 1 §3) : décider si un fichier source est en
// Mode Page, sans parsing complet et sans dépendance à `PageSourceToken`.
//
// Invariant introduit : le mode est décidable sans parsing complet — un
// fichier est en Mode Page ssi sa toute première unité syntaxique, avant
// tout texte HTML verbatim et avant tout autre délimiteur, est un `{%`
// dont le premier `Ident` vaut `"extends"`.

/// Détermine si `source` relève du Mode Page (présence d'un `{% extends %}`
/// en tête de fichier), sans effectuer le parsing complet de la grammaire.
///
/// ─── Algorithme ────────────────────────────────────────────────────────────
///
/// Réutilise `scan` (Phase 1.2, gelé) et consomme au plus deux `RawSpan` :
/// le premier span de l'itérateur, puis, seulement s'il s'agit d'un
/// `BlockOpen` (`{%`), le second. Aucun troisième appel à `next()` n'est
/// effectué : la fonction s'arrête dès que le verdict est connu — c'est le
/// sens de « O(1) amorti » (Document 1 §3) : le coût est borné par la
/// position du premier délimiteur, jamais par la longueur totale du fichier
/// au-delà de ce point.
///
/// - Si le premier span n'est pas `BlockOpen` (fichier sans délimiteur →
///   `None` ; premier délimiteur `{{` → `ExprOpen` ; texte HTML précédant
///   `{%` → `Literal`), le fichier n'est pas en Mode Page : `false`.
/// - Si le premier span est `BlockOpen`, le second span est examiné : `true`
///   ssi c'est un `Ident` de contenu exactement `"extends"`.
///
/// ─── Ce que cette fonction NE valide PAS ───────────────────────────────────
///
/// Ne valide pas la forme complète de la déclaration `extends` (présence
/// d'un chemin, guillemets bien formés, `%}` de fermeture) : un `extends`
/// syntaxiquement malformé en tête de fichier est tout de même détecté ici
/// (`true`) et échoue plus tard dans `parse_page_tokens` (§3), pas dans
/// cette fonction.
///
/// Un fichier où du texte précède `{% extends %}` retourne `false` : la
/// première unité syntaxique n'est alors pas `BlockOpen` mais `Literal`.
/// Ce même fichier, s'il atteint `parse_page_tokens` par un autre chemin
/// d'appel, échoue avec `PageComposeParseError::ExtendsNotFirst` (Phase 4.6)
/// — produire cette erreur nommée n'est pas la responsabilité de cette
/// fonction, qui ne retourne qu'un `bool` (contrat d'appel, cf. Document 1
/// §3 : `parse_page_tokens` n'est appelée qu'après `detect_extends ==
/// true`, sauf cas du parent, admis sans cette précondition).
///
/// ─── Invariants mémoire ─────────────────────────────────────────────────────
///
/// Aucune allocation heap : `scan` n'alloue rien (Phase 1.2), et cette
/// fonction ne construit aucune structure intermédiaire. Aucune E/S : pas
/// d'appel à `std::fs`, la fonction opère exclusivement sur `source: &str`
/// déjà en mémoire.
pub fn detect_extends(source: &str) -> bool {
    let mut spans = scan(source);
    match spans.next() {
        Some(RawSpan {
            kind: SpanKind::BlockOpen,
            ..
        }) => matches!(
            spans.next(),
            Some(RawSpan {
                kind: SpanKind::Ident,
                slice: "extends"
            })
        ),
        _ => false,
    }
}

// =============================================================================
// Tests — Phase 4.2
// =============================================================================

#[cfg(test)]
mod tests_phase_4_2_detect_extends {
    use super::detect_extends;

    /// Jalon Vert — fichier sans `{%` (aucun délimiteur de bloc) → `false`.
    #[test]
    fn no_block_delimiter_returns_false() {
        assert!(!detect_extends("<div>hello {{ entity.field }}</div>"));
    }

    /// Jalon Vert — `{% extends %}` en toute première position → `true`.
    #[test]
    fn extends_at_head_returns_true() {
        assert!(detect_extends(r#"{% extends "base.marius" %}"#));
    }

    /// Jalon Vert — un autre mot-clé de bloc en tête (`{% if %}`) → `false`.
    #[test]
    fn if_at_head_returns_false() {
        assert!(!detect_extends("{% if entity.active %}yes{% endif %}"));
    }

    /// Jalon Vert — `extends` précédé de texte HTML → `false` : la première
    /// unité syntaxique est alors `Literal`, pas `BlockOpen`. Preuve directe
    /// que la fonction juge la *position*, pas la simple *présence* du
    /// mot-clé dans le fichier.
    #[test]
    fn extends_after_leading_text_returns_false() {
        assert!(!detect_extends(
            r#"<p>intro</p>{% extends "base.marius" %}"#
        ));
    }

    /// Fichier vide → `false` (premier `next()` retourne `None`, aucune E/S,
    /// aucun panic).
    #[test]
    fn empty_source_returns_false() {
        assert!(!detect_extends(""));
    }
}

// =============================================================================
// Phase 4.3 — Classifieur : sous-ensemble `Runtime`
// =============================================================================
// Responsabilité unique (roadmap §4.3) : un template Mode Page sans opérateur
// de composition produit un flux `PageSourceToken` structurellement
// équivalent à `parse_tokens` (Mode Fragment, Phase 1.3, gelé).
//
// Périmètre :
//   - Reconnaît `Static` (Literal), `Field` (`{{ }}`), `IfBool`/`EndIf`
//     (`{% if %}` / `{% endif %}`) — les quatre productions de la grammaire
//     runtime, chacune enveloppée sous `PageSourceToken::Runtime`.
//   - Ne touche pas `parse_tokens` (gelé) : aucune fonction existante
//     modifiée, aucun automate partagé — deux implémentations disjointes,
//     conformément à la frontière de domaine d'erreur actée Document 1 §0.
//   - N'implémentait pas, à la clôture de 4.3, `block`/`endblock`, `static`,
//     `extends`, ni le catch-all `Unsupported` : tout mot-clé de bloc autre
//     que `if`/`endif` — y compris `include` (absent de la grammaire Mode
//     Page par construction du type `PageSourceToken::Runtime`, cf. Phase
//     4.1) — échouait avec `PageComposeParseError::InvalidBlockSequence`.
//     Depuis, `block`/`endblock` (Phase 4.4), `static` (Phase 4.5),
//     `extends` (Phase 4.6) et le catch-all `Unsupported` avec l'exclusion
//     explicite d'`include` (Phase 4.7) sont sortis de ce catch-all — voir
//     sections dédiées ci-dessous. La grammaire des mots-clés de bloc est
//     désormais close (Document 1 clos sur ce point).
//   - `{% block %}` / `{% endblock %}` (Phase 4.4), `{% static %}` (Phase
//     4.5), `{% extends %}` (Phase 4.6) et le catch-all `Unsupported` /
//     `{% include %}` (Phase 4.7) : voir sections dédiées ci-dessous, qui
//     étendent `parse_page_block` (seule fonction modifiée à chaque fois)
//     sans toucher à ce dispatch de tête.

// =============================================================================
// Phase 4.6 — Position d'`extends` + `ExtendsNotFirst`
// =============================================================================
// Invariant introduit (roadmap §4.6) : `extends`, s'il existe, occupe
// nécessairement la première position non-whitespace du fichier — jamais
// ailleurs, jamais en double.
//
// Périmètre :
//   - `ParsedPageTemplate<'src>` (Document 1 §2.2) devient le type de sortie
//     de `parse_page_tokens` — `extends: Option<&'src str>` et
//     `tokens: Vec<PageSourceToken<'src>>`, ce dernier ne portant jamais de
//     déclaration `extends` (cf. doc du type).
//   - `parse_page_block` reconnaît désormais `extends` (branche dédiée,
//     forme jugée localement) et retourne un `PageBlockOutcome` pour laisser
//     `parse_page_tokens`, seule à connaître la position d'un span dans le
//     flux, juger la légalité de cette position.
//   - Logique de position uniquement : aucune résolution du chemin déclaré
//     (résolution d'existence : Linker, Document 2, `PageLinkError::
//     ExtendsNotFound`, hors périmètre ici).
//   - Ce diff modifie la signature publique de `parse_page_tokens`
//     (`Vec<PageSourceToken>` → `ParsedPageTemplate`) : les tests des
//     Phases 4.3/4.4/4.5 sont ajustés en conséquence (accès via `.tokens`),
//     sans changement de leurs assertions de fond — pure adaptation de
//     signature, pas une extension de portée de ce diff.

/// Construit l'AST complet (`ParsedPageTemplate<'src>`) d'un unique fichier
/// — grammaire Mode Page hors catch-all `Unsupported` (Phase 4.7).
///
/// ─── Automate ──────────────────────────────────────────────────────────────
///
/// Structurellement identique à `parse_tokens` (Phase 1.3, gelé) : même
/// dispatch sur `SpanKind` en position de tête (`Literal` → `Static`,
/// `ExprOpen` → `Field`, `BlockOpen` → sous-automate de bloc), même primitives
/// de consommation (`expect_ident`/`expect_kind`, réimplémentées ici sous
/// domaine d'erreur `PageComposeParseError` pour ne pas coupler ce
/// classifieur au type d'erreur gelé `PageParseError` — Document 1 §0).
/// Chaque token de contenu est enveloppé sous `PageSourceToken::Runtime`
/// avant d'être poussé dans l'AST — c'est la seule différence structurelle
/// avec `parse_tokens`.
///
/// ─── Position d'`extends` (Phase 4.6) ──────────────────────────────────────
///
/// `extends` n'est jamais poussé dans `tokens` : c'est une propriété du
/// fichier, portée par le champ séparé `ParsedPageTemplate::extends`
/// (Document 1 §2.2, cf. doc du type). La position de tête est vérifiée ici,
/// et seulement ici — `parse_page_block` reconnaît la forme syntaxique
/// d'`extends` mais ne sait pas, et ne doit pas savoir, à quelle position du
/// flux il a été rencontré (cf. doc de `PageBlockOutcome`). Concrètement :
/// `is_head` est vrai uniquement à la toute première itération de la boucle,
/// quel que soit le type de span rencontré ensuite ; toute déclaration
/// `extends` obtenue alors que `is_head` est faux — qu'elle apparaisse après
/// un autre token ou qu'elle soit une seconde occurrence — échoue avec
/// `PageComposeParseError::ExtendsNotFirst` (Document 1 §6, §7 : fail-fast,
/// pas d'accumulation). Un fichier sans aucun `extends` (parent) laisse ce
/// champ à `None` sans qu'aucune erreur ne soit levée — Document 1 §3.
///
/// ─── Grammaire close (Phase 4.7) ───────────────────────────────────────────
///
/// Reconnaît désormais tout mot-clé de bloc : `if`/`endif`/`block`/
/// `endblock`/`static`/`extends` chacun sous sa forme dédiée, `include`
/// explicitement exclu (`PageComposeParseError::InvalidBlockSequence`), et
/// tout le reste sous `PageSourceToken::Unsupported` (catch-all, voir doc de
/// `parse_page_block`). Aucun mot-clé de tête ne peut plus atteindre un
/// chemin d'erreur générique non informatif — Document 1 clos sur ce point.
///
/// ─── Invariants mémoire ─────────────────────────────────────────────────────
///
/// Zéro allocation de texte : chaque `&'src str` porté par un
/// `PageSourceToken` (ou par `ParsedPageTemplate::extends`) est un emprunt
/// direct sur `spans`, jamais une copie — identique au contrat de
/// `parse_tokens`. Le seul `Vec` alloué est celui de `tokens`, build-time,
/// conditionnel au premier `push` (cf. Document 1 §5) — une déclaration
/// `extends` n'y contribue jamais.
pub fn parse_page_tokens<'src>(
    spans: impl Iterator<Item = RawSpan<'src>>,
) -> Result<ParsedPageTemplate<'src>, PageComposeParseError> {
    let mut iter = spans.peekable();
    let mut tokens = Vec::new();
    let mut extends: Option<&'src str> = None;
    let mut is_head = true;

    while let Some(span) = iter.next() {
        let head = is_head;
        is_head = false;

        match span.kind {
            // Texte HTML verbatim → Static directement, enveloppé Runtime.
            SpanKind::Literal => {
                tokens.push(PageSourceToken::Runtime(FlatPageToken::Static(span.slice)));
            }

            // `{{ entity.field }}` → Field, enveloppé Runtime.
            SpanKind::ExprOpen => {
                tokens.push(PageSourceToken::Runtime(parse_page_expr(&mut iter)?));
            }

            // `{% keyword … %}` → IfBool | EndIf | BlockOpen | BlockEnd |
            // Static(..) | Extends(path). `parse_page_block` décide de la
            // forme (`PageBlockOutcome`) ; seule cette fonction sait si le
            // span de tête `{%` consommé était le tout premier du fichier,
            // donc seule elle peut juger la position d'un `Extends` (Phase
            // 4.6 : voir doc ci-dessus).
            SpanKind::BlockOpen => match parse_page_block(&mut iter)? {
                PageBlockOutcome::Extends(path) => {
                    if !head {
                        return Err(PageComposeParseError::ExtendsNotFirst);
                    }
                    extends = Some(path);
                }
                PageBlockOutcome::Token(token) => tokens.push(token),
            },

            // Tout autre span en position initiale est une erreur
            // structurelle : ExprClose, BlockClose, Ident, Punct ne peuvent
            // pas ouvrir un token, au même titre que dans `parse_tokens`.
            got => {
                return Err(PageComposeParseError::UnexpectedToken {
                    expected: "Literal | ExprOpen | BlockOpen",
                    got,
                });
            }
        }
    }

    Ok(ParsedPageTemplate { extends, tokens })
}

// =============================================================================
// Phase 4.4 — Reconnaissance `{% block %}` / `{% endblock %}`
// =============================================================================
// Responsabilité unique (roadmap §4.4) : les marqueurs de composition
// `{% block name %}` / `{% endblock %}` sont représentables dans l'AST Mode
// Page sans être résolus (correspondance parent/enfant, Document 2) ni
// validés (appariement, absence d'imbrication, Document 2) — permissivité
// délibérée déjà actée Document 1 §4/§6.
//
// Invariant introduit : un `{% block name %}` produit toujours
// `PageSourceToken::Block(PageBlockToken::BlockOpen { name })`, un
// `{% endblock %}` toujours `PageSourceToken::Block(PageBlockToken::BlockEnd)`
// — sans vérification d'appariement ni de nom à la fermeture, y compris pour
// des blocs imbriqués. Un fichier à blocs mal appariés ou imbriqués n'est
// donc PAS rejeté par cette phase : c'est une propriété positive de ce
// diff, prouvée par le test `nested_blocks_parse_succeeds` ci-dessous, pas
// une lacune à corriger ici.
//
// Périmètre : une seule fonction modifiée (`parse_page_block`), une seule
// branche de l'automate ajoutée (`block` | `endblock` dans son `match`).
// Le dispatch de tête de `parse_page_tokens` (Phase 4.3) est ajusté en
// conséquence (propagation directe du `PageSourceToken` déjà enveloppé),
// sans ajout de nouvelle branche de `match` sur `SpanKind` — `BlockOpen`
// reste l'unique point d'entrée vers `parse_page_block`, comme en 4.3.
// `extends` (4.6) et le catch-all `Unsupported` (4.7) restent hors
// périmètre de ce diff 4.4 : ils continuaient, à l'époque, d'échouer via
// `PageComposeParseError::InvalidBlockSequence`. `{% static %}` est sorti de
// ce même catch-all en Phase 4.5 (section dédiée plus bas) — seul `extends`
// y échoue encore à ce stade.

// ─── Parseurs de sous-séquences (domaine `PageComposeParseError`) ────────────
//
// Symétriques de `parse_expr`/`parse_block` (Phase 1.3, gelées) : même
// pattern de consommation, domaine d'erreur `PageComposeParseError` au lieu
// de `PageParseError` — duplication délibérée plutôt que généricité sur le
// type d'erreur, pour ne pas coupler le classifieur Mode Page au type
// d'erreur gelé du Parser Mode Fragment (Document 1 §0).

/// Consomme `Ident(entity) Punct(.) Ident(field) ExprClose` et produit
/// `FlatPageToken::Field`. Précondition : `ExprOpen` vient d'être consommé
/// par `parse_page_tokens`.
fn parse_page_expr<'src, I>(iter: &mut I) -> Result<FlatPageToken<'src>, PageComposeParseError>
where
    I: Iterator<Item = RawSpan<'src>>,
{
    let entity = expect_ident_page(iter, "Ident(entity)")?;
    expect_kind_page(iter, SpanKind::Punct, "Punct('.')")?;
    let field = expect_ident_page(iter, "Ident(field)")?;
    expect_kind_page(iter, SpanKind::ExprClose, "ExprClose('}}')")?;
    Ok(FlatPageToken::Field { entity, field })
}

/// Résultat de `parse_page_block` (Phase 4.6). Distingue un token de contenu
/// ordinaire, prêt à être poussé dans `ParsedPageTemplate::tokens`, d'une
/// déclaration `{% extends "path" %}`, qui n'est **jamais** poussée dans
/// `tokens` (cf. doc de `ParsedPageTemplate`).
///
/// Cette distinction est nécessaire parce que la position d'`extends`
/// (tête de fichier ou non) ne peut être jugée que par `parse_page_tokens`
/// — seule fonction qui observe l'ordre des spans de tête au fil de son
/// itération. `parse_page_block` reconnaît la forme syntaxique d'`extends`
/// (grammaire), mais ne connaît pas, et ne doit pas se voir déléguer, sa
/// position (une question de grammaire mono-fichier distincte, cf. doc de
/// `PageComposeParseError::ExtendsNotFirst`) : lui faire porter ce jugement
/// dupliquerait, à l'échelle d'une seule fonction, l'état que
/// `parse_page_tokens` maintient déjà (`is_head`) — deux sources de vérité
/// pour une même position, un candidat naturel à la divergence.
enum PageBlockOutcome<'src> {
    /// Token de contenu ordinaire — `if`/`endif`/`block`/`endblock`/`static`.
    Token(PageSourceToken<'src>),
    /// Chemin brut d'une déclaration `{% extends path %}`, syntaxiquement
    /// bien formée. La légalité de sa position est jugée par l'appelant.
    Extends(&'src str),
}

/// Consomme `Ident(keyword) … BlockClose` et produit le `PageBlockOutcome`
/// correspondant. Précondition : `BlockOpen` vient d'être consommé par
/// `parse_page_tokens`.
///
/// Portée Phase 4.7 : reconnaît `if`/`endif` (Phase 4.3, logique inchangée),
/// `block`/`endblock` (Phase 4.4, logique inchangée), `static` (Phase 4.5,
/// logique inchangée), `extends` (Phase 4.6, logique inchangée), `include`
/// (exclusion explicite, introduite ici) et le catch-all `Unsupported`
/// (introduit ici) pour tout le reste. Cette fonction est désormais totale
/// sur la grammaire lexicale des mots-clés de bloc : aucun `Ident` de tête
/// ne peut plus atteindre un chemin d'erreur générique non informatif —
/// Document 1 clos sur ce point (roadmap §4.7).
///
/// ─── Pourquoi le type de retour change : `PageBlockOutcome`, plus
///     `PageSourceToken` directement ─────────────────────────────────────────
///
/// `if`/`endif`/`block`/`endblock`/`static` restent enveloppés exactement
/// comme en Phase 4.5 (`PageSourceToken`, lui-même sous `Runtime` ou
/// `Block`/`Static` selon le cas). `extends` seul n'a pas d'enveloppe
/// `PageSourceToken` : ce n'est pas un token de contenu, c'est un champ de
/// `ParsedPageTemplate` (cf. doc du type) — `PageBlockOutcome::Extends` le
/// fait remonter à l'appelant sans le faire transiter par `PageSourceToken`,
/// ce qui rendrait par construction impossible de le pousser par erreur
/// dans `tokens`.
///
/// ─── Invariant introduit en Phase 4.6 : zéro E/S sur `extends`,
///     forme jugée ici, position jugée par l'appelant ─────────────────────────
///
/// Comme `static` (Phase 4.5), la branche `extends` capture `path` tel quel
/// — aucun appel `std::fs`, aucune vérification d'existence. Elle vérifie en
/// revanche la forme (`Ident(path) BlockClose`, sinon `UnexpectedToken`/
/// `UnexpectedEof`) : c'est un jugement de grammaire mono-fichier, dans le
/// domaine de cette fonction. Ce que cette fonction ne vérifie jamais, y
/// compris pour `extends` : la position dans le fichier — jugée exclusivement
/// par `parse_page_tokens` via `PageComposeParseError::ExtendsNotFirst`
/// (cf. doc de `PageBlockOutcome`).
///
/// ─── Invariant introduit en Phase 4.5 : zéro E/S sur `static` ────────────
///
/// La branche `static` capture `original_path` tel quel — aucun appel
/// `std::fs`, aucune vérification d'existence, aucune résolution de chemin
/// relatif. Un chemin syntaxiquement bien formé mais inexistant sur disque
/// produit un `Ok` identique à un chemin existant : l'existence est une
/// propriété du Linker (`PageLinkError::StaticFileNotFound`, Document 2),
/// pas du Parser (Document 1 §5/§6). Cf. `static_path_parses_without_touching_filesystem`.
///
/// ─── Permissivité délibérée sur l'imbrication (Document 1 §4, §6) ─────────
///
/// Cette fonction ne maintient aucune pile de blocs ouverts : un
/// `{% block %}` rencontré alors qu'un autre est déjà ouvert est accepté
/// sans distinction — l'appariement correct et l'absence d'imbrication ne
/// sont pas des garanties de sortie du Parser (cf. Document 1 §6). Juger
/// l'imbrication exige un état de pile que seule la Validation (Document 2,
/// `PageValidationError::NestedBlock`) construit ; le dupliquer ici
/// recréerait la fusion syntaxe/sémantique que le Parser doit éviter par
/// construction.
fn parse_page_block<'src, I>(iter: &mut I) -> Result<PageBlockOutcome<'src>, PageComposeParseError>
where
    I: Iterator<Item = RawSpan<'src>>,
{
    let keyword = expect_ident_page(
        iter,
        "keyword (if | endif | block | endblock | static | extends | asset | script | endscript)",
    )?;

    match keyword {
        "if" => {
            let raw = expect_ident_page(iter, "Ident(entity.field)")?;
            let (entity, field) = split_dotted_page(raw)?;
            expect_kind_page(iter, SpanKind::BlockClose, "BlockClose('%}')")?;
            Ok(PageBlockOutcome::Token(PageSourceToken::Runtime(
                FlatPageToken::IfBool { entity, field },
            )))
        }
        "endif" => {
            expect_kind_page(iter, SpanKind::BlockClose, "BlockClose('%}')")?;
            Ok(PageBlockOutcome::Token(PageSourceToken::Runtime(
                FlatPageToken::EndIf,
            )))
        }
        "block" => {
            let name = expect_ident_page(iter, "Ident(name)")?;
            expect_kind_page(iter, SpanKind::BlockClose, "BlockClose('%}')")?;
            Ok(PageBlockOutcome::Token(PageSourceToken::Block(
                PageBlockToken::BlockOpen { name },
            )))
        }
        "endblock" => {
            expect_kind_page(iter, SpanKind::BlockClose, "BlockClose('%}')")?;
            Ok(PageBlockOutcome::Token(PageSourceToken::Block(
                PageBlockToken::BlockEnd,
            )))
        }
        // `{% static path %}` (Phase 4.5) : capture brute, zéro E/S. `path`
        // est l'`Ident` de bloc nu retourné par le scanner — pas de
        // dépouillement de guillemets (cf. doc `PageSourceToken::Static`).
        // Aucune vérification d'existence : c'est le rôle du Linker
        // (`PageLinkError::StaticFileNotFound`, Document 2), pas celui de
        // cette fonction.
        "static" => {
            let original_path = expect_ident_page(iter, "Ident(path)")?;
            expect_kind_page(iter, SpanKind::BlockClose, "BlockClose('%}')")?;
            Ok(PageBlockOutcome::Token(PageSourceToken::Static(
                StaticPartialRef { original_path },
            )))
        }
        // `{% extends path %}` (Phase 4.6) : capture brute, zéro E/S, même
        // convention non-quotée que `static` (symétrie délibérée — cf. doc
        // `StaticPartialRef::original_path`). Forme jugée ici ; position
        // jugée par `parse_page_tokens` (cf. doc de `PageBlockOutcome`).
        "extends" => {
            let path = expect_ident_page(iter, "Ident(path)")?;
            expect_kind_page(iter, SpanKind::BlockClose, "BlockClose('%}')")?;
            Ok(PageBlockOutcome::Extends(path))
        }
        // `{% include path %}` (Phase 4.7) : exclusion explicite du catch-all
        // `Unsupported` ci-dessous — roadmap §4.7 exige `∉ {if, endif,
        // include, extends, block, endblock, static}` pour la branche
        // par défaut. `include` n'est pas « non supporté » au sens
        // d'`Unsupported` (un mot-clé dont la grammaire est inconnue de ce
        // Parser) : sa grammaire *est* connue (Mode Fragment, `parse_block`,
        // gelé) — il est structurellement absent de la grammaire Mode Page
        // par construction du type (`PageSourceToken::Runtime` n'émet
        // jamais `FlatPageToken::StaticInclude`, cf. doc de cette variante).
        // Le confondre avec `Unsupported` ferait porter à la Validation
        // (Document 2) la charge de distinguer, au sein d'un même verdict
        // « non supporté », un mot-clé simplement pas encore implémenté
        // (`for`) d'un mot-clé délibérément interdit dans ce mode
        // (`include`, qui a un équivalent : `static`) — une confusion que
        // Document 1 §0 proscrit explicitement (fusion syntaxe/sémantique).
        // Bras explicite plutôt que laissé retomber dans le catch-all : sans
        // lui, `include` migrerait silencieusement vers `Unsupported` dès
        // que le catch-all serait ajouté — un effet de bord de ce diff, pas
        // une décision prise consciemment.
        "include" => Err(PageComposeParseError::InvalidBlockSequence),
        // `{% asset key %}` (spec `marius-assets-specification.md` §9) :
        // à la différence d'`include` (Mode Fragment exclusif, cf. bras
        // ci-dessus), `asset` est valide dans les deux modes — l'exemple de
        // référence de la spec §9 (balises `<link>` dans un layout) est
        // typiquement du Mode Page. Enveloppé sous `Runtime` comme `if`/
        // `endif` : c'est un token de contenu ordinaire pour ce Parser,
        // résolu plus tard par `resolve_and_measure`/`generate_aot_snippet`.
        "asset" => {
            let key = expect_ident_page(iter, "Ident(key)")?;
            expect_kind_page(iter, SpanKind::BlockClose, "BlockClose('%}')")?;
            Ok(PageBlockOutcome::Token(PageSourceToken::Runtime(
                FlatPageToken::AssetRef(key),
            )))
        }
        // `{% script %}` / `{% endscript %}` (session dédiée au hoisting) :
        // valides dans les deux modes, comme `asset` juste au-dessus —
        // enveloppés sous `Runtime` comme `if`/`endif`, ce sont des tokens
        // de contenu ordinaires pour ce Parser. La distinction Page/Fragment
        // isolé (hisser ou laisser en No-Op) ne se joue jamais ici, ni même
        // dans ce crate — elle vit exclusivement dans `build.rs`.
        "script" => {
            expect_kind_page(iter, SpanKind::BlockClose, "BlockClose('%}')")?;
            Ok(PageBlockOutcome::Token(PageSourceToken::Runtime(
                FlatPageToken::ScriptStart,
            )))
        }
        "endscript" => {
            expect_kind_page(iter, SpanKind::BlockClose, "BlockClose('%}')")?;
            Ok(PageBlockOutcome::Token(PageSourceToken::Runtime(
                FlatPageToken::ScriptEnd,
            )))
        }
        // Catch-all (Phase 4.7, roadmap §4.7, Document 1 §2.1) : tout
        // mot-clé de bloc hors grammaire déjà reconnue (`for`, `join`,
        // `where`, `filter`, `group`, ou tout mot-clé inconnu) est capturé
        // sous `Unsupported`, jamais rejeté à ce stade — Document 1 §6 :
        // « jamais silencieusement ignoré ni rejeté ». Cette branche clôt la
        // grammaire de `parse_page_block` : plus aucun mot-clé ne peut
        // atteindre un chemin d'erreur générique non informatif.
        //
        // ─── Arité non contrainte (0, 1 ou N tokens avant `%}`) ────────────
        //
        // Ce Parser ne connaît pas la grammaire de ces mots-clés — il ignore
        // si `for` attend `item in items` (3 tokens), `filter` un unique
        // prédicat, ou si un mot-clé inconnu n'attend rien du tout. Il ne
        // doit donc jamais échouer sur l'arité : tous les tokens jusqu'à
        // `BlockClose` sont consommés sans jugement de forme, en une seule
        // passe, sans retour arrière — cohérent avec l'automate `O(n)` sans
        // backtracking de ce module (cf. doc du fichier d'architecture).
        //
        // ─── `tail` : premier token suivant le mot-clé, ou vide ───────────
        //
        // `tail` capture le premier `Ident` rencontré après le mot-clé
        // (`""` si `BlockClose` suit immédiatement) — un indice minimal,
        // suffisant pour que la Validation (Document 2) nomme le rejet
        // (`ForLoopDetected`, `RelationalKeyword`, etc.) sans que ce Parser
        // ne tente de reconstituer la totalité du contenu du bloc. Tout
        // token additionnel au-delà du premier est consommé mais non
        // conservé : cette consommation ne vise qu'à resynchroniser
        // l'automate sur `BlockClose`, pas à préserver le contenu — zéro
        // allocation (`tail` reste un emprunt direct sur `spans`, jamais une
        // concaténation).
        keyword => {
            let mut tail = "";
            let mut seen_first = false;
            loop {
                match iter.next() {
                    Some(span) if span.kind == SpanKind::BlockClose => break,
                    Some(span) => {
                        if !seen_first {
                            tail = span.slice;
                            seen_first = true;
                        }
                    }
                    None => return Err(PageComposeParseError::UnexpectedEof),
                }
            }
            Ok(PageBlockOutcome::Token(PageSourceToken::Unsupported {
                keyword,
                tail,
            }))
        }
    }
}

// ─── Primitives de consommation (domaine `PageComposeParseError`) ────────────

/// Consomme le span suivant et retourne sa slice si c'est un `Ident`.
#[inline]
fn expect_ident_page<'src, I>(
    iter: &mut I,
    expected: &'static str,
) -> Result<&'src str, PageComposeParseError>
where
    I: Iterator<Item = RawSpan<'src>>,
{
    match iter.next() {
        Some(span) if span.kind == SpanKind::Ident => Ok(span.slice),
        Some(span) => Err(PageComposeParseError::UnexpectedToken {
            expected,
            got: span.kind,
        }),
        None => Err(PageComposeParseError::UnexpectedEof),
    }
}

/// Consomme le span suivant et vérifie qu'il a le `kind` attendu.
#[inline]
fn expect_kind_page<'src, I>(
    iter: &mut I,
    kind: SpanKind,
    expected: &'static str,
) -> Result<(), PageComposeParseError>
where
    I: Iterator<Item = RawSpan<'src>>,
{
    match iter.next() {
        Some(span) if span.kind == kind => Ok(()),
        Some(span) => Err(PageComposeParseError::UnexpectedToken {
            expected,
            got: span.kind,
        }),
        None => Err(PageComposeParseError::UnexpectedEof),
    }
}

/// Coupe `"entity.field"` sur le premier `.` et retourne `("entity",
/// "field")`. Symétrique de `split_dotted` (Phase 1.3, gelée).
#[inline]
fn split_dotted_page(raw: &str) -> Result<(&str, &str), PageComposeParseError> {
    raw.find('.')
        .map(|i| (&raw[..i], &raw[i + 1..]))
        .ok_or(PageComposeParseError::InvalidBlockSequence)
}

// =============================================================================
// Tests — Phase 4.3
// =============================================================================

#[cfg(test)]
mod tests_phase_4_3_parse_page_tokens_runtime_subset {
    use super::{
        FlatPageToken, PageComposeParseError, PageSourceToken, parse_page_tokens, parse_tokens,
        scan,
    };

    /// Dépouille l'enveloppe `Runtime` d'un AST Mode Page pour comparaison
    /// directe avec la sortie de `parse_tokens` (Mode Fragment). Panique si
    /// une variante non-`Runtime` apparaît : les fixtures de ce module sont
    /// construites pour ne jamais en émettre (aucun opérateur de composition
    /// n'y figure).
    fn strip_runtime_envelope(tokens: Vec<PageSourceToken<'_>>) -> Vec<FlatPageToken<'_>> {
        tokens
            .into_iter()
            .map(|t| match t {
                PageSourceToken::Runtime(inner) => inner,
                other => panic!(
                    "fixture de non-régression 4.3 attend uniquement Runtime, obtenu {other:?}"
                ),
            })
            .collect()
    }

    /// Jalon Vert — fixture `Static` seul : un template sans aucun opérateur
    /// produit, dépouillé de son enveloppe `Runtime`, exactement le même AST
    /// que `parse_tokens` sur la même source.
    #[test]
    fn runtime_subset_matches_parse_tokens_static_only() {
        let src = "<div>plain html</div>";

        let expected = parse_tokens(scan(src)).expect("parse_tokens (référence) doit réussir");
        let actual =
            parse_page_tokens(scan(src)).expect("parse_page_tokens (classifieur) doit réussir");

        assert_eq!(strip_runtime_envelope(actual.tokens), expected);
    }

    /// Jalon Vert — fixture `Field` seul : `{{ entity.field }}` produit la
    /// même structure sous les deux parseurs.
    #[test]
    fn runtime_subset_matches_parse_tokens_field_only() {
        let src = "{{ user.name }}";

        let expected = parse_tokens(scan(src)).expect("parse_tokens (référence) doit réussir");
        let actual =
            parse_page_tokens(scan(src)).expect("parse_page_tokens (classifieur) doit réussir");

        assert_eq!(strip_runtime_envelope(actual.tokens), expected);
    }

    /// Jalon Vert — fixture `IfBool`/`EndIf` : un bloc conditionnel complet
    /// produit la même structure sous les deux parseurs.
    #[test]
    fn runtime_subset_matches_parse_tokens_if_endif() {
        let src = "{% if user.active %}yes{% endif %}";

        let expected = parse_tokens(scan(src)).expect("parse_tokens (référence) doit réussir");
        let actual =
            parse_page_tokens(scan(src)).expect("parse_page_tokens (classifieur) doit réussir");

        assert_eq!(strip_runtime_envelope(actual.tokens), expected);
    }

    /// Jalon Vert — un mot-clé structurellement exclu de la grammaire Mode
    /// Page (`include`) échoue explicitement plutôt que d'être
    /// silencieusement accepté ou ignoré — comportement documenté, pas un
    /// effet de bord. Ni `extends` (sorti en Phase 4.6, position jugée par
    /// `ExtendsNotFirst`) ni `for` (capturé sous `Unsupported` depuis le
    /// catch-all de la Phase 4.7, cf. `tests_phase_4_7_unsupported_catch_all`)
    /// n'illustrent plus cet invariant : `include` est désormais le seul
    /// mot-clé qui échoue encore ici, de façon définitive — cf. doc de
    /// `parse_page_block` (Phase 4.7, exclusion explicite du catch-all).
    #[test]
    fn composition_keyword_out_of_scope_fails_explicitly() {
        let src = r#"{% include fragment.html %}"#;
        let result = parse_page_tokens(scan(src));
        assert_eq!(result, Err(PageComposeParseError::InvalidBlockSequence));
    }
}

// =============================================================================
// Tests — Phase 4.4
// =============================================================================

#[cfg(test)]
mod tests_phase_4_4_block_endblock {
    use super::{FlatPageToken, PageBlockToken, PageSourceToken, parse_page_tokens, scan};

    /// Jalon Vert — template à 1 bloc top-level : `{% block name %}` produit
    /// exactement `BlockOpen { name }`, `{% endblock %}` produit exactement
    /// `BlockEnd`, le contenu intermédiaire reste `Runtime` inchangé.
    #[test]
    fn single_top_level_block_produces_block_open_and_block_end() {
        let src = "{% block header %}content{% endblock %}";

        let actual = parse_page_tokens(scan(src))
            .expect("parse_page_tokens doit réussir sur un bloc bien formé");

        assert_eq!(
            actual.tokens,
            vec![
                PageSourceToken::Block(PageBlockToken::BlockOpen { name: "header" }),
                PageSourceToken::Runtime(FlatPageToken::Static("content")),
                PageSourceToken::Block(PageBlockToken::BlockEnd),
            ]
        );
    }

    /// Jalon Vert — blocs imbriqués : preuve explicite de non-rejet à ce
    /// stade (Document 1 §4/§6 — l'appariement et l'absence d'imbrication
    /// ne sont pas des garanties du Parser). Le classifieur n'inspecte
    /// aucune pile d'état ; il reproduit fidèlement chaque marqueur
    /// rencontré, y compris quand un `{% block %}` s'ouvre alors qu'un autre
    /// est déjà ouvert.
    #[test]
    fn nested_blocks_parse_succeeds() {
        let src = "{% block outer %}{% block inner %}x{% endblock %}{% endblock %}";

        let actual = parse_page_tokens(scan(src))
            .expect("parse_page_tokens doit réussir sur des blocs imbriqués");

        assert_eq!(
            actual.tokens,
            vec![
                PageSourceToken::Block(PageBlockToken::BlockOpen { name: "outer" }),
                PageSourceToken::Block(PageBlockToken::BlockOpen { name: "inner" }),
                PageSourceToken::Runtime(FlatPageToken::Static("x")),
                PageSourceToken::Block(PageBlockToken::BlockEnd),
                PageSourceToken::Block(PageBlockToken::BlockEnd),
            ]
        );
    }
}

// =============================================================================
// Tests — Phase 4.5
// =============================================================================

#[cfg(test)]
mod tests_phase_4_5_static {
    use super::{FlatPageToken, PageSourceToken, StaticPartialRef, parse_page_tokens, scan};

    /// Jalon Vert (roadmap §4.5) — un chemin syntaxiquement valide mais
    /// absent du disque est accepté : cette fonction ne fait aucune E/S,
    /// donc l'existence réelle du fichier n'a aucune incidence sur le
    /// résultat. Aucune fixture sur disque n'est créée pour ce test — la
    /// chaîne de chemin est arbitraire, exactement comme le prescrit la
    /// roadmap ; c'est la preuve positive de l'absence d'E/S, pas seulement
    /// une absence de panique.
    #[test]
    fn static_path_parses_without_touching_filesystem() {
        let src = "before{% static this/path/does/not/exist.html %}after";

        let actual = parse_page_tokens(scan(src))
            .expect("parse_page_tokens doit réussir sans vérifier l'existence du fichier");

        assert_eq!(
            actual.tokens,
            vec![
                PageSourceToken::Runtime(FlatPageToken::Static("before")),
                PageSourceToken::Static(StaticPartialRef {
                    original_path: "this/path/does/not/exist.html",
                }),
                PageSourceToken::Runtime(FlatPageToken::Static("after")),
            ]
        );
    }
}

// =============================================================================
// Tests — Phase 4.6
// =============================================================================

#[cfg(test)]
mod tests_phase_4_6_extends_position {
    use super::{FlatPageToken, PageComposeParseError, PageSourceToken, parse_page_tokens, scan};

    /// Jalon Vert (roadmap §4.6) — `extends` en tête de fichier est capturé
    /// dans `ParsedPageTemplate::extends`, et n'apparaît jamais dans
    /// `tokens` (cf. doc du type : `extends` est un champ séparé, pas une
    /// variante de `PageSourceToken`).
    #[test]
    fn extends_at_head_is_captured_and_absent_from_tokens() {
        let src = "{% extends base.marius %}content";

        let actual = parse_page_tokens(scan(src))
            .expect("parse_page_tokens doit réussir avec extends en tête");

        assert_eq!(actual.extends, Some("base.marius"));
        assert_eq!(
            actual.tokens,
            vec![PageSourceToken::Runtime(FlatPageToken::Static("content"))]
        );
    }

    /// Jalon Vert (roadmap §4.6) — `extends` rencontré après un autre token
    /// (ici un `Static` de type HTML verbatim) échoue avec
    /// `ExtendsNotFirst` : la position de tête est une propriété du fichier
    /// entier, pas seulement du premier bloc `{% %}` rencontré.
    #[test]
    fn extends_after_a_static_token_fails_with_extends_not_first() {
        let src = "leading text{% extends base.marius %}";

        let result = parse_page_tokens(scan(src));

        assert_eq!(result, Err(PageComposeParseError::ExtendsNotFirst));
    }

    /// Jalon Vert (roadmap §4.6) — un fichier parent, sans aucun `extends`,
    /// réussit avec `extends == None` : l'absence de la déclaration n'est
    /// pas une erreur, Document 1 §3 l'admet explicitement comme cas normal.
    #[test]
    fn absent_extends_on_parent_file_succeeds_with_none() {
        let src = "{% block header %}content{% endblock %}";

        let actual =
            parse_page_tokens(scan(src)).expect("parse_page_tokens doit réussir sans extends");

        assert_eq!(actual.extends, None);
    }
}

// =============================================================================
// Tests — Phase 4.7
// =============================================================================

#[cfg(test)]
mod tests_phase_4_7_unsupported_catch_all {
    use super::{PageSourceToken, parse_page_tokens, scan};

    /// Jalon Vert (roadmap §4.7) — paramétré sur `for`, `join`, `where`,
    /// `filter`, `group`, et un mot-clé arbitraire inconnu : chacun produit
    /// `Unsupported { keyword, .. }` avec le bon `keyword`, jamais un rejet
    /// générique (`InvalidBlockSequence`) ni un rejet silencieux.
    #[test]
    fn unsupported_catch_all_captures_arbitrary_keywords() {
        let keywords = ["for", "join", "where", "filter", "group", "frobnicate"];

        for keyword in keywords {
            let src = format!("{{% {keyword} arg %}}");
            let actual = parse_page_tokens(scan(&src)).unwrap_or_else(|e| {
                panic!("mot-clé {keyword:?} doit être capturé, pas rejeté (erreur : {e:?})")
            });

            assert_eq!(
                actual.tokens.len(),
                1,
                "mot-clé {keyword:?} : un seul token attendu dans le flux"
            );
            match actual.tokens[0] {
                PageSourceToken::Unsupported {
                    keyword: got_keyword,
                    ..
                } => assert_eq!(got_keyword, keyword, "keyword capturé incorrect"),
                other => panic!("mot-clé {keyword:?} : attendu Unsupported, obtenu {other:?}"),
            }
        }
    }
}

#[cfg(test)]
mod tests_phase_5_1_page_arena {
    use super::{PageArena, ParsedPageTemplate};

    /// Jalon Vert (roadmap §5.1) — deux admissions successives produisent
    /// deux `TemplateId` distincts : l'identité est assignée par position
    /// de poussée, jamais réutilisée ni partagée entre deux fichiers admis.
    #[test]
    fn admit_twice_yields_distinct_template_ids() {
        let mut arena = PageArena::default();
        let child = ParsedPageTemplate {
            extends: Some("parent.marius"),
            tokens: Vec::new(),
        };
        let parent = ParsedPageTemplate {
            extends: None,
            tokens: Vec::new(),
        };

        let child_id = arena.admit(child);
        let parent_id = arena.admit(parent);

        assert_ne!(
            child_id, parent_id,
            "deux admissions distinctes doivent produire deux TemplateId distincts"
        );
    }

    /// Jalon Vert (roadmap §5.1) — `get` après `admit` retourne le contenu
    /// inséré inchangé (égalité de valeur complète, pas seulement des
    /// tokens) : l'arène ne copie ni ne transforme rien à l'admission.
    #[test]
    fn get_after_admit_returns_unchanged_content() {
        let mut arena = PageArena::default();
        let original = ParsedPageTemplate {
            extends: Some("parent.marius"),
            tokens: Vec::new(),
        };
        let expected = original.clone();

        let id = arena.admit(original);

        assert_eq!(
            arena.get(id),
            &expected,
            "le contenu retourné par get doit être identique à celui admis"
        );
    }
}

// =============================================================================
// PHASE 5.2 — `collect_blocks` : cas non imbriqué (Document 2 §3)
// =============================================================================
// Responsabilité (roadmap §5.2) : apparier, par pile, les `BlockOpen`/
// `BlockEnd` d'**un** fichier déjà admis en arène, et produire les
// `NamedBlockRange` correspondantes.
//
// ─── Choix explicite sur les catégories de cas hors périmètre 5.2 ─────────
// (roadmap §5.2 : « à choisir explicitement, pas laisser un todo! silencieux »)
//
//   1. `PageSourceToken::Unsupported` (mots-clés `for`/`join`/`where`/…) :
//      à ce stade (5.2/5.3), traité comme du contenu opaque par la branche
//      `_` — ignoré par la boucle d'appariement, ne produit aucune erreur.
//      Retour `Ok` systématique tant qu'aucun bloc n'est mal apparié : c'est
//      la variante « retour Ok uniquement pour l'instant » explicitement
//      choisie parmi les deux proposées par la roadmap. La Phase 5.4
//      ci-dessous remplace cette branche par le mapping nommé vers
//      `PageValidationError::ForLoopDetected`/`RelationalKeyword`.
//
//   2. Profondeur d'imbrication > 1 : couverte depuis la Phase 5.3
//      ci-dessous (`NestedBlock`) — plus un point hors périmètre depuis ce
//      diff.
//
// ─── Point ouvert, non tranché par ce diff ─────────────────────────────────
//
//   Un flux structurellement mal formé au sens de l'appariement lui-même —
//   `BlockEnd` sans `BlockOpen` correspondant, ou `BlockOpen` non refermé en
//   fin de flux — n'est PAS un cas couvert par le chemin heureux testé ici,
//   et n'est représenté par aucune variante existante de
//   `PageValidationError` (`NonBoolIfCondition`, `ForLoopDetected`,
//   `RelationalKeyword`, `NestedBlock` : aucune ne nomme un déséquilibre
//   structurel). Introduire une nouvelle variante pour ce cas dépasserait le
//   périmètre de cette phase (« ne préparer aucun comportement relevant des
//   phases ultérieures »). Choix retenu : un `panic!` documenté, nommé,
//   assorti d'un message explicite — jamais un `todo!`/`unimplemented!` muet
//   — sur une entrée que les fixtures testées à ce stade ne produisent
//   jamais. À trancher explicitement dans une session ultérieure, au même
//   titre que le point ouvert déjà signalé au Document 2 §6.1.
//
// =============================================================================
// PHASE 5.3 — `collect_blocks` : détection `NestedBlock` (Document 2 §3)
// =============================================================================
// Extension de 5.2 (roadmap §5.3) : une seule condition ajoutée dans la
// boucle existante, aucune restructuration de la pile. Invariant introduit :
// l'imbrication est rejetée nommément, jamais acceptée comme plage valide.
//
// ─── Mécanisme ──────────────────────────────────────────────────────────
//
//   La pile LIFO appariait déjà correctement n'importe quelle profondeur
//   (propriété algorithmique de 5.2, documentée dans son commentaire de
//   tête). Cette phase n'ajoute donc aucune capacité d'appariement — elle
//   ajoute une *interdiction* : si `open_stack` est déjà non-vide au moment
//   d'empiler un nouveau `BlockOpen`, ce `BlockOpen` est en position
//   imbriquée, ce qui produit `PageValidationError::NestedBlock { name }`
//   (`name` du bloc imbriqué fautif, pas du bloc englobant — c'est
//   l'occurrence la plus profonde qui viole la contrainte de platitude).
//
// ─── Fail-slow, pas fail-fast ────────────────────────────────────────────
//
//   L'empilement continue malgré l'erreur détectée (`open_stack.push`
//   n'est jamais court-circuité) : la boucle va jusqu'au bout du flux,
//   accumulant une erreur par `BlockOpen` en position imbriquée. Ce choix
//   anticipe la vérification fail-slow prescrite en Phase 5.4 (« 2 erreurs
//   simultanées → `Vec` de longueur 2 ») sans l'implémenter par avance :
//   c'est une conséquence directe et minimale de « ne jamais interrompre la
//   boucle sur une erreur nommée », pas un branchement additionnel préparé
//   pour 5.4.
//
// ─── Pas de sortie mixte succès/erreur ───────────────────────────────────
//
//   `ranges` continue d'être peuplé même en présence d'erreurs (nécessaire
//   pour que chaque `BlockEnd` trouve un `start` à dépiler), mais n'est
//   jamais retourné si `errors` est non vide : la fonction retourne
//   `Err(errors)` ou `Ok(ranges)`, jamais les deux à la fois. Les plages
//   calculées en présence d'imbrication sont donc délibérément jetées, pas
//   exposées comme un résultat partiellement fiable.
//
// =============================================================================
// PHASE 5.4 — `collect_blocks` : `ForLoopDetected` / `RelationalKeyword` (Document 2 §3)
// =============================================================================
// Extension de 5.2/5.3 (roadmap §5.4) : une seule branche de `match` ajoutée,
// aucune logique de pile touchée. Invariant introduit : mapping total et
// nommé entre mot-clé `Unsupported` et erreur de validation — plus aucun
// mot-clé `Unsupported` ne peut traverser `collect_blocks` sans produire une
// erreur nommée (le point 1 de la doc de tête, ci-dessus, est donc clos).
//
// ─── Règle du mapping ──────────────────────────────────────────────────────
//
//   `PageSourceToken::Unsupported { keyword, .. }` :
//     - `keyword == "for"`      → `PageValidationError::ForLoopDetected`
//     - tout autre `keyword`    → `PageValidationError::RelationalKeyword { keyword }`
//
//   Ce n'est pas une énumération explicite des mots-clés relationnels connus
//   (`join`/`where`/`filter`/`group`) suivie d'un troisième cas silencieux :
//   c'est un mapping *total* sur le seul axe qui compte ici — `for` est
//   distingué parce que `PageValidationError` lui réserve une variante sans
//   charge utile, tout le reste (relationnel connu ou mot-clé futur non
//   encore nommé par la grammaire, cf. le catch-all Phase 4.7 déjà total sur
//   `keyword: &str` arbitraire) tombe dans `RelationalKeyword`, qui porte le
//   `keyword` reçu tel quel. Aucun `keyword` ne peut donc rester non
//   catégorisé — propriété vérifiée par construction (deux branches
//   exhaustives sur un `bool`), pas par une liste à maintenir.
//
// ─── Fail-slow, orthogonal à `NestedBlock` ─────────────────────────────────
//
//   Cette branche ne fait pas partie de la pile d'appariement (`open_stack`
//   n'est ni lu ni modifié) : un mot-clé `Unsupported` peut coexister avec un
//   bloc imbriqué dans le même flux, chacun poussant sa propre erreur dans
//   `errors` sans interférence — même politique fail-slow que 5.3, sur un axe
//   de validation indépendant.
pub fn collect_blocks<'src>(
    template: TemplateId,
    tokens: &[PageSourceToken<'src>],
) -> Result<Vec<NamedBlockRange<'src>>, Vec<PageValidationError<'src>>> {
    let mut open_stack: Vec<(&'src str, usize)> = Vec::new();
    let mut ranges = Vec::new();
    let mut errors = Vec::new();

    for (index, token) in tokens.iter().enumerate() {
        match token {
            // Ouverture : empile `(name, start)`. `start` pointe juste après
            // le marqueur `BlockOpen` lui-même — la plage couvre le contenu
            // du bloc, jamais ses délimiteurs (convention actée par la doc
            // de `NamedBlockRange`). Une pile déjà non-vide à cet instant
            // signale une imbrication (Phase 5.3) : erreur accumulée,
            // empilement néanmoins poursuivi (fail-slow, cf. doc de tête).
            PageSourceToken::Block(PageBlockToken::BlockOpen { name }) => {
                if !open_stack.is_empty() {
                    errors.push(PageValidationError::NestedBlock { name });
                }
                open_stack.push((name, index + 1));
            }
            // Fermeture : dépile et matérialise la plage `[start, index)`,
            // `index` (position du `BlockEnd`) exclusif — même convention.
            PageSourceToken::Block(PageBlockToken::BlockEnd) => {
                let (name, start) = open_stack.pop().unwrap_or_else(|| {
                    panic!(
                        "collect_blocks (Phase 5.2) : BlockEnd sans BlockOpen \
                         correspondant à l'index {index} — cas mal formé hors \
                         périmètre du chemin heureux, non représenté par \
                         PageValidationError à ce stade (voir doc de tête)"
                    )
                });
                ranges.push(NamedBlockRange {
                    name,
                    template,
                    start,
                    end: index,
                });
            }
            // Mot-clé de grammaire non supporté (Phase 5.4, cf. doc de tête) :
            // mapping total vers l'erreur de validation nommée
            // correspondante. N'interagit pas avec `open_stack` — orthogonal
            // à l'appariement de blocs, fail-slow au même titre que
            // `NestedBlock` ci-dessus.
            PageSourceToken::Unsupported { keyword, .. } => {
                if *keyword == "for" {
                    errors.push(PageValidationError::ForLoopDetected);
                } else {
                    errors.push(PageValidationError::RelationalKeyword { keyword });
                }
            }
            // Tout le reste (`Runtime`, `Static`) est du contenu opaque du
            // point de vue de l'appariement de blocs — ni poussé ni dépilé.
            _ => {}
        }
    }

    assert!(
        open_stack.is_empty(),
        "collect_blocks (Phase 5.2) : {} bloc(s) BlockOpen non refermé(s) en \
         fin de flux — cas mal formé hors périmètre du chemin heureux, non \
         représenté par PageValidationError à ce stade (voir doc de tête)",
        open_stack.len()
    );

    if errors.is_empty() {
        Ok(ranges)
    } else {
        Err(errors)
    }
}

// =============================================================================
// Tests — Phase 5.2
// =============================================================================

#[cfg(test)]
mod tests_phase_5_2_collect_blocks {
    use super::{
        FlatPageToken, NamedBlockRange, PageBlockToken, PageSourceToken, TemplateId, collect_blocks,
    };

    /// Jalon Vert (roadmap §5.2) — deux blocs top-level (non imbriqués)
    /// produisent exactement deux `NamedBlockRange`, aux indices exacts de
    /// contenu (bornes `[start, end)` excluant les marqueurs `BlockOpen`/
    /// `BlockEnd` eux-mêmes, pas seulement au nombre de plages retournées).
    #[test]
    fn two_top_level_blocks_produce_exact_ranges() {
        let template = TemplateId(0);
        let tokens = vec![
            PageSourceToken::Block(PageBlockToken::BlockOpen { name: "a" }),
            PageSourceToken::Runtime(FlatPageToken::Static("x")),
            PageSourceToken::Block(PageBlockToken::BlockEnd),
            PageSourceToken::Block(PageBlockToken::BlockOpen { name: "b" }),
            PageSourceToken::Runtime(FlatPageToken::Static("y")),
            PageSourceToken::Block(PageBlockToken::BlockEnd),
        ];

        let ranges = collect_blocks(template, &tokens).expect("chemin heureux attendu");

        assert_eq!(
            ranges,
            vec![
                NamedBlockRange {
                    name: "a",
                    template,
                    start: 1,
                    end: 2,
                },
                NamedBlockRange {
                    name: "b",
                    template,
                    start: 4,
                    end: 5,
                },
            ]
        );
    }
}

// =============================================================================
// Tests — Phase 5.3
// =============================================================================

#[cfg(test)]
mod tests_phase_5_3_nested_block_detection {
    use super::{
        FlatPageToken, PageBlockToken, PageSourceToken, PageValidationError, TemplateId,
        collect_blocks,
    };

    /// Jalon Vert (roadmap §5.3) — un bloc imbriqué produit
    /// `Err(vec![NestedBlock { name: "inner" }])` : le nom rapporté est celui
    /// du bloc fautif (le plus profond), pas du bloc englobant. Le typage en
    /// `Result` exclut par construction toute sortie mixte : ce test
    /// documente cette absence de mélange succès/erreur en assertant
    /// directement sur la variante `Err`, sans exposer de plage à côté.
    #[test]
    fn nested_block_produces_named_error() {
        let template = TemplateId(0);
        let tokens = vec![
            PageSourceToken::Block(PageBlockToken::BlockOpen { name: "outer" }),
            PageSourceToken::Block(PageBlockToken::BlockOpen { name: "inner" }),
            PageSourceToken::Runtime(FlatPageToken::Static("x")),
            PageSourceToken::Block(PageBlockToken::BlockEnd),
            PageSourceToken::Block(PageBlockToken::BlockEnd),
        ];

        let result = collect_blocks(template, &tokens);

        assert_eq!(
            result,
            Err(vec![PageValidationError::NestedBlock { name: "inner" }])
        );
    }
}

// =============================================================================
// Tests — Phase 5.4
// =============================================================================

#[cfg(test)]
mod tests_phase_5_4_unsupported_mapping {
    use super::{PageSourceToken, PageValidationError, TemplateId, collect_blocks};

    /// Jalon Vert (roadmap §5.4) — `for` produit nommément `ForLoopDetected`,
    /// jamais `RelationalKeyword`. Cas distingué du reste par construction
    /// (cf. doc de tête de `collect_blocks`, section Phase 5.4).
    #[test]
    fn for_keyword_produces_for_loop_detected() {
        let template = TemplateId(0);
        let tokens = vec![PageSourceToken::Unsupported {
            keyword: "for",
            tail: " item in items",
        }];

        let result = collect_blocks(template, &tokens);

        assert_eq!(result, Err(vec![PageValidationError::ForLoopDetected]));
    }

    /// Jalon Vert (roadmap §5.4) — chacun des mots-clés relationnels connus
    /// (`join`/`where`/`filter`/`group`) produit nommément
    /// `RelationalKeyword { keyword }`, avec le `keyword` reçu tel quel.
    /// Paramétré, comme le catch-all Parser (Phase 4.7) dont cette
    /// validation est le pendant côté `collect_blocks`.
    #[test]
    fn relational_keywords_produce_relational_keyword_error() {
        let template = TemplateId(0);

        for keyword in ["join", "where", "filter", "group"] {
            let tokens = vec![PageSourceToken::Unsupported { keyword, tail: "" }];

            let result = collect_blocks(template, &tokens);

            assert_eq!(
                result,
                Err(vec![PageValidationError::RelationalKeyword { keyword }]),
                "mot-clé {keyword:?} : erreur RelationalKeyword attendue"
            );
        }
    }

    /// Jalon Vert (roadmap §5.4) — le mapping est *total*, pas une liste
    /// fermée sur les quatre mots-clés relationnels connus : un mot-clé
    /// arbitraire non listé (mais déjà capturé par le catch-all Phase 4.7,
    /// cf. `unsupported_catch_all_captures_arbitrary_keywords`) tombe aussi
    /// dans `RelationalKeyword`, jamais silencieusement ignoré.
    #[test]
    fn arbitrary_unsupported_keyword_also_produces_relational_keyword_error() {
        let template = TemplateId(0);
        let tokens = vec![PageSourceToken::Unsupported {
            keyword: "frobnicate",
            tail: " arg",
        }];

        let result = collect_blocks(template, &tokens);

        assert_eq!(
            result,
            Err(vec![PageValidationError::RelationalKeyword {
                keyword: "frobnicate"
            }])
        );
    }

    /// Jalon Vert (roadmap §5.4) — fail-slow vérifié : deux mots-clés
    /// `Unsupported` dans le même flux produisent un `Vec` de longueur 2,
    /// pas une sortie fail-fast qui s'arrêterait à la première erreur.
    #[test]
    fn two_unsupported_keywords_in_same_stream_accumulate_both_errors() {
        let template = TemplateId(0);
        let tokens = vec![
            PageSourceToken::Unsupported {
                keyword: "for",
                tail: "",
            },
            PageSourceToken::Unsupported {
                keyword: "where",
                tail: "",
            },
        ];

        let result = collect_blocks(template, &tokens);

        assert_eq!(
            result,
            Err(vec![
                PageValidationError::ForLoopDetected,
                PageValidationError::RelationalKeyword { keyword: "where" },
            ])
        );
    }
}

// =============================================================================
// PHASE 5.5 — `link` : appariement sans E/S (Document 2 §4)
// =============================================================================
// Responsabilité (roadmap §5.5) : répondre à des questions de correspondance
// *par référence*, sans muter aucune structure — pour chaque plage du
// parent, quelle est la substitution retenue (plage enfant si redéfinition
// de même nom, plage parent sinon) ? Toute plage enfant sans correspondance
// côté parent est un bloc orphelin. Fonction pure modulo E/S injectée (la
// vérification `static`, ci-dessous, Phase 5.6, ne branche que la fonction
// `file_exists` reçue — aucun `std::fs` direct dans ce module).
//
// ─── Décision de signature (roadmap §5.5, point explicitement laissé ouvert) ─
//
//   La roadmap propose deux options : signature réduite à
//   `(parent_blocks, child_blocks)` avec re-signature en 5.6, ou signature
//   complète dès 5.5 avec `static_refs`/`file_exists` présents mais non
//   utilisés. La roadmap recommande explicitement la seconde (« pour ne pas
//   re-signer la fonction en 5.6 ») — retenue en 5.5. Confirmé par 5.6
//   ci-dessous : la signature n'a pas bougé, seul le corps a gagné une
//   boucle.
//
// ─── Règle de construction du plan ──────────────────────────────────────────
//
//   Pour chaque plage du parent (ordre de parcours = ordre du parent) : la
//   substitution retenue est celle de l'enfant si un nom identique existe
//   côté enfant, sinon celle du parent lui-même (comportement par défaut —
//   Document 2 §4). Conséquence directe : `substitutions.len() ==
//   parent_blocks.len()` est un invariant de complétude, vérifié par
//   construction (une itération, une poussée, jamais de `continue` qui
//   sauterait une plage parent) — pas seulement par les tests.
//
//   Toute plage de l'enfant qui ne correspond à aucun nom du parent est un
//   `PageLinkError::OrphanBlock` — jamais silencieusement ignorée. Boucle
//   séparée de la construction du plan (deux responsabilités disjointes du
//   même contrat : « quelle substitution » vs. « quel enfant est
//   orphelin »), fail-slow comme `collect_blocks` : les deux boucles vont
//   jusqu'au bout, `substitutions` est entièrement construit même si
//   `errors` est non vide, mais seul l'un des deux est retourné.
// =============================================================================
// PHASE 5.6 — `link` : vérification `static` (Document 2 §4)
// =============================================================================
// Extension de 5.5 (roadmap §5.6) : une boucle ajoutée, aucune modification
// de la logique de blocs (construction du plan, détection `OrphanBlock`
// inchangées ligne à ligne). Invariant introduit : existence de fichier
// vérifiée via E/S injectée (`file_exists: impl Fn(&str) -> bool`), jamais
// via `std::fs` direct — la fonction reste testable sans FS réel, seule la
// fermeture passée par l'appelant décide de ce qu'« exister » signifie.
//
// ─── Mécanisme ──────────────────────────────────────────────────────────
//
//   Une troisième boucle, sur `static_refs` : pour chaque
//   `StaticPartialRef { original_path }`, `file_exists(original_path)` est
//   interrogé. `false` → `PageLinkError::StaticFileNotFound { path:
//   original_path }` poussée dans le même `errors` que `OrphanBlock` — un
//   seul `Vec` d'erreurs pour les deux axes de validation du Linker, fidèle
//   au fail-slow déjà en place : aucune des trois boucles (substitution,
//   orphelin, static) n'interrompt les autres.
//
// ─── Duplication d'E/S assumée (Document 2 §4) ─────────────────────────────
//
//   `file_exists` ici est distinct de la lecture de taille que fera plus
//   tard le Resolver (Document 3 §3, `get_file_size`) : deux fonctions
//   injectées, deux contextes de phase, pas de mutualisation prématurée —
//   décision déjà actée par le document d'architecture, appliquée sans
//   écart.
//
// ─── `link` clos (Document 2 §4 terminé) ───────────────────────────────────
//
//   Les trois erreurs de `PageLinkError` (`ExtendsNotFound`, `OrphanBlock`,
//   `StaticFileNotFound`) ont chacune leur point d'émission : `OrphanBlock`
//   et `StaticFileNotFound` dans `link` (ce module), `ExtendsNotFound` dans
//   l'orchestrateur (Document 3, hors périmètre — résolution du chemin
//   `extends` lui-même, pas une correspondance de blocs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockSubstitution<'src> {
    /// Nom du bloc, identique à `NamedBlockRange::name` du parent
    /// correspondant. Dupliqué depuis `source` pour que le Lowering
    /// (Phase 5.8+) puisse itérer sur `substitutions` sans redériver le nom
    /// depuis une plage dont l'origine (enfant ou parent) varie.
    pub name: &'src str,
    /// Plage de contenu retenue : plage enfant si override, plage parent
    /// sinon. Porte son propre `TemplateId` (`NamedBlockRange::template`) —
    /// c'est ce champ, pas `name`, qui indique dans quel AST le Lowering
    /// devra lire le contenu substitué.
    pub source: NamedBlockRange<'src>,
}

/// Plan de fusion produit par `link` : une substitution par bloc du parent,
/// dans l'ordre du parent. Type de données pur — aucune méthode de fusion
/// ici, c'est le rôle du Lowering (Document 2 §5, Phase 5.8+).
///
/// `substitutions.len() == parent_blocks.len()` est un invariant de ce type
/// produit par `link` (voir doc de tête ci-dessus) — pas revérifié à la
/// construction (pas de constructeur dédié : le champ est public, produit
/// uniquement par `link` dans ce module à ce stade).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkPlan<'src> {
    pub substitutions: Vec<BlockSubstitution<'src>>,
}

/// Calcule le plan de fusion entre les blocs d'un parent et ceux d'un
/// enfant (correspondance par nom), et vérifie l'existence de chaque
/// fichier `{% static %}` référencé via `file_exists` — aucune E/S directe
/// dans cette fonction, aucune mutation des tranches reçues. Voir doc de
/// tête (Phases 5.5/5.6) pour la règle de construction du plan et le
/// mécanisme de vérification `static`.
pub fn link<'src>(
    parent_blocks: &[NamedBlockRange<'src>],
    child_blocks: &[NamedBlockRange<'src>],
    static_refs: &[StaticPartialRef<'src>],
    file_exists: impl Fn(&str) -> bool,
) -> Result<LinkPlan<'src>, Vec<PageLinkError<'src>>> {
    let mut substitutions = Vec::with_capacity(parent_blocks.len());
    for parent_range in parent_blocks {
        let source = child_blocks
            .iter()
            .find(|child_range| child_range.name == parent_range.name)
            .copied()
            .unwrap_or(*parent_range);
        substitutions.push(BlockSubstitution {
            name: parent_range.name,
            source,
        });
    }

    let mut errors = Vec::new();
    for child_range in child_blocks {
        let has_matching_parent = parent_blocks
            .iter()
            .any(|parent_range| parent_range.name == child_range.name);
        if !has_matching_parent {
            errors.push(PageLinkError::OrphanBlock {
                name: child_range.name,
            });
        }
    }

    for static_ref in static_refs {
        if !file_exists(static_ref.original_path) {
            errors.push(PageLinkError::StaticFileNotFound {
                path: static_ref.original_path,
            });
        }
    }

    if errors.is_empty() {
        Ok(LinkPlan { substitutions })
    } else {
        Err(errors)
    }
}

// =============================================================================
// Tests — Phase 5.5
// =============================================================================

#[cfg(test)]
mod tests_phase_5_5_link {
    use super::{LinkPlan, NamedBlockRange, PageLinkError, TemplateId, link};

    fn range(name: &str, template: TemplateId, start: usize, end: usize) -> NamedBlockRange<'_> {
        NamedBlockRange {
            name,
            template,
            start,
            end,
        }
    }

    /// Jalon Vert (roadmap §5.5) — un bloc enfant de même nom qu'un bloc
    /// parent est retenu comme substitution (override), la plage source
    /// pointant vers l'enfant, pas vers le parent.
    #[test]
    fn child_override_replaces_parent_range() {
        let parent_template = TemplateId(0);
        let child_template = TemplateId(1);
        let parent_blocks = vec![range("title", parent_template, 0, 3)];
        let child_blocks = vec![range("title", child_template, 10, 20)];

        let plan = link(&parent_blocks, &child_blocks, &[], |_| true).expect("pas d'orphelin");

        assert_eq!(
            plan,
            LinkPlan {
                substitutions: vec![super::BlockSubstitution {
                    name: "title",
                    source: range("title", child_template, 10, 20),
                }],
            }
        );
    }

    /// Jalon Vert (roadmap §5.5) — un bloc parent sans redéfinition côté
    /// enfant conserve son propre contenu (fallback par défaut, Document 2
    /// §4).
    #[test]
    fn parent_range_kept_when_no_override() {
        let parent_template = TemplateId(0);
        let parent_blocks = vec![range("footer", parent_template, 5, 8)];
        let child_blocks: Vec<NamedBlockRange<'_>> = Vec::new();

        let plan = link(&parent_blocks, &child_blocks, &[], |_| true).expect("pas d'orphelin");

        assert_eq!(
            plan,
            LinkPlan {
                substitutions: vec![super::BlockSubstitution {
                    name: "footer",
                    source: range("footer", parent_template, 5, 8),
                }],
            }
        );
    }

    /// Jalon Vert (roadmap §5.5) — un bloc enfant sans correspondance côté
    /// parent produit `OrphanBlock`, jamais une substitution silencieuse.
    #[test]
    fn child_block_without_parent_match_is_orphan() {
        let parent_template = TemplateId(0);
        let child_template = TemplateId(1);
        let parent_blocks = vec![range("title", parent_template, 0, 3)];
        let child_blocks = vec![range("sidebar", child_template, 0, 3)];

        let result = link(&parent_blocks, &child_blocks, &[], |_| true);

        assert_eq!(
            result,
            Err(vec![PageLinkError::OrphanBlock { name: "sidebar" }])
        );
    }

    /// Jalon Vert (roadmap §5.5) — invariant de complétude : une entrée de
    /// plan par bloc parent, jamais moins, quel que soit le nombre de blocs
    /// enfant (redéfinis ou non).
    #[test]
    fn substitutions_len_always_equals_parent_blocks_len() {
        let parent_template = TemplateId(0);
        let child_template = TemplateId(1);
        let parent_blocks = vec![
            range("a", parent_template, 0, 1),
            range("b", parent_template, 2, 3),
            range("c", parent_template, 4, 5),
        ];
        let child_blocks = vec![range("b", child_template, 10, 11)];

        let plan = link(&parent_blocks, &child_blocks, &[], |_| true).expect("pas d'orphelin");

        assert_eq!(plan.substitutions.len(), parent_blocks.len());
    }
}

// =============================================================================
// Tests — Phase 5.6
// =============================================================================

#[cfg(test)]
mod tests_phase_5_6_link_static_check {
    use super::{NamedBlockRange, PageLinkError, StaticPartialRef, TemplateId, link};

    fn range(name: &str, template: TemplateId, start: usize, end: usize) -> NamedBlockRange<'_> {
        NamedBlockRange {
            name,
            template,
            start,
            end,
        }
    }

    /// Jalon Vert (roadmap §5.6) — `file_exists` renvoyant `false` produit
    /// `StaticFileNotFound`, le chemin porté étant celui reçu tel quel
    /// (aucune normalisation dans `link`).
    #[test]
    fn missing_static_file_produces_static_file_not_found() {
        let parent_blocks: Vec<NamedBlockRange<'_>> = Vec::new();
        let child_blocks: Vec<NamedBlockRange<'_>> = Vec::new();
        let static_refs = vec![StaticPartialRef {
            original_path: "nav.html",
        }];

        let result = link(&parent_blocks, &child_blocks, &static_refs, |_| false);

        assert_eq!(
            result,
            Err(vec![PageLinkError::StaticFileNotFound { path: "nav.html" }])
        );
    }

    /// Jalon Vert (roadmap §5.6) — `file_exists` renvoyant `true` ne
    /// produit aucune erreur : le plan est calculé normalement.
    #[test]
    fn existing_static_file_produces_no_error() {
        let parent_blocks: Vec<NamedBlockRange<'_>> = Vec::new();
        let child_blocks: Vec<NamedBlockRange<'_>> = Vec::new();
        let static_refs = vec![StaticPartialRef {
            original_path: "nav.html",
        }];

        let result = link(&parent_blocks, &child_blocks, &static_refs, |_| true);

        assert!(result.is_ok());
        assert!(result.unwrap().substitutions.is_empty());
    }

    /// Jalon Vert (roadmap §5.6) — fail-slow croisé sur les deux axes du
    /// Linker : un bloc enfant orphelin ET un fichier `static` manquant
    /// dans le même appel produisent un `Vec` de 2 erreurs, jamais une
    /// seule (pas d'interruption au premier axe en défaut).
    #[test]
    fn orphan_block_and_missing_static_file_accumulate_both_errors() {
        let parent_template = TemplateId(0);
        let child_template = TemplateId(1);
        let parent_blocks = vec![range("title", parent_template, 0, 3)];
        let child_blocks = vec![range("sidebar", child_template, 0, 3)];
        let static_refs = vec![StaticPartialRef {
            original_path: "missing.css",
        }];

        let result = link(&parent_blocks, &child_blocks, &static_refs, |_| false);

        assert_eq!(
            result,
            Err(vec![
                PageLinkError::OrphanBlock { name: "sidebar" },
                PageLinkError::StaticFileNotFound {
                    path: "missing.css"
                },
            ])
        );
    }
}

// =============================================================================
// PHASE 5.7 — `collect_static_refs` (Document 2 §4, alimentation de `link`)
// =============================================================================
// Responsabilité (roadmap §5.7) : extraire, sans omission, toutes les
// références `{% static %}` d'un flux `PageSourceToken` — fonction séparée,
// une seule responsabilité, pour alimenter le paramètre `static_refs` de
// `link` (Phase 5.6, déjà clos). Le câblage réel (quel flux — enfant, parent,
// ou les deux — est passé à `link` par l'orchestrateur) est hors périmètre
// de cette phase : Document 3.
//
// ─── Pourquoi une fonction séparée, pas une extension de `collect_blocks` ──
//
//   `collect_blocks` (Phase 5.2-5.4, Document 2 §3) a une seule
//   responsabilité déjà remplie : position des blocs et validation de forme
//   sans second fichier. Y ajouter la collecte `static` mélangerait deux
//   catégories de concept distinctes dans une même fonction (§0 : une
//   fonction, une catégorie de concept éliminée) — ici, « où sont les
//   blocs » et « où sont les références static » n'ont aucune donnée ni
//   aucun invariant en commun (pas de pile, pas d'appariement, pas d'erreur
//   de forme). Contrairement à la fusion actée pour `collect_blocks`
//   lui-même (construction de plage + validation de forme : même flux, même
//   ordre, même pile), aucune économie de parcours ne justifierait ici de
//   coupler les deux : un filtre `Static` et un appariement `BlockOpen`/
//   `BlockEnd` restent deux boucles indépendantes même fusionnées en une
//   seule passe physique, sans partage d'état — la séparation en deux
//   fonctions ne coûte donc aucune localité de cache supplémentaire.
//
// ─── Pas de déduplication (Document 2 §6.2, point ouvert non tranché ici) ──
//
//   Chaque occurrence de `{% static %}` dans le flux produit une entrée,
//   y compris si `original_path` est identique à une entrée déjà retournée.
//   Comportement identique à `{% include %}` en Mode Fragment (gelé) :
//   compter les occurrences réelles, pas les chemins distincts. La
//   déduplication cross-page évoquée par le scaffolding de `StaticPartialRef`
//   (partager un unique `static_partials::{IDENT}` entre plusieurs pages)
//   resterait hors de portée même avec une déduplication *intra*-flux ici —
//   c'est un problème d'orchestrateur sur plusieurs fichiers, pas un problème
//   de cette fonction sur un seul flux. Introduire un filtre de doublons
//   maintenant serait un comportement spéculatif non demandé par cette phase.
//
// ─── Complexité et mémoire ──────────────────────────────────────────────────
//
//   Une seule boucle sur `tokens`, `O(n)`. Aucun étage de recherche
//   (`HashSet`, tri) : la fonction ne compare jamais deux entrées entre
//   elles, elle projette uniquement. `Vec<StaticPartialRef<'src>>` alloué au
//   premier `push`, croissance linéaire — pas de capacité pré-allouée sur la
//   taille de `tokens` (le nombre de `Static` est généralement une faible
//   fraction du flux ; `Vec::with_capacity(tokens.len())` sur-allouerait dans
//   le cas courant sans bénéfice mesuré). `StaticPartialRef` est `Copy`,
//   copié depuis la slice sans indirection nouvelle.

/// Extrait, dans l'ordre du flux et sans déduplication, toutes les
/// références `{% static %}` de `tokens`. Filtre pur : ne consulte ni
/// `PageArena`, ni `LinkPlan`, ne fait aucune E/S. Voir doc de tête
/// (Phase 5.7) pour la justification de la séparation d'avec
/// `collect_blocks` et l'absence de déduplication.
pub fn collect_static_refs<'src>(tokens: &[PageSourceToken<'src>]) -> Vec<StaticPartialRef<'src>> {
    let mut refs = Vec::new();
    for token in tokens {
        if let PageSourceToken::Static(static_ref) = token {
            refs.push(*static_ref);
        }
    }
    refs
}

// =============================================================================
// Tests — Phase 5.7
// =============================================================================

#[cfg(test)]
mod tests_phase_5_7_collect_static_refs {
    use super::{FlatPageToken, PageSourceToken, StaticPartialRef, collect_static_refs};

    /// Jalon Vert (roadmap §5.7) — un flux portant 2 `Static`, dont 1 chemin
    /// dupliqué (`nav.html` apparaît deux fois), produit 2 entrées : la
    /// fonction compte les occurrences réelles, elle ne déduplique pas par
    /// valeur de `original_path` (Document 2 §6.2, comportement dégradé
    /// retenu comme contrat v1).
    #[test]
    fn duplicated_static_path_yields_two_entries_not_one() {
        let tokens = vec![
            PageSourceToken::Static(StaticPartialRef {
                original_path: "nav.html",
            }),
            PageSourceToken::Runtime(FlatPageToken::Static("between")),
            PageSourceToken::Static(StaticPartialRef {
                original_path: "nav.html",
            }),
        ];

        let refs = collect_static_refs(&tokens);

        assert_eq!(
            refs,
            vec![
                StaticPartialRef {
                    original_path: "nav.html"
                },
                StaticPartialRef {
                    original_path: "nav.html"
                },
            ]
        );
    }
}

// =============================================================================
// PHASE 5.8 — `lower` : projection sans substitution (Document 2 §5)
// =============================================================================
// Responsabilité (roadmap §5.8) : poser la signature finale du Lowering —
// `(&[PageSourceToken], &LinkPlan, &PageArena) -> Vec<FlatPageToken>` — et en
// implémenter le sous-ensemble exercé sans aucun bloc en entrée : projection
// `Runtime` → identité, `Static` → `StaticInclude` provisoire. La splice des
// plages substituées (`PageSourceToken::Block`, via `LinkPlan`/`PageArena`)
// est explicitement hors périmètre de cette phase — couverte en 5.9.
//
// ─── Pourquoi la signature complète dès maintenant ─────────────────────────
//
//   Même arbitrage que pour `link` en 5.5/5.6 (roadmap §5.5, doc de tête
//   ci-dessus) : la roadmap demande explicitement de poser `plan` et `arena`
//   dans la signature dès 5.8 pour que 5.9 étende le corps sans re-signer la
//   fonction. `plan`/`arena` ne sont pas encore lus par cette phase — voir
//   ci-dessous pour la raison pour laquelle ceci n'est pas un paramètre
//   spéculatif au sens interdit par la contrainte de phase : la signature
//   est un engagement de contrat déjà acté par Document 2 §5 et la roadmap,
//   pas une anticipation de logique non spécifiée.
//
// ─── Chemin heureux uniquement : `Block` hors périmètre, documenté ─────────
//
//   Cette phase ne reçoit, par contrat de test (roadmap §5.8 : « testée
//   uniquement sur un LinkPlan vide »), aucun `PageSourceToken::Block` en
//   entrée. Suivant le précédent déjà établi en 5.2 (`collect_blocks`,
//   `BlockEnd` sans `BlockOpen` correspondant : panique documentée plutôt que
//   `todo!` silencieux ou comportement inventé), le cas `Block` panique ici
//   avec un message explicite renvoyant à la Phase 5.9 : ni `todo!` ni
//   `unimplemented!` littéral, mais une invariante non couverte nommée sans
//   ambiguïté, plutôt qu'un branchement de substitution deviné par avance.
//
//   `PageSourceToken::Unsupported` ne peut pas non plus atteindre cette
//   fonction — précondition déjà actée par Document 2 §5 : ce cas est rejeté
//   en amont par `collect_blocks` (Phase 5.4, clos). Une occurrence ici
//   serait un bug de la phase amont, pas un cas à absorber dans le Lowering
//   (citation directe du contrat : « le Lowering suppose une entrée déjà
//   validée »). Panique documentée, même style que ci-dessus.
//
// ─── Projection `Static` → `StaticInclude` (provisoire) ───────────────────
//
//   `len = 0` et `rel_from_manifest = original_path` : exactement le même
//   couple de valeurs provisoires que le pattern `include` du Mode Fragment
//   (gelé, `parse_block`, ligne ~1033) — `len` sera résolu par le Resolver
//   (Document 2 §5, symétrie explicitement actée par le contrat), et
//   `rel_from_manifest` par l'orchestrateur (Document 3, hors périmètre).
//   Aucune divergence de convention entre les deux modes sur ce point.
//
// ─── Mémoire : capacité exacte pour ce sous-ensemble ───────────────────────
//
//   `Vec::with_capacity(parent_tokens.len())` est une borne exacte, pas une
//   estimation, tant qu'aucun `Block` n'est présent : chaque `Runtime` et
//   chaque `Static` produit exactement un `FlatPageToken` en sortie, la
//   correspondance est 1:1. Cette égalité cesse d'être vraie dès que la
//   Phase 5.9 introduira la splice de plages (les délimiteurs `BlockOpen`/
//   `BlockEnd` disparaissent, le contenu spliced peut différer en longueur
//   du contenu parent d'origine) — capacité à réévaluer à ce moment, pas
//   anticipée ici.
// =============================================================================
// PHASE 5.9 — `lower` : substitution effective (Document 2 §5)
// =============================================================================
// Extension de 5.8 (roadmap §5.9) : le corps de `lower` gagne le traitement
// de `PageSourceToken::Block` — aucune modification de signature (confirmée
// dès 5.8), aucune modification des branches `Runtime`/`Static` déjà closes.
// Invariant introduit (clôture du domaine composition, Document 2 §1) : le
// contenu émis pour un bloc dépend *exclusivement* de `LinkPlan` — jamais
// implicitement du contenu situé entre `BlockOpen`/`BlockEnd` dans
// `parent_tokens`. Concrètement : la plage `[start, end)` lue est toujours
// `arena.get(substitution.source.template).tokens[start..end]`, jamais une
// sous-tranche de `parent_tokens` elle-même — même quand `substitution.source
// .template` se trouve être le parent (cas « non redéfini », Document 2 §4 :
// « comportement par défaut »). Un consommateur ne peut pas, en lisant ce
// code, confondre « ce qui est physiquement entre les délimiteurs du parent »
// et « ce qui est effectivement émis » : ce sont deux sources distinctes, et
// seule la seconde compte.
//
// ─── Mécanisme de correspondance et de saut ────────────────────────────────
//
//   Boucle à index explicite (pas de `for`/`enumerate` : l'avancée n'est pas
//   uniforme — un `Block(BlockOpen)` consomme plusieurs positions de
//   `parent_tokens` d'un coup, jusqu'au `BlockEnd` apparié). Sur
//   `Block(BlockOpen { name })` : recherche dans `plan.substitutions` de
//   l'entrée de même `name` (linéaire — `substitutions` est court, borné par
//   le nombre de blocs d'un fichier, jamais un ensemble justifiant un index
//   de recherche). La plage retenue (`substitution.source`) est lue depuis
//   `arena`, jamais depuis `parent_tokens` (voir ci-dessus), puis chaque
//   token de cette plage est projeté par la même règle `Runtime`/`Static`
//   que le niveau supérieur (factorisée dans `lower_leaf_token`, privée à ce
//   module — aucune duplication de la logique de projection entre le niveau
//   racine et le contenu splicé). L'index `i` saute ensuite directement
//   après le `BlockEnd` apparié dans `parent_tokens` (recherche du premier
//   `BlockEnd` suivant `i` — sûre par précondition : les blocs ne sont pas
//   imbriqués, invariant déjà garanti en amont par `collect_blocks`,
//   `NestedBlock` rejeté avant que `lower` ne soit jamais appelé).
//
// ─── Absence de correspondance ou de fermeture : précondition violée ───────
//
//   Deux cas restent des paniques documentées, jamais des `Result` : (1) un
//   `name` de `BlockOpen` absent de `plan.substitutions` — ne peut se
//   produire si `plan` provient de `link` appelé avec les blocs du *même*
//   parent que `parent_tokens` représente (précondition d'appel, comme pour
//   `TemplateId` au Document 2 §2 : détectable par assertion, jamais un
//   contenu halluciné) ; (2) un `BlockOpen` sans `BlockEnd` apparié dans
//   `parent_tokens` — rejeté en amont par `collect_blocks`
//   (`Vec<PageValidationError>`, Phase 5.2-5.4), ne peut structurellement
//   pas atteindre `lower` sur une entrée déjà validée. Un `Block(BlockEnd)`
//   rencontré par la boucle principale sans avoir été consommé comme
//   fermeture d'un `BlockOpen` est le même bug de précondition, symétrique.
//
// ─── Contenu splicé : pas de `Block` imbriqué, panique symétrique ──────────
//
//   `lower_leaf_token` (utilisée à la fois au niveau racine et pour projeter
//   le contenu d'une plage substituée) panique sur `Block(_)` : l'imbrication
//   de blocs est rejetée en amont (`NestedBlock`, Phase 5.3), donc un
//   `Block` ne peut structurellement pas apparaître à l'intérieur d'une
//   plage `NamedBlockRange` déjà validée — même raisonnement que pour
//   `Unsupported` (Document 2 §5 : « bug de la phase amont, pas un cas à
//   gérer ici »), étendu ici au niveau splicé plutôt qu'au seul niveau
//   racine.
//
// ─── Domaine composition clos (Document 2 §1) ──────────────────────────────
//
//   À partir d'ici, `FlatPageToken<'src>` — sans variante `Block`,
//   `Extends`, ou `TemplateId` — est la seule sortie possible de ce pipeline :
//   aucun type intermédiaire d'héritage ne peut franchir cette fonction par
//   construction du système de types (Document 2 §1, postcondition finale).
//   `validate_ast`, `resolve_and_measure`, `generate_aot_snippet` (Mode
//   Fragment, gelés) s'appliquent sans modification ni branchement de mode —
//   vérifié par `cargo check` : aucun de leurs `match` exhaustifs sur
//   `FlatPageToken` n'a exigé de nouveau bras pour cette phase (jalon de
//   compilation, pas seulement d'exécution, roadmap §5.9).
pub fn lower<'src>(
    parent_tokens: &[PageSourceToken<'src>],
    plan: &LinkPlan<'src>,
    arena: &PageArena<'src>,
) -> Vec<FlatPageToken<'src>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < parent_tokens.len() {
        match &parent_tokens[i] {
            PageSourceToken::Block(PageBlockToken::BlockOpen { name }) => {
                let substitution = plan
                    .substitutions
                    .iter()
                    .find(|substitution| substitution.name == *name)
                    .unwrap_or_else(|| {
                        panic!(
                            "lower (Phase 5.9) : aucune substitution pour le bloc \
                             {name:?} dans LinkPlan — précondition violée : `plan` \
                             doit provenir de `link` appelé avec les blocs du même \
                             parent que `parent_tokens` représente."
                        )
                    });
                let source = substitution.source;
                let source_tokens = &arena.get(source.template).tokens[source.start..source.end];
                out.extend(source_tokens.iter().map(lower_leaf_token));

                i = find_matching_block_end(parent_tokens, i) + 1;
            }
            PageSourceToken::Block(PageBlockToken::BlockEnd) => unreachable!(
                "lower (Phase 5.9) : PageSourceToken::Block(BlockEnd) rencontré \
                 par la boucle principale sans BlockOpen apparié — précondition \
                 violée (entrée déjà validée par collect_blocks, Phase 5.2-5.4) : \
                 toute fermeture doit avoir été consommée par le traitement de \
                 son ouverture."
            ),
            other => {
                out.push(lower_leaf_token(other));
                i += 1;
            }
        }
    }
    out
}

/// Projette un token n'introduisant pas de composition (`Runtime`, `Static`)
/// vers `FlatPageToken` — factorisation partagée entre le niveau racine de
/// `lower` et le contenu d'une plage substituée (Phase 5.9, voir doc de
/// tête : « aucune duplication de la logique de projection »).
///
/// Panique sur `Block(_)` et `Unsupported { .. }` : aucun des deux ne peut
/// structurellement atteindre ce point sur une entrée déjà validée — voir
/// doc de tête, Phase 5.9, pour la justification de chaque cas.
fn lower_leaf_token<'src>(token: &PageSourceToken<'src>) -> FlatPageToken<'src> {
    match token {
        PageSourceToken::Runtime(flat) => *flat,
        PageSourceToken::Static(StaticPartialRef { original_path }) => {
            FlatPageToken::StaticInclude {
                original_path,
                rel_from_manifest: original_path,
                len: 0,
            }
        }
        PageSourceToken::Block(_) => unreachable!(
            "lower_leaf_token (Phase 5.9) : PageSourceToken::Block rencontré à \
             l'intérieur d'une plage projetée — précondition violée : \
             l'imbrication de blocs est rejetée en amont par collect_blocks \
             (NestedBlock, Phase 5.3), une plage NamedBlockRange déjà validée \
             ne peut structurellement pas en contenir."
        ),
        PageSourceToken::Unsupported { .. } => unreachable!(
            "lower_leaf_token (Phase 5.9) : PageSourceToken::Unsupported \
             rencontré — précondition violée (Document 2 §5) : ce cas doit \
             être rejeté en amont par collect_blocks (Phase 5.4), jamais \
             atteindre le Lowering. Bug de la phase amont, pas un cas géré ici."
        ),
    }
}

/// Cherche, à partir de `open_index + 1`, l'index du premier
/// `PageSourceToken::Block(PageBlockToken::BlockEnd)` de `tokens` — la
/// fermeture appariée au `BlockOpen` situé à `open_index`. Sûr sans pile
/// d'appariement : les blocs ne sont pas imbriqués (précondition, `lower`
/// n'est jamais appelé sur une entrée où `NestedBlock` aurait dû être
/// détecté par `collect_blocks`), donc le premier `BlockEnd` rencontré est
/// nécessairement celui qui ferme ce `BlockOpen`, jamais un autre.
///
/// Panique si aucun `BlockEnd` n'est trouvé : précondition violée, même
/// famille que l'assertion `open_stack.is_empty()` de `collect_blocks`
/// (Phase 5.2) — un `BlockOpen` sans fermeture est rejeté en amont, jamais
/// une entrée que `lower` doit absorber.
fn find_matching_block_end(tokens: &[PageSourceToken<'_>], open_index: usize) -> usize {
    tokens[open_index + 1..]
        .iter()
        .position(|token| matches!(token, PageSourceToken::Block(PageBlockToken::BlockEnd)))
        .map(|relative| open_index + 1 + relative)
        .unwrap_or_else(|| {
            panic!(
                "find_matching_block_end (Phase 5.9) : BlockOpen à l'index \
                 {open_index} sans BlockEnd apparié — précondition violée : \
                 rejeté en amont par collect_blocks (Phase 5.2-5.4), ne peut \
                 structurellement pas atteindre lower."
            )
        })
}

// =============================================================================
// Tests — Phase 5.8
// =============================================================================

#[cfg(test)]
mod tests_phase_5_8_lower_no_substitution {
    use super::{FlatPageToken, LinkPlan, PageArena, PageSourceToken, StaticPartialRef, lower};

    /// Jalon Vert (roadmap §5.8) — template sans blocs (`LinkPlan` vide,
    /// aucun `PageSourceToken::Block` en entrée) : le `Static` unique produit
    /// exactement un `FlatPageToken::StaticInclude { len: 0, .. }`, et
    /// chaque `Runtime` traverse inchangé (égalité valeur à valeur, testée
    /// sur plusieurs variantes de `FlatPageToken` pour couvrir la
    /// projection identité au-delà du seul cas `Static`).
    #[test]
    fn runtime_tokens_pass_through_and_static_becomes_static_include_with_len_zero() {
        let plan = LinkPlan {
            substitutions: Vec::new(),
        };
        let arena = PageArena::default();

        let tokens = vec![
            PageSourceToken::Runtime(FlatPageToken::Static("before")),
            PageSourceToken::Runtime(FlatPageToken::Field {
                entity: "user",
                field: "name",
            }),
            PageSourceToken::Static(StaticPartialRef {
                original_path: "nav.html",
            }),
            PageSourceToken::Runtime(FlatPageToken::IfBool {
                entity: "user",
                field: "active",
            }),
            PageSourceToken::Runtime(FlatPageToken::EndIf),
        ];

        let result = lower(&tokens, &plan, &arena);

        assert_eq!(
            result,
            vec![
                FlatPageToken::Static("before"),
                FlatPageToken::Field {
                    entity: "user",
                    field: "name",
                },
                FlatPageToken::StaticInclude {
                    original_path: "nav.html",
                    rel_from_manifest: "nav.html",
                    len: 0,
                },
                FlatPageToken::IfBool {
                    entity: "user",
                    field: "active",
                },
                FlatPageToken::EndIf,
            ]
        );
    }
}

// =============================================================================
// Tests — Phase 5.9
// =============================================================================

#[cfg(test)]
mod tests_phase_5_9_lower_substitution {
    use super::{
        FlatPageToken, PageArena, PageBlockToken, PageSourceToken, ParsedPageTemplate,
        collect_blocks, link, lower,
    };

    /// Jalon Vert (roadmap §5.9) — bout en bout en mémoire : un parent à
    /// deux blocs (`title`, `footer`), un enfant qui redéfinit `title` mais
    /// pas `footer`. Vérifie dans le même test le bloc redéfini (contenu
    /// enfant émis, contenu parent d'origine absent) et le bloc non
    /// redéfini (contenu parent conservé) — séquence `FlatPageToken` exacte,
    /// élément par élément, `Vec` entier comparé par égalité de valeur.
    ///
    /// Assertion de type, pas seulement de valeur : le type de retour de
    /// `lower` est `Vec<FlatPageToken<'src>>` — sans variante `Block`,
    /// `Extends`, ni `TemplateId` possible en sortie, garanti par le système
    /// de types (Document 2 §1), pas reconfirmé ici par une inspection de
    /// valeur supplémentaire.
    #[test]
    fn overridden_block_uses_child_content_untouched_block_keeps_parent_content() {
        let parent_tokens = vec![
            PageSourceToken::Runtime(FlatPageToken::Static("<html>")),
            PageSourceToken::Block(PageBlockToken::BlockOpen { name: "title" }),
            PageSourceToken::Runtime(FlatPageToken::Static("Default Title")),
            PageSourceToken::Block(PageBlockToken::BlockEnd),
            PageSourceToken::Runtime(FlatPageToken::Static("<body>")),
            PageSourceToken::Block(PageBlockToken::BlockOpen { name: "footer" }),
            PageSourceToken::Runtime(FlatPageToken::Static("Default Footer")),
            PageSourceToken::Block(PageBlockToken::BlockEnd),
            PageSourceToken::Runtime(FlatPageToken::Static("</body></html>")),
        ];

        let child_tokens = vec![
            PageSourceToken::Block(PageBlockToken::BlockOpen { name: "title" }),
            PageSourceToken::Runtime(FlatPageToken::Static("Child Title")),
            PageSourceToken::Block(PageBlockToken::BlockEnd),
        ];

        let mut arena = PageArena::default();
        let parent_id = arena.admit(ParsedPageTemplate {
            extends: None,
            tokens: parent_tokens.clone(),
        });
        let child_id = arena.admit(ParsedPageTemplate {
            extends: Some("parent.marius"),
            tokens: child_tokens.clone(),
        });

        let parent_blocks =
            collect_blocks(parent_id, &parent_tokens).expect("parent blocks bien formés");
        let child_blocks =
            collect_blocks(child_id, &child_tokens).expect("child blocks bien formés");

        let plan = link(&parent_blocks, &child_blocks, &[], |_| true)
            .expect("linking réussit : aucun bloc orphelin, aucune référence static");

        let result = lower(&parent_tokens, &plan, &arena);

        assert_eq!(
            result,
            vec![
                FlatPageToken::Static("<html>"),
                FlatPageToken::Static("Child Title"),
                FlatPageToken::Static("<body>"),
                FlatPageToken::Static("Default Footer"),
                FlatPageToken::Static("</body></html>"),
            ]
        );
    }
}
