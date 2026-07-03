// =============================================================================
// marius-fragment-forge — forge/fragment-forge/src/lib.rs
// Projet Marius · ADR-002 / no_std-attitude-within-marius.md
//
// Génère le corps de render() pour chaque table surveillée.
// Appelé depuis crates/core/schema/build.rs au moment de la compilation.
//
// Produit pour chaque table surveillée :
//   1. const {NAME}_STATIC_CAP  : somme exacte des octets HTML statiques
//   2. const {NAME}_DYNAMIC_CAP : somme des largeurs maximales des champs (pire cas)
//   3. fn render(record: &{Name}StorageRow, payload: &{Name}RenderPayload, buf: &mut String)
//      → séquence de push_str (statique) + write_fmt (dynamique) + marius_html_escape (varlena)
//      → zéro allocation intermédiaire (pas de String temporaire, pas de format!() alloué)
//
// ─── Taxonomie des structs générées ──────────────────────────────────────────
//
//   {Name}Row           : struct sqlx::FromRow, non-repr(C).
//                         Types natifs + Option<T> pour nullable.
//                         Varlena portées sous forme de Option<String> (allocation sqlx).
//                         Rôle : transport depuis la base jusqu'au site de projection.
//                         Durée de vie : éphémère (détruite après render()).
//
//   {Name}StorageRow    : struct #[repr(C)], layout bit-à-bit aligné sur le DDL.
//                         Types fixed-length uniquement. Nullable → sentinel.
//                         Varlena exclues (incompatibles avec repr(C) : fat pointer 16B).
//                         Rôle : stockage en mémoire contiguë, cache CPU-friendly.
//                         Durée de vie : persistante (vit dans les artéfacts en mémoire).
//
//   {Name}RenderPayload : struct éphémère de rendu, non-repr(C).
//                         Champs varlena portés sous forme de &'a str (emprunt sans copie).
//                         Rôle : assemblage des références varlena depuis la Row
//                         pour les transmettre à render() sans réallocation.
//                         Durée de vie : limitée à la durée de render() ('a).
//
// ─── Chemin critique (hot path) ──────────────────────────────────────────────
//
//   StorageRow (repr(C)) + RenderPayload (&'a str) → render() → buf: &mut String
//
//   Invariant no-realloc : buf.capacity() NE DOIT PAS changer pendant render().
//   Vérification : tests unitaires test_{name}_no_realloc() alimentent
//   les pires cas (valeurs maximales de chaque type) et assertent
//   buf.capacity() == {NAME}_TOTAL_CAP après le render.
//
// ─── Politique d'escape HTML ─────────────────────────────────────────────────
//
//   Champ normal    : facteur × 5  (pire cas : '&' → '&amp;', 5 chars)
//   Champ pre_escaped : facteur × 1  (tag COMMENT ON COLUMN 'marius:pre_escaped')
//
//   La détection du tag est effectuée dans build.rs à l'introspection.
//   Fragment-Forge reçoit l'information via VarlenField::pre_escaped.
//
// ─── Contraintes (directives Session 5 / no_std-attitude) ───────────────────
//
//   - Zéro logique dans le template : aucun if/match dans le corps généré.
//   - Zéro runtime de templating : le code généré est des appels directs
//     push_str / write_fmt / marius_html_escape.
//   - Capacité calculée statiquement : STATIC_CAP + DYNAMIC_CAP = borne supérieure exacte.
//   - Les fonctions de ce module sont pure Rust (pas d'I/O, pas d'alloc dynamique propre).
//
// =============================================================================

// pub mod orchestrator;
// pub mod prologue;
// pub mod body;
// pub mod generator;

// =============================================================================
// I. Types internes
// =============================================================================

/// Catégorie d'un champ fixed-length pour le calcul de capacité dynamique.
///
/// Chaque variante encode le pire cas d'affichage textuel du type correspondant.
/// Ces bornes sont utilisées pour calculer DYNAMIC_CAP à la compilation (build.rs),
/// garantissant l'absence de réallocation pendant render().
///
/// ─── Correspondances DDL → FieldKind ─────────────────────────────────────────
///
///   TIMESTAMPTZ, TIMESTAMP, BIGINT → I64
///   INTEGER, INT, SERIAL, DATE     → I32
///   SMALLINT                       → I16
///   BOOLEAN                        → Bool
///   REAL                           → F32
///   DOUBLE PRECISION               → F64
///
/// Tous les types varlena (TEXT, VARCHAR, BYTEA, JSONB…) sont exclus :
/// leur capacité est portée par VarlenField, pas par FieldKind.
#[derive(Debug, Clone, Copy)]
pub enum FieldKind {
    /// INT8 / BIGINT / TIMESTAMPTZ / TIMESTAMP
    /// i64::MIN = -9223372036854775808 = 20 caractères
    I64,
    /// INT4 / INTEGER / SERIAL / DATE (days_from_CE)
    /// i32::MIN = -2147483648 = 11 caractères
    I32,
    /// INT2 / SMALLINT
    /// i16::MIN = -32768 = 6 caractères
    I16,
    /// BOOLEAN
    /// "false" = 5 caractères (> "true" = 4)
    Bool,
    /// REAL / FLOAT4
    /// représentation flottante pire cas : "-3.40282347e38" ≈ 14 caractères
    F32,
    /// DOUBLE PRECISION / FLOAT8
    /// représentation flottante pire cas ≈ 24 caractères
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
            Self::I64  => 20,  // "-9223372036854775808"
            Self::I32  => 11,  // "-2147483648"
            Self::I16  => 6,   // "-32768"
            Self::Bool => 5,   // "false"
            Self::F32  => 14,  // "-3.40282347e38" (approximation conservative)
            Self::F64  => 24,  // représentation longue
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
        let t = sql_type.split('(').next().unwrap_or(sql_type).trim().to_lowercase();
        match t.as_str() {
            "int8" | "bigint"
            | "timestamptz" | "timestamp with time zone"
            | "timestamp"   | "timestamp without time zone" => Some(Self::I64),
            "int4" | "integer" | "int" | "serial" | "date" => Some(Self::I32),
            "int2" | "smallint"                            => Some(Self::I16),
            "bool" | "boolean"                             => Some(Self::Bool),
            "float4" | "real"                              => Some(Self::F32),
            "float8" | "double precision"                  => Some(Self::F64),
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
    pub name:   String,
    /// Catégorie de type (détermine max_display_width).
    pub kind:   FieldKind,
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
///   TEXT sans CHECK ni VARCHAR → panic! si référencé dans le listing dense ;
///                             fallback à 10 000 pour le rendu page complète,
///                             accompagné d'un cargo:warning.
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
///     build.rs lit pg_description pour détecter ce tag.
///     Un facteur de 1 évite la sur-estimation de DYNAMIC_CAP pour ces champs.
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
    pub name:                    String,
    /// Borne supérieure en octets, si elle existe dans le schéma PostgreSQL
    /// (VARCHAR(N) via atttypmod, ou TEXT avec CHECK(length(col) <= N) parsable).
    ///
    /// `None` (ADR-007) : la colonne est un TEXT sans contrainte exploitable —
    /// ni VARCHAR(N), ni CHECK reconnu. Ce n'est PAS une erreur en soi : la
    /// classification Hot/Cold/Erreur est tranchée par resolve_and_measure
    /// selon que le champ est référencé ou non par le template résolu.
    /// Aucun fallback numérique n'est jamais substitué à None — une absence
    /// de borne reste une absence de borne jusqu'à la frontière de résolution.
    pub max_len:                  Option<usize>,
    /// true si le contenu est certifié pré-échappé (tag 'marius:pre_escaped').
    /// Facteur de capacité = 1 au lieu de HTML_ESCAPE_FACTOR.
    pub pre_escaped:               bool,
    /// true si la colonne DDL est nullable (Option<String> dans VarlenOwned).
    /// En v1, toujours true (LEFT JOIN produit systématiquement Option).
    /// Réservé v2 : champ NOT NULL → String directe, court-circuite l'Option.
    pub nullable:                  bool,
    /// Surcharge manuelle de max_escaped_len. None = calculé (max_len × facteur).
    /// Utile quand la borne théorique est trop conservative pour un champ donné.
    /// Sans effet si `max_len` est également `None` — il n'y a alors rien à
    /// surcharger, `max_escaped_len()` retourne None indépendamment de ce champ.
    pub max_escaped_len_override:  Option<usize>,
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
    /// Priorité : max_escaped_len_override > pre_escaped > facteur HTML.
    ///
    /// Retourne `None` si `max_len` est `None` (ADR-007) : il n'existe pas de
    /// borne à propager, quelle que soit la valeur de `max_escaped_len_override`
    /// ou `pre_escaped`. L'appelant (resolve_and_measure) est responsable de
    /// traiter ce `None` selon la table de vérité Hot/Cold/Erreur — cette
    /// méthode ne décide jamais d'une valeur de repli.
    pub fn max_escaped_len(&self) -> Option<usize> {
        if let Some(override_len) = self.max_escaped_len_override {
            return Some(override_len);
        }
        let n = self.max_len?;
        Some(if self.pre_escaped {
            n
        } else {
            n * Self::HTML_ESCAPE_FACTOR
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
    pub fixed:   &'a [FieldSpec],
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
pub fn relative_path_for_include_str(
    manifest_dir:      &str,
    rel_from_manifest: &str,
) -> String {
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
    schema:  &str,
    table:   &str,
    fields:  &[FieldSpec],
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
    Field {
        entity: &'src str,
        field:  &'src str,
    },

    /// Bloc conditionnel booléen : `{% if entity.field %}`.
    IfBool {
        entity: &'src str,
        field:  &'src str,
    },

    /// Fermeture de bloc : `{% endif %}`.
    EndIf,

    /// Inclusion statique résolue au build-time : `{% include path %}`.
    ///
    /// `len` : longueur en octets du fichier inclus, connue à la compilation
    /// (via `std::fs::metadata`). Composante directe de `PAGE_STATIC_CAP`.
    StaticInclude {
        original_path:     &'src str,
        rel_from_manifest: &'src str,
        len:               usize,
    },
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
        let tokens: [FlatPageToken<'_>; 5] = [
            FlatPageToken::Static("content"),
            FlatPageToken::Field   { entity: "user", field: "name" },
            FlatPageToken::IfBool  { entity: "user", field: "active" },
            FlatPageToken::EndIf,
            FlatPageToken::StaticInclude {
                original_path:     "templates/header.html",
                rel_from_manifest: "../templates/header.html",
                len:               42,
            },
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
    pub kind:  SpanKind,
}

// ─── État interne ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Literal,  // Entre les blocs : tout est HTML statique jusqu'au prochain délimiteur.
    InExpr,   // Intérieur de `{{ … }}` : Ident, Punct, ExprClose.
    InBlock,  // Intérieur de `{% … %}` : Ident (token brut), BlockClose.
}

struct Scanner<'src> {
    src:  &'src str,
    pos:  usize,   // Offset byte courant — toujours sur une frontière char valide.
    mode: Mode,
}

impl<'src> Scanner<'src> {
    fn new(src: &'src str) -> Self {
        Self { src, pos: 0, mode: Mode::Literal }
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
                    (None, None)                       => None,
                };

                match rel {
                    // Pas de délimiteur : le reste est un unique Literal.
                    None => {
                        let span = RawSpan { slice: &src[self.pos..], kind: SpanKind::Literal };
                        self.pos = src.len();
                        Some(span)
                    }
                    // Délimiteur immédiat : l'émettre et basculer de mode.
                    Some(0) => {
                        let p = self.pos;
                        if src[p..].starts_with("{{") {
                            self.mode = Mode::InExpr;
                            self.pos  = p + 2;
                            Some(RawSpan { slice: &src[p..p + 2], kind: SpanKind::ExprOpen })
                        } else {
                            // starts_with("{%") — seule autre option possible
                            self.mode = Mode::InBlock;
                            self.pos  = p + 2;
                            Some(RawSpan { slice: &src[p..p + 2], kind: SpanKind::BlockOpen })
                        }
                    }
                    // Literal précède le délimiteur.
                    // On émet le Literal et on reste en mode Literal :
                    // le délimiteur sera émis au prochain appel.
                    Some(rel) => {
                        let end  = self.pos + rel;
                        let span = RawSpan { slice: &src[self.pos..end], kind: SpanKind::Literal };
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
                    self.pos  = p + 2;
                    return Some(RawSpan { slice: &src[p..p + 2], kind: SpanKind::ExprClose });
                }

                if src[p..].starts_with('.') {
                    self.pos = p + 1;
                    return Some(RawSpan { slice: &src[p..p + 1], kind: SpanKind::Punct });
                }

                // Identifiant : séquence de bytes ASCII alphanumériques ou `_`.
                // Tous single-byte → `pos` reste sur une frontière valide.
                let start = p;
                let b     = src.as_bytes();
                while self.pos < src.len()
                    && (b[self.pos].is_ascii_alphanumeric() || b[self.pos] == b'_')
                {
                    self.pos += 1;
                }

                if self.pos > start {
                    Some(RawSpan { slice: &src[start..self.pos], kind: SpanKind::Ident })
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
                    self.pos  = p + 2;
                    return Some(RawSpan { slice: &src[p..p + 2], kind: SpanKind::BlockClose });
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
                    Some(RawSpan { slice: &src[start..self.pos], kind: SpanKind::Ident })
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
    use super::{scan, RawSpan, SpanKind};

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
        let src  = "hello {{ user.name }} world";
        let got: Vec<_> = scan(src).collect();

        let expected = [
            s("hello ",  SpanKind::Literal),
            s("{{",      SpanKind::ExprOpen),
            s("user",    SpanKind::Ident),
            s(".",       SpanKind::Punct),
            s("name",    SpanKind::Ident),
            s("}}",      SpanKind::ExprClose),
            s(" world",  SpanKind::Literal),
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
            s("{%",           SpanKind::BlockOpen),
            s("if",           SpanKind::Ident),
            s("user.active",  SpanKind::Ident),
            s("%}",           SpanKind::BlockClose),
            s("oui",          SpanKind::Literal),
            s("{%",           SpanKind::BlockOpen),
            s("endif",        SpanKind::Ident),
            s("%}",           SpanKind::BlockClose),
        ];

        assert_eq!(got.len(), expected.len(), "nombre de spans incorrect");
        for (i, (g, e)) in got.iter().zip(expected.iter()).enumerate() {
            assert_eq!(g, e, "span[{i}]");
        }
    }

    /// Cas limites : source vide et source sans délimiteur.
    #[test]
    fn scan_empty_and_literal_only() {
        assert!(scan("").next().is_none(), "source vide doit être épuisée immédiatement");

        let got: Vec<_> = scan("<p>texte statique</p>").collect();
        assert_eq!(got, [s("<p>texte statique</p>", SpanKind::Literal)]);
    }

    /// Vérifie qu'un délimiteur en tête de source produit ExprOpen sans Literal vide.
    #[test]
    fn scan_delimiter_at_start() {
        let got: Vec<_> = scan("{{ x }}").collect();
        assert_eq!(got[0].kind, SpanKind::ExprOpen,
            "le premier span doit être ExprOpen, pas un Literal vide");
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
    UnexpectedToken { expected: &'static str, got: SpanKind },
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
    let mut ast  = Vec::new();

    while let Some(span) = iter.next() {
        let token = match span.kind {
            // Texte HTML verbatim → Static directement.
            SpanKind::Literal  => FlatPageToken::Static(span.slice),

            // `{{ entity.field }}` → Field.
            SpanKind::ExprOpen  => parse_expr(&mut iter)?,

            // `{% keyword … %}` → IfBool | EndIf | StaticInclude.
            SpanKind::BlockOpen => parse_block(&mut iter)?,

            // Tout autre span en position initiale est une erreur structurelle.
            // ExprClose, BlockClose, Ident, Punct ne peuvent pas ouvrir un token.
            got => return Err(PageParseError::UnexpectedToken {
                expected: "Literal | ExprOpen | BlockOpen",
                got,
            }),
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
    expect_kind(iter, SpanKind::Punct,     "Punct('.')")?;
    let field  = expect_ident(iter, "Ident(field)")?;
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
    let keyword = expect_ident(iter, "keyword (if | endif | include)")?;

    match keyword {
        "if" => {
            let raw            = expect_ident(iter, "Ident(entity.field)")?;
            let (entity, field) = split_dotted(raw)?;
            expect_kind(iter, SpanKind::BlockClose, "BlockClose('%}')")?;
            Ok(FlatPageToken::IfBool { entity, field })
        }
        "endif" => {
            expect_kind(iter, SpanKind::BlockClose, "BlockClose('%}')")?;
            Ok(FlatPageToken::EndIf)
        }
        "include" => {
            let path = expect_ident(iter, "Ident(path)")?;
            expect_kind(iter, SpanKind::BlockClose, "BlockClose('%}')")?;
            Ok(FlatPageToken::StaticInclude {
                original_path:     path,
                rel_from_manifest: path, // provisoire : sera résolu par l'orchestrateur
                len:               0,    // provisoire : idem
            })
        }
        _ => Err(PageParseError::InvalidBlockSequence),
    }
}

// ─── Primitives de consommation ───────────────────────────────────────────────

/// Consomme le span suivant et retourne sa slice si c'est un `Ident`.
/// Retourne une erreur décrivant ce qui était attendu sinon.
#[inline]
fn expect_ident<'src, I>(
    iter:     &mut I,
    expected: &'static str,
) -> Result<&'src str, PageParseError>
where
    I: Iterator<Item = RawSpan<'src>>,
{
    match iter.next() {
        Some(span) if span.kind == SpanKind::Ident => Ok(span.slice),
        Some(span) => Err(PageParseError::UnexpectedToken { expected, got: span.kind }),
        None       => Err(PageParseError::UnexpectedEof),
    }
}

/// Consomme le span suivant et vérifie qu'il a le `kind` attendu.
/// La slice n'est pas retournée (les délimiteurs ne portent pas de sémantique).
#[inline]
fn expect_kind<'src, I>(
    iter:     &mut I,
    kind:     SpanKind,
    expected: &'static str,
) -> Result<(), PageParseError>
where
    I: Iterator<Item = RawSpan<'src>>,
{
    match iter.next() {
        Some(span) if span.kind == kind => Ok(()),
        Some(span) => Err(PageParseError::UnexpectedToken { expected, got: span.kind }),
        None       => Err(PageParseError::UnexpectedEof),
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
    use super::{
        parse_tokens, PageParseError,
        scan, SpanKind,
        FlatPageToken,
    };

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
        let src = "hello {{ user.name }} {% if user.active %} {% include fragment.html %} {% endif %}";
        let got = parse_tokens(scan(src)).expect("parsing doit réussir sur un template valide");

        // Note : FlatPageToken doit dériver PartialEq, Eq (ajout non-cassant sur Phase 1.1).
        let expected: &[FlatPageToken<'_>] = &[
            FlatPageToken::Static("hello "),
            FlatPageToken::Field { entity: "user", field: "name" },
            FlatPageToken::Static(" "),
            FlatPageToken::IfBool { entity: "user", field: "active" },
            FlatPageToken::Static(" "),
            FlatPageToken::StaticInclude {
                original_path:     "fragment.html",
                rel_from_manifest: "fragment.html",
                len:               0,
            },
            FlatPageToken::Static(" "),
            FlatPageToken::EndIf,
        ];

        assert_eq!(got.len(), expected.len(),
            "nombre de tokens incorrect : got {}, expected {}", got.len(), expected.len());

        for (i, (g, e)) in got.iter().zip(expected.iter()).enumerate() {
            assert_eq!(g, e, "token[{i}] incorrect");
        }
    }

    /// Erreur sur token inattendu en position initiale (ExprClose seul, sans ExprOpen).
    #[test]
    fn error_on_unexpected_top_level_span() {
        // Scanner ne peut pas produire un ExprClose seul en position initiale,
        // mais ce test vérifie le chemin d'erreur de parse_tokens directement.
        use super::{RawSpan};
        let orphan = [RawSpan { slice: "}}", kind: SpanKind::ExprClose }];
        let err = parse_tokens(orphan.into_iter()).unwrap_err();
        assert_eq!(
            err,
            PageParseError::UnexpectedToken {
                expected: "Literal | ExprOpen | BlockOpen",
                got:      SpanKind::ExprClose,
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
                expected: "keyword (if | endif | include)",
                got:      SpanKind::BlockClose,
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
    NestedIfNotSupported { nested_entity: &'src str, nested_field: &'src str },
    /// Fin de l'AST atteinte alors qu'un bloc `if` était encore ouvert.
    UnclosedIf { entity: &'src str, field: &'src str },
}

/// Parcourt l'AST et valide la machine à états des blocs conditionnels.
///
/// # FSM
/// ```text
/// État : None | Some((entity, field))
///
/// None  + IfBool      → Some(entity, field)         [transition normale]
/// None  + EndIf       → None  + push UnexpectedEndIf [erreur, état inchangé]
/// Some  + IfBool      → Some  + push Nested          [erreur, état inchangé]
/// Some  + EndIf       → None                         [fermeture normale]
/// *     + Static/Field/Include → état inchangé       [neutre]
/// EOF   + Some(e, f)  → push UnclosedIf(e, f)        [erreur de parité]
/// ```
///
/// # Garantie de terminaison
/// Parcours linéaire de longueur `tokens.len()` : O(n), pas de récursion.
///
/// # Allocation
/// `Vec::new()` n'alloue pas avant le premier `push` :
/// un AST valide produit `Ok(())` sans allocation heap.
pub fn validate_ast<'src>(
    tokens: &[FlatPageToken<'src>],
) -> Result<(), Vec<SemanticError<'src>>> {
    let mut errors: Vec<SemanticError<'src>> = Vec::new();

    // `None`              : pas de bloc conditionnel ouvert.
    // `Some((e, f))`      : dans un bloc `{% if e.f %}`, en attente de `{% endif %}`.
    let mut current_open_if: Option<(&'src str, &'src str)> = None;

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
                        nested_field:  field,
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

            // Static, Field, StaticInclude : aucun effet sur la FSM.
            FlatPageToken::Static(_)
            | FlatPageToken::Field { .. }
            | FlatPageToken::StaticInclude { .. } => {}
        }
    }

    // Contrôle de parité final.
    // Si un bloc est resté ouvert, l'erreur est enregistrée après le parcours,
    // ce qui garantit que toutes les erreurs intra-parcours sont déjà dans `errors`.
    if let Some((entity, field)) = current_open_if {
        errors.push(SemanticError::UnclosedIf { entity, field });
    }

    if errors.is_empty() { Ok(()) } else { Err(errors) }
}

// =============================================================================
// Tests — Phase 1.4
// =============================================================================

#[cfg(test)]
mod tests_phase_1_4 {
    use super::{validate_ast, SemanticError, FlatPageToken};

    /// Jalon Vert — séquence valide : deux blocs if séquentiels non imbriqués.
    ///
    /// Vérifie que la FSM revient bien à l'état None après chaque EndIf,
    /// et que le second IfBool ne déclenche pas de NestedIfNotSupported.
    #[test]
    fn test_semantic_valid() {
        let tokens: &[FlatPageToken<'_>] = &[
            FlatPageToken::Static("avant"),
            FlatPageToken::IfBool  { entity: "user", field: "active" },
            FlatPageToken::Field   { entity: "user", field: "name"   },
            FlatPageToken::EndIf,
            FlatPageToken::Static("entre"),
            FlatPageToken::IfBool  { entity: "user", field: "admin"  },
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
            FlatPageToken::IfBool { entity: "user", field: "active"  },
            // [2] Imbrication interdite — l'externe reste actif
            FlatPageToken::IfBool { entity: "user", field: "admin"   },
            // Ferme le bloc externe (l'imbriqué a été ignoré)
            FlatPageToken::EndIf,
            // [3] Bloc non fermé à l'EOF
            FlatPageToken::IfBool { entity: "user", field: "premium" },
        ];

        let expected = vec![
            SemanticError::UnexpectedEndIf,
            SemanticError::NestedIfNotSupported {
                nested_entity: "user",
                nested_field:  "admin",
            },
            SemanticError::UnclosedIf {
                entity: "user",
                field:  "premium",
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
    pub total_static_bytes:  usize,
    /// Somme des pires cas d'affichage de tous les champs Field du template.
    /// Fixed-length : FieldKind::max_display_width().
    /// Varlena : VarlenField::max_escaped_len() (facteur escape × max_len).
    pub total_dynamic_bytes: usize,
    /// Nombre de fichiers externes inclus résolus avec succès.
    pub include_count: usize,
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
pub fn resolve_and_measure<'src>(
    tokens:        &mut [FlatPageToken<'src>],
    schema:        &SchemaIndex<'_>,
    get_file_size: impl Fn(&str) -> Result<usize, String>,
) -> Result<TemplateMetrics, Vec<ResolverError<'src>>> {
    let mut metrics = TemplateMetrics {
        total_static_bytes:  0,
        total_dynamic_bytes: 0,
        include_count:       0,
    };
    let mut errors:  Vec<ResolverError<'src>> = Vec::new();

    for token in tokens.iter_mut() {
        match token {

            // Octets HTML connus statiquement : contribution directe.
            FlatPageToken::Static(s) => {
                metrics.total_static_bytes += s.len();
            }

            // Inclusion externe : résolution I/O et mutation en place.
            FlatPageToken::StaticInclude { rel_from_manifest, len, .. } => {
                let path = *rel_from_manifest;
                match get_file_size(path) {
                    Ok(size) => {
                        *len = size;
                        metrics.total_static_bytes += size;
                        metrics.include_count      += 1;
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
                        None    => errors.push(ResolverError::UnboundedField { entity, field }),
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
        }
    }

    if errors.is_empty() { Ok(metrics) } else { Err(errors) }
}

// =============================================================================
// Tests — Phase 2.1
// =============================================================================

#[cfg(test)]
mod tests_phase_2_1 {
    use super::{resolve_and_measure, ResolverError, TemplateMetrics, FlatPageToken, SchemaIndex, FieldSpec, FieldKind, VarlenField};

    /// Construit un StaticInclude avec len = 0 (valeur provisoire Phase 1.3).
    /// Les deux paths sont identiques : l'orchestrateur n'a pas encore calculé
    /// le chemin relatif au manifest.
    fn make_include(path: &str) -> FlatPageToken<'_> {
        FlatPageToken::StaticInclude {
            original_path:     path,
            rel_from_manifest: path,
            len:               0,
        }
    }

    /// Vérifie le chemin heureux : mutation en place + métriques correctes.
    ///
    /// total_static_bytes = 6 (Static "<html>") + 10 (StaticInclude "a.html") = 16.
    #[test]
    fn test_resolve_success() {
        let mut tokens = vec![
            FlatPageToken::Static("<html>"),
            make_include("a.html"),
        ];

        let schema = SchemaIndex { fixed: &[], varlena: &[] };
        let result = resolve_and_measure(&mut tokens, &schema, |path| match path {
            "a.html" => Ok(10),
            other    => Err(format!("unknown : {other}")),
        });

        // Métriques correctes.
        assert_eq!(
            result,
            Ok(TemplateMetrics { total_static_bytes: 16, total_dynamic_bytes: 0, include_count: 1 }),
        );

        // Preuve de mutation en place : len vaut 10, pas 0.
        // Les slices &'src str sont inchangées — seul le scalaire `len` a évolué.
        match &tokens[1] {
            FlatPageToken::StaticInclude { len, original_path, rel_from_manifest } => {
                assert_eq!(*len,               10,       "len doit être muté de 0 à 10");
                assert_eq!(*original_path,     "a.html", "original_path inchangé");
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

        let schema = SchemaIndex { fixed: &[], varlena: &[] };
        let result = resolve_and_measure(&mut tokens, &schema, |path| match path {
            "a.html" => Ok(10),
            other    => Err(format!("introuvable : {other}")),
        });

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
        }

        // Invariant fail-slow : "a.html" a bien été muté malgré l'erreur suivante.
        match &tokens[1] {
            FlatPageToken::StaticInclude { len, .. } => {
                assert_eq!(*len, 10, "la mutation de a.html doit survivre à l'erreur partielle");
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
            FlatPageToken::Field { entity: "user", field: "name" },
            FlatPageToken::Static("</p>"),
        ];

        let fixed = vec![FieldSpec { name: "name".to_string(), kind: FieldKind::I32, attnum: 1 }];
        let schema = SchemaIndex { fixed: &fixed, varlena: &[] };

        assert_eq!(
            resolve_and_measure(
                &mut tokens,
                &schema,
                |_| unreachable!("get_file_size ne doit pas être appelé sans StaticInclude"),
            ),
            Ok(TemplateMetrics {
                total_static_bytes:  7,
                total_dynamic_bytes: FieldKind::I32.max_display_width(),
                include_count:       0,
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
            max_len: None,
            pre_escaped: false,
            nullable: true,
            max_escaped_len_override: None,
        }
    }

    fn bounded_field(name: &str, max_len: usize) -> VarlenField {
        VarlenField {
            name: name.to_string(),
            max_len: Some(max_len),
            pre_escaped: false,
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
        let mut tokens = vec![
            FlatPageToken::Field { entity: "record", field: "description" },
        ];
        let varlena = vec![unbounded_field("description")];
        let schema = SchemaIndex { fixed: &[], varlena: &varlena };

        let result = resolve_and_measure(
            &mut tokens, &schema,
            |_| unreachable!("aucun StaticInclude dans ce test"),
        );

        assert_eq!(
            result,
            Err(vec![ResolverError::UnboundedField { entity: "record", field: "description" }]),
        );
    }

    /// Ligne 2 de la table de vérité : champ borné, RÉFÉRENCÉ par l'AST.
    /// → Ok, contribue normalement à total_dynamic_bytes via max_escaped_len().
    /// Comportement Hot — inchangé depuis avant ADR-007, vérifié explicitement
    /// pour garantir qu'il n'a pas régressé avec le passage à Option<usize>.
    #[test]
    fn bounded_field_referenced_contributes_normally() {
        let mut tokens = vec![
            FlatPageToken::Field { entity: "record", field: "headline" },
        ];
        let varlena = vec![bounded_field("headline", 100)];
        let schema = SchemaIndex { fixed: &[], varlena: &varlena };

        let metrics = resolve_and_measure(
            &mut tokens, &schema,
            |_| unreachable!("aucun StaticInclude dans ce test"),
        ).expect("champ borné référencé : résolution attendue en succès");

        assert_eq!(metrics.total_dynamic_bytes, 100 * VarlenField::HTML_ESCAPE_FACTOR);
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
        let mut tokens = vec![
            FlatPageToken::Static("<article></article>"),
        ];
        let varlena = vec![unbounded_field("description")];
        let schema = SchemaIndex { fixed: &[], varlena: &varlena };

        let metrics = resolve_and_measure(
            &mut tokens, &schema,
            |_| unreachable!("aucun StaticInclude dans ce test"),
        ).expect("champ Cold non référencé : résolution attendue en succès");

        assert_eq!(metrics.total_dynamic_bytes, 0, "champ Cold ne doit jamais contribuer");
        assert_eq!(metrics.total_static_bytes, 19); // "<article></article>".len()
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
pub fn generate_aot_snippet<'src>(
    tokens: &[FlatPageToken<'src>],
    schema: &SchemaIndex<'_>,
) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(25 + tokens.len() * 60);

    // ── Déclarations de références varlena ────────────────────────────────────
    let mut varlena_seen: Vec<&str> = tokens.iter()
        .filter_map(|t| match t {
            FlatPageToken::Field { field, .. }
                if schema.find_varlena(field).is_some() => Some(*field),
            _ => None,
        })
        .collect();
    varlena_seen.sort_unstable();
    varlena_seen.dedup();
    for name in &varlena_seen {
        writeln!(out, "let {name}_ref: Option<&str> = varlena.{name}.as_deref();").unwrap();
    }

    let mut indent: &str = "";

    for token in tokens {
        match token {
            FlatPageToken::Static(s) => {
                writeln!(out, "{}buf.push_str({:?});", indent, s).unwrap();
            }

            FlatPageToken::Field { field, .. } => {
                if schema.find_varlena(field).is_some() {
                    writeln!(
                        out,
                        "{}if let Some(s) = {field}_ref {{ marius_html_escape(s, buf); }}",
                        indent,
                    ).unwrap();
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

            FlatPageToken::StaticInclude { rel_from_manifest, .. } => {
                writeln!(
                    out,
                    "{}buf.push_str(include_str!({:?}));",
                    indent, rel_from_manifest,
                ).unwrap();
            }
        }
    }

    out
}

// =============================================================================
// Tests — Phase 2.2
// =============================================================================

#[cfg(test)]
mod tests_phase_2_2 {
    use super::{generate_aot_snippet, SchemaIndex, FieldSpec, FieldKind, VarlenField, FlatPageToken};

    fn make_schema<'a>(fixed: &'a [FieldSpec], varlena: &'a [VarlenField]) -> SchemaIndex<'a> {
        SchemaIndex { fixed, varlena }
    }

    /// Snippet avec champ fixed (write_fmt) et champ varlena (html_escape).
    /// IfBool émet != 0 (u8 dans StorageRow).
    /// Aucun buf.reserve dans le snippet — c'est la responsabilité de l'orchestrateur.
    #[test]
    fn test_generate_aot_snippet_typed() {
        let fixed = vec![
            FieldSpec { name: "title".to_string(),     kind: FieldKind::I32, attnum: 1 },
            FieldSpec { name: "is_published".to_string(), kind: FieldKind::Bool, attnum: 2 },
        ];
        let varlena = vec![
            VarlenField {
                name: "body".to_string(),
                max_len: Some(1000),
                pre_escaped: false,
                nullable: true,
                max_escaped_len_override: None,
            },
        ];
        let schema = make_schema(&fixed, &varlena);

        let tokens: &[FlatPageToken<'_>] = &[
            FlatPageToken::Static("<article>"),
            FlatPageToken::Field   { entity: "record", field: "title" },
            FlatPageToken::Field   { entity: "varlena", field: "body" },
            FlatPageToken::IfBool  { entity: "record", field: "is_published" },
            FlatPageToken::Static("<span>publié</span>"),
            FlatPageToken::EndIf,
            FlatPageToken::StaticInclude {
                original_path:     "...",
                rel_from_manifest: "frag.html",
                len: 42,
            },
        ];

        let got = generate_aot_snippet(tokens, &schema);

        // Varlena ref déclarée en tête, triée.
        assert!(got.contains("let body_ref: Option<&str> = varlena.body.as_deref();"),
            "déclaration varlena absente:\n{got}");
        // Fixed → write_fmt.
        assert!(got.contains(r#"::std::fmt::Write::write_fmt(buf, format_args!("{}", record.title)).ok();"#),
            "write_fmt absent:\n{got}");
        // Varlena → html_escape.
        assert!(got.contains("if let Some(s) = body_ref { marius_html_escape(s, buf); }"),
            "html_escape absent:\n{got}");
        // IfBool → != 0 (u8).
        assert!(got.contains("if record.is_published != 0 {"),
            "condition u8 absente:\n{got}");
        // StaticInclude.
        assert!(got.contains(r#"buf.push_str(include_str!("frag.html"));"#),
            "include_str absent:\n{got}");
        // Pas de buf.reserve dans le snippet.
        assert!(!got.contains("buf.reserve"),
            "buf.reserve ne doit pas être dans le snippet:\n{got}");
    }

    /// Snippet sans varlena : aucune déclaration de ref.
    #[test]
    fn test_generate_aot_snippet_no_varlena() {
        let fixed = vec![FieldSpec { name: "id".to_string(), kind: FieldKind::I64, attnum: 1 }];
        let schema = make_schema(&fixed, &[]);
        let tokens: &[FlatPageToken<'_>] = &[
            FlatPageToken::Static("<p>"),
            FlatPageToken::Field { entity: "record", field: "id" },
            FlatPageToken::Static("</p>"),
        ];
        let got = generate_aot_snippet(tokens, &schema);
        assert!(!got.contains("_ref"), "pas de déclaration ref sans varlena:\n{got}");
        assert!(got.contains("record.id"), "champ id absent:\n{got}");
        assert!(!got.contains("buf.reserve"), "buf.reserve hors scope:\n{got}");
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
    pub name:     &'src str,
    /// Arène d'origine des indices `start`/`end` ci-dessous.
    pub template: TemplateId,
    /// Index de début de la plage de contenu (inclusif), dans l'AST référencé
    /// par `template`.
    pub start:    usize,
    /// Index de fin de la plage de contenu (exclusif), dans l'AST référencé
    /// par `template`.
    pub end:      usize,
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
    /// Portée Phase 4.5 : cette variante couvre aussi, temporairement, tout
    /// mot-clé de bloc encore non représentable par `PageSourceToken` à ce
    /// stade du classifieur (`extends`, et tout mot-clé hors grammaire
    /// runtime) — Document 1 §3 autorise explicitement l'échec de ces
    /// fichiers avant la clôture du classifieur (Phase 4.7). `block` et
    /// `endblock` en sont sortis en Phase 4.4 (branche `Block` dédiée) ;
    /// `static` en est sorti en Phase 4.5 (branche `Static` dédiée) ;
    /// `extends` migrera à son tour (4.6), puis le catch-all `Unsupported`
    /// clôturera la grammaire (4.7) — sans que cette variante d'erreur ne
    /// soit retirée : elle reste le domaine des erreurs de grammaire
    /// structurelle des mots-clés déjà reconnus (`if`/`endif`/`block`/
    /// `endblock`/`static`) — par exemple un `{% if %}` sans point dans
    /// l'identifiant, cf. `split_dotted_page`.
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
        ChildTemplateSpec, NamedBlockRange, PageBlockToken, PageComposeParseError,
        PageLinkError, PageValidationError, StaticPartialRef, TemplateId,
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
        let r_a = NamedBlockRange { name: "header", template: child_a, start: 3, end: 7 };
        let r_b = NamedBlockRange { name: "header", template: child_b, start: 3, end: 7 };
        let _copy = r_a; // Copy, pas de move

        assert_eq!(r_a.end - r_a.start, 4, "plage [start, end) : 4 tokens couverts");
        assert_ne!(r_a, r_b, "même range, arène différente : distinguable par valeur");
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
                NamedBlockRange { name: "header", template: this_child, start: 0, end: 2 },
                NamedBlockRange { name: "body",   template: this_child, start: 3, end: 9 },
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
        let r = StaticPartialRef { original_path: "partials/nav.html" };
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
        let _validation: PageValidationError<'_> =
            PageValidationError::ForLoopDetected;
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
//     Depuis, `block`/`endblock` (Phase 4.4) et `static` (Phase 4.5) sont
//     sortis de ce catch-all — voir sections dédiées ci-dessous. `extends`
//     (4.6) et le reste (4.7) migreront à leur tour, mot-clé par mot-clé,
//     sans anticipation.
//   - `{% block %}` / `{% endblock %}` (Phase 4.4) et `{% static %}` (Phase
//     4.5) : voir sections dédiées ci-dessous, qui étendent
//     `parse_page_block` (seule fonction modifiée à chaque fois) sans
//     toucher à ce dispatch de tête.

/// Construit l'AST `Vec<PageSourceToken<'src>>` d'un unique fichier, limité
/// au sous-ensemble `Runtime` de la grammaire Mode Page (Phase 4.3).
///
/// ─── Automate ──────────────────────────────────────────────────────────────
///
/// Structurellement identique à `parse_tokens` (Phase 1.3, gelé) : même
/// dispatch sur `SpanKind` en position de tête (`Literal` → `Static`,
/// `ExprOpen` → `Field`, `BlockOpen` → sous-automate de bloc), même primitives
/// de consommation (`expect_ident`/`expect_kind`, réimplémentées ici sous
/// domaine d'erreur `PageComposeParseError` pour ne pas coupler ce
/// classifieur au type d'erreur gelé `PageParseError` — Document 1 §0).
/// Chaque token produit est enveloppé sous `PageSourceToken::Runtime` avant
/// d'être poussé dans l'AST — c'est la seule différence structurelle avec
/// `parse_tokens`.
///
/// ─── Ce que cette fonction ne fait pas encore ──────────────────────────────
///
/// Ne calcule pas `ParsedPageTemplate::extends` (Phase 4.6) : le type de
/// sortie de cette phase est `Vec<PageSourceToken<'src>>` nu, pas encore
/// `ParsedPageTemplate` (Document 1 §2.2) — la signature finale, avec
/// extraction de la position d'`extends`, est posée en Phase 4.6 quand cette
/// information devient calculable. Ne reconnaît `extends`, `static`, ni le
/// catch-all `Unsupported` (voir note de portée ci-dessus) — seuls
/// `if`/`endif` (Phase 4.3) et `block`/`endblock` (Phase 4.4) le sont.
///
/// ─── Invariants mémoire ─────────────────────────────────────────────────────
///
/// Zéro allocation de texte : chaque `&'src str` porté par un
/// `PageSourceToken` est un emprunt direct sur `spans`, jamais une copie —
/// identique au contrat de `parse_tokens`. Le seul `Vec` alloué est celui de
/// sortie, build-time, conditionnel au premier `push` (cf. Document 1 §5).
pub fn parse_page_tokens<'src>(
    spans: impl Iterator<Item = RawSpan<'src>>,
) -> Result<Vec<PageSourceToken<'src>>, PageComposeParseError> {
    let mut iter = spans.peekable();
    let mut ast = Vec::new();

    while let Some(span) = iter.next() {
        let token = match span.kind {
            // Texte HTML verbatim → Static directement, enveloppé Runtime.
            SpanKind::Literal => PageSourceToken::Runtime(FlatPageToken::Static(span.slice)),

            // `{{ entity.field }}` → Field, enveloppé Runtime.
            SpanKind::ExprOpen => PageSourceToken::Runtime(parse_page_expr(&mut iter)?),

            // `{% keyword … %}` → IfBool | EndIf | BlockOpen | BlockEnd
            // (seuls mots-clés reconnus à ce stade). `parse_page_block`
            // retourne désormais directement le `PageSourceToken` englobant
            // — `if`/`endif` sous `Runtime`, `block`/`endblock` sous
            // `Block` — donc aucune enveloppe supplémentaire n'est apposée
            // ici (Phase 4.4 : voir doc de `parse_page_block`).
            SpanKind::BlockOpen => parse_page_block(&mut iter)?,

            // Tout autre span en position initiale est une erreur
            // structurelle : ExprClose, BlockClose, Ident, Punct ne peuvent
            // pas ouvrir un token, au même titre que dans `parse_tokens`.
            got => {
                return Err(PageComposeParseError::UnexpectedToken {
                    expected: "Literal | ExprOpen | BlockOpen",
                    got,
                })
            }
        };
        ast.push(token);
    }

    Ok(ast)
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

/// Consomme `Ident(keyword) … BlockClose` et produit le `PageSourceToken`
/// correspondant. Précondition : `BlockOpen` vient d'être consommé par
/// `parse_page_tokens`.
///
/// Portée Phase 4.5 : reconnaît `if`/`endif` (Phase 4.3, logique inchangée),
/// `block`/`endblock` (Phase 4.4, logique inchangée) et `static` (introduit
/// ici). Tout autre mot-clé (`extends`, `include`, ou inconnu) retourne
/// toujours `InvalidBlockSequence` — comportement temporaire, qui migrera
/// mot-clé par mot-clé dans les phases suivantes (4.6 `extends`, 4.7
/// catch-all `Unsupported`).
///
/// ─── Pourquoi le type de retour change : `PageSourceToken`, plus
///     `FlatPageToken` ──────────────────────────────────────────────────────
///
/// `block`/`endblock` n'ont pas d'équivalent dans `FlatPageToken` (grammaire
/// runtime, Document 1 §2.1) : ils s'enveloppent directement sous
/// `PageSourceToken::Block`, jamais sous `Runtime`. `static` non plus : il
/// s'enveloppe sous `PageSourceToken::Static(StaticPartialRef)` (Phase 4.5),
/// par le même raisonnement — aucune variante `FlatPageToken` équivalente,
/// cf. doc `StaticPartialRef` (distincte de `StaticInclude` par construction
/// de type). `if`/`endif`, eux, restent enveloppés sous `Runtime`,
/// exactement comme en Phase 4.3. Cette fonction devient donc le site
/// d'enveloppe unique pour tout mot-clé de bloc reconnu : `parse_page_tokens`
/// (dispatch de tête) n'a plus à choisir l'enveloppe lui-même, il propage
/// tel quel ce qui est retourné ici — un seul endroit décide de la variante
/// `PageSourceToken`, cohérent avec la règle « le Parser ne décide qu'une
/// fois de la forme d'un token ».
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
fn parse_page_block<'src, I>(iter: &mut I) -> Result<PageSourceToken<'src>, PageComposeParseError>
where
    I: Iterator<Item = RawSpan<'src>>,
{
    let keyword = expect_ident_page(iter, "keyword (if | endif | block | endblock | static)")?;

    match keyword {
        "if" => {
            let raw = expect_ident_page(iter, "Ident(entity.field)")?;
            let (entity, field) = split_dotted_page(raw)?;
            expect_kind_page(iter, SpanKind::BlockClose, "BlockClose('%}')")?;
            Ok(PageSourceToken::Runtime(FlatPageToken::IfBool {
                entity,
                field,
            }))
        }
        "endif" => {
            expect_kind_page(iter, SpanKind::BlockClose, "BlockClose('%}')")?;
            Ok(PageSourceToken::Runtime(FlatPageToken::EndIf))
        }
        "block" => {
            let name = expect_ident_page(iter, "Ident(name)")?;
            expect_kind_page(iter, SpanKind::BlockClose, "BlockClose('%}')")?;
            Ok(PageSourceToken::Block(PageBlockToken::BlockOpen { name }))
        }
        "endblock" => {
            expect_kind_page(iter, SpanKind::BlockClose, "BlockClose('%}')")?;
            Ok(PageSourceToken::Block(PageBlockToken::BlockEnd))
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
            Ok(PageSourceToken::Static(StaticPartialRef { original_path }))
        }
        _ => Err(PageComposeParseError::InvalidBlockSequence),
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
        parse_page_tokens, parse_tokens, scan, FlatPageToken, PageComposeParseError,
        PageSourceToken,
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

        assert_eq!(strip_runtime_envelope(actual), expected);
    }

    /// Jalon Vert — fixture `Field` seul : `{{ entity.field }}` produit la
    /// même structure sous les deux parseurs.
    #[test]
    fn runtime_subset_matches_parse_tokens_field_only() {
        let src = "{{ user.name }}";

        let expected = parse_tokens(scan(src)).expect("parse_tokens (référence) doit réussir");
        let actual =
            parse_page_tokens(scan(src)).expect("parse_page_tokens (classifieur) doit réussir");

        assert_eq!(strip_runtime_envelope(actual), expected);
    }

    /// Jalon Vert — fixture `IfBool`/`EndIf` : un bloc conditionnel complet
    /// produit la même structure sous les deux parseurs.
    #[test]
    fn runtime_subset_matches_parse_tokens_if_endif() {
        let src = "{% if user.active %}yes{% endif %}";

        let expected = parse_tokens(scan(src)).expect("parse_tokens (référence) doit réussir");
        let actual =
            parse_page_tokens(scan(src)).expect("parse_page_tokens (classifieur) doit réussir");

        assert_eq!(strip_runtime_envelope(actual), expected);
    }

    /// Jalon Vert — un mot-clé de composition (`extends`), hors scope de
    /// cette phase, échoue explicitement plutôt que d'être silencieusement
    /// accepté ou ignoré — comportement documenté, pas un effet de bord.
    #[test]
    fn composition_keyword_out_of_scope_fails_explicitly() {
        let src = r#"{% extends "base.marius" %}"#;
        let result = parse_page_tokens(scan(src));
        assert_eq!(result, Err(PageComposeParseError::InvalidBlockSequence));
    }
}

// =============================================================================
// Tests — Phase 4.4
// =============================================================================

#[cfg(test)]
mod tests_phase_4_4_block_endblock {
    use super::{parse_page_tokens, scan, FlatPageToken, PageBlockToken, PageSourceToken};

    /// Jalon Vert — template à 1 bloc top-level : `{% block name %}` produit
    /// exactement `BlockOpen { name }`, `{% endblock %}` produit exactement
    /// `BlockEnd`, le contenu intermédiaire reste `Runtime` inchangé.
    #[test]
    fn single_top_level_block_produces_block_open_and_block_end() {
        let src = "{% block header %}content{% endblock %}";

        let actual = parse_page_tokens(scan(src))
            .expect("parse_page_tokens doit réussir sur un bloc bien formé");

        assert_eq!(
            actual,
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

        let actual = parse_page_tokens(scan(src));

        assert_eq!(
            actual,
            Ok(vec![
                PageSourceToken::Block(PageBlockToken::BlockOpen { name: "outer" }),
                PageSourceToken::Block(PageBlockToken::BlockOpen { name: "inner" }),
                PageSourceToken::Runtime(FlatPageToken::Static("x")),
                PageSourceToken::Block(PageBlockToken::BlockEnd),
                PageSourceToken::Block(PageBlockToken::BlockEnd),
            ])
        );
    }
}

// =============================================================================
// Tests — Phase 4.5
// =============================================================================

#[cfg(test)]
mod tests_phase_4_5_static {
    use super::{parse_page_tokens, scan, FlatPageToken, PageSourceToken, StaticPartialRef};

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
            actual,
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
