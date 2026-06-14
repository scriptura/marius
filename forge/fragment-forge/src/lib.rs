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
//   Fragment-Forge reçoit l'information via VarlenField::is_pre_escaped.
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

pub mod orchestrator;
pub mod prologue;
pub mod body;
pub mod generator;

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
///   Facteur normal     : HTML_ESCAPE_FACTOR = 5
///     Pire cas : chaque caractère source est remplacé par "&amp;" (5 chars).
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
    pub name:          String,
    /// Longueur maximale en octets (contrainte DDL ou fallback selon politique).
    pub max_len:       usize,
    /// true si le contenu est certifié pré-échappé (tag 'marius:pre_escaped').
    /// Facteur de capacité = 1 au lieu de 5.
    pub is_pre_escaped: bool,
}

impl VarlenField {
    /// Facteur d'escape HTML pire cas (champ non annoté).
    ///
    /// '&' → '&amp;' = 1 char source → 5 chars HTML.
    /// Tout le contenu pourrait être des '&', d'où le facteur 5.
    pub const HTML_ESCAPE_FACTOR: usize = 5;

    /// Longueur maximale après escape HTML, en octets.
    ///
    /// Composante varlena de DYNAMIC_CAP.
    /// Facteur 1 si is_pre_escaped (contenu certifié sans '&<>"\'').
    /// Facteur 5 sinon (pire cas).
    pub fn max_escaped_len(&self) -> usize {
        if self.is_pre_escaped {
            self.max_len
        } else {
            self.max_len * Self::HTML_ESCAPE_FACTOR
        }
    }
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
/// Elles sont comptabilisées dans dynamic_capacity().
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

/// Somme des largeurs maximales de toutes les valeurs dynamiques.
///
/// Composé de trois sous-totaux :
///
///   1. data-id (PK dans le tag ouvrant) : max_display_width du champ PK.
///      Le champ PK est identifié par son nom (pk_field), pas par sa position.
///      Fallback : 11 (max i32) si le nom PK ne correspond à aucun FieldSpec.
///
///   2. Corps fixed-length : somme des max_display_width de tous les FieldSpec.
///      Inclut la PK une deuxième fois (elle apparaît aussi dans le corps <dl>).
///
///   3. Corps varlena : somme des max_escaped_len de tous les VarlenField.
///      Tient compte du facteur d'escape (5 ou 1 selon is_pre_escaped).
///
/// Invariant : buf.reserve(STATIC_CAP + DYNAMIC_CAP) doit suffire pour tout
/// enregistrement possible. Une sous-estimation provoque un realloc.
pub fn dynamic_capacity(
    fields:   &[FieldSpec],
    pk_field: &str,
    varlena:  &[VarlenField],
) -> usize {
    // Contribution PK dans data-id (tag ouvrant).
    // Recherche par nom, pas par position attnum, car la PK peut être en attnum=2.
    let data_id_cap: usize = fields.iter()
        .find(|f| f.name == pk_field)
        .map(|f| f.kind.max_display_width())
        .unwrap_or(11); // fallback i32::MIN pour PK INT4 implicite

    // Contribution des champs fixed-length dans le corps <dl>.
    let fixed_cap: usize = fields.iter()
        .map(|f| f.kind.max_display_width())
        .sum();

    // Contribution des champs varlena dans le corps <dl>.
    // max_escaped_len() applique le bon facteur selon is_pre_escaped.
    let varlena_cap: usize = varlena.iter()
        .map(|v| v.max_escaped_len())
        .sum();

    data_id_cap + fixed_cap + varlena_cap
}

// =============================================================================
// III. Génération du corps de render()
// =============================================================================

/// Génère le corps complet de la fonction render().
///
/// Signature de la fonction générée (conforme au trait Projection ADR-003) :
/// ```text
/// fn render(record: &{Name}StorageRow, varlena: &{Name}VarlenOwned, buf: &mut String)
/// ```
///
/// ─── Paramètres ──────────────────────────────────────────────────────────────
///
///   schema, table : identifiants DDL → classe CSS et data-id.
///   fields        : champs fixed-length dans l'ordre attnum.
///   pk_field      : nom du champ PK (depuis pg_constraint, pas l'ordre attnum).
///   varlena       : champs varlena de la table jointe, avec leurs bornes.
///
/// ─── Corps généré ────────────────────────────────────────────────────────────
///
///   1. buf.reserve(STATIC_CAP + DYNAMIC_CAP)  — pré-allocation exacte.
///   2. Reconstruction locale du RenderPayload via as_deref() (zéro copie).
///      let {nom}_ref: Option<&str> = varlena.{nom}.as_deref();
///      Durée de vie locale à render() : pas de traversée de frontière de thread.
///   3. push_str("<article ...>")              — balise ouvrante statique.
///   4. write_fmt(record.pk_field)             — valeur PK dynamique.
///   5. push_str("><dl>")                      — transition statique.
///   6. Pour chaque champ fixed-length :
///      push_str("<dt>nom</dt><dd>")
///      write_fmt(record.nom)
///      push_str("</dd>")
///   7. Pour chaque champ varlena :
///      push_str("<dt>nom</dt><dd>")
///      if let Some(s) = {nom}_ref { marius_html_escape(s, buf); }
///      push_str("</dd>")
///   8. push_str("</dl></article>")           — balise fermante statique.
///
/// ─── Pourquoi as_deref() local et non &RenderPayload<'_> dans la signature ───
///
///   Le trait Projection déclare render(&Self::Record, &Self::VarlenOwned, &mut String).
///   VarlenOwned est 'static + Send (Option<String> possédées).
///   RenderPayload<'a> (Option<&'a str>) n'est pas 'static, ne traverse pas
///   les threads Rayon, et n'appartient pas à l'interface publique du trait.
///   Fragment-Forge reconstruit les &str localement dans le corps de render() :
///   le lifetime est inféré depuis varlena (paramètre de render), jamais exposé.
///
/// ─── Retour ──────────────────────────────────────────────────────────────────
///
///   (static_cap, dynamic_cap, corps_render) — les deux premières valeurs
///   permettent à build.rs d'émettre les constantes de capacité au niveau module.
pub fn generate_render(
    schema:   &str,
    table:    &str,
    _name:    &str,
    fields:   &[FieldSpec],
    pk_field: &FieldSpec,
    varlena:  &[VarlenField],
) -> (usize, usize, String) {
    let sc      = static_capacity(schema, table, fields, varlena);
    let dc      = dynamic_capacity(fields, &pk_field.name, varlena);
    let css     = format!("{schema}-{table}");
    let mut c   = String::new();

    // ─── Pré-allocation ───────────────────────────────────────────────────────
    // STATIC_CAP + DYNAMIC_CAP = borne supérieure exacte calculée à la compilation.
    // Invariant : buf.capacity() ne doit pas croître pendant render().
    // Le test no-realloc vérifie cet invariant avec les valeurs pires cas.
    c.push_str(&format!(
        "// Capacités calculées par Fragment-Forge à la compilation.\n\
         // STATIC_CAP : somme exacte des octets HTML statiques (balises + noms de champs).\n\
         // DYNAMIC_CAP : somme des largeurs maximales des valeurs (fixed × width, varlena × escape_factor).\n\
         // Invariant : buf.capacity() NE DOIT PAS augmenter pendant render().\n\
         const STATIC_CAP:  usize = {sc};\n\
         const DYNAMIC_CAP: usize = {dc};\n\
         buf.reserve(STATIC_CAP + DYNAMIC_CAP);\n"
    ));

    // ─── Reconstruction locale du RenderPayload ───────────────────────────────
    // as_deref() : Option<String> → Option<&str> sans copie (réaffectation de fat pointer).
    // Durée de vie des &str : liée à `varlena` (paramètre de render), inférée par le
    // compilateur. Aucune traversée de frontière de thread — uniquement local à render().
    // VarlenOwned est 'static et traverse Rayon ; ces &str locaux ne le font pas.
    if !varlena.is_empty() {
        c.push_str("// Reconstruction locale des &str depuis VarlenOwned (as_deref, zéro copie).\n");
        for v in varlena {
            let n = &v.name;
            c.push_str(&format!(
                "let {n}_ref: Option<&str> = varlena.{n}.as_deref();\n"
            ));
        }
    }

    // ─── Tag ouvrant + PK (data-id) ──────────────────────────────────────────
    // La PK est lue depuis record (StorageRow, repr(C), champ fixed-length).
    // Elle n'est jamais dans varlena (VarlenOwned ne porte que les champs texte JOIN).
    c.push_str(&format!(
        "buf.push_str(\"<article class=\\\"{css}\\\" data-id=\\\"\");\n\
         ::std::fmt::Write::write_fmt(buf, format_args!(\"{{}}\", record.{pk_field})).ok();\n\
         buf.push_str(\"\\\"><dl>\");\n",
        pk_field = pk_field.name
    ));

    // ─── Champs fixed-length ─────────────────────────────────────────────────
    // Lus depuis record (StorageRow, repr(C)).
    // write_fmt() : zéro allocation — écrit directement dans buf via fmt::Write.
    for f in fields {
        let n = &f.name;
        c.push_str(&format!(
            "buf.push_str(\"<dt>{n}</dt><dd>\");\n\
             ::std::fmt::Write::write_fmt(buf, format_args!(\"{{}}\", record.{n})).ok();\n\
             buf.push_str(\"</dd>\");\n"
        ));
    }

    // ─── Champs varlena ──────────────────────────────────────────────────────
    // Lus depuis les {n}_ref locaux construits par as_deref() ci-dessus.
    // marius_html_escape() : boucle char par char dans buf, zéro allocation.
    // Le branchement Option est inévitable (LEFT JOIN peut retourner NULL).
    // Aucune autre logique conditionnelle n'est autorisée dans le corps généré.
    for v in varlena {
        let n = &v.name;
        c.push_str(&format!(
            "buf.push_str(\"<dt>{n}</dt><dd>\");\n\
             if let Some(s) = {n}_ref {{ marius_html_escape(s, buf); }}\n\
             buf.push_str(\"</dd>\");\n"
        ));
    }

    // ─── Tag fermant ─────────────────────────────────────────────────────────
    c.push_str("buf.push_str(\"</dl></article>\");\n");

    (sc, dc, c)
}

// =============================================================================
// IV. Constantes de capacité émises au niveau module
// =============================================================================

/// Génère les trois constantes de capacité au niveau du module (pas dans un impl).
///
/// Ces constantes sont émises entre la définition de la struct Projection
/// et le bloc impl, pour deux raisons :
///
///   1. Les constantes associées déclarées dans un impl doivent figurer dans le trait.
///      Projection ne déclare pas STATIC_CAP etc. → elles ne peuvent pas être
///      des items du impl.
///
///   2. Les tests unitaires no-realloc les référencent directement depuis le module,
///      sans instancier la Projection.
///
/// Nommage : {SCREAMING_SNAKE_TABLE}_STATIC_CAP, _DYNAMIC_CAP, _TOTAL_CAP.
pub fn generate_capacity_consts(screaming: &str, sc: usize, dc: usize) -> String {
    format!(
        "/// Octets HTML statiques (balises + noms de champs) pour {screaming}.\n\
         pub const {screaming}_STATIC_CAP:  usize = {sc};\n\
         /// Largeurs maximales des valeurs dynamiques pour {screaming} (pire cas).\n\
         pub const {screaming}_DYNAMIC_CAP: usize = {dc};\n\
         /// Capacité totale = STATIC_CAP + DYNAMIC_CAP. Utiliser pour String::with_capacity().\n\
         pub const {screaming}_TOTAL_CAP:   usize = {total};\n",
        total = sc + dc,
    )
}

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
    pub total_static_bytes: usize,
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
    IoError { path: &'src str, details: String },
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
    get_file_size: impl Fn(&str) -> Result<usize, String>,
) -> Result<TemplateMetrics, Vec<ResolverError<'src>>> {
    let mut metrics = TemplateMetrics { total_static_bytes: 0, include_count: 0 };
    let mut errors:  Vec<ResolverError<'src>> = Vec::new();

    for token in tokens.iter_mut() {
        match token {

            // Octets HTML connus statiquement : contribution directe.
            // `s : &mut &'src str` — auto-deref vers (&'src str).len().
            FlatPageToken::Static(s) => {
                metrics.total_static_bytes += s.len();
            }

            // Inclusion externe : résolution I/O et mutation en place.
            //
            // Décomposition des borrows :
            //   rel_from_manifest : &mut &'src str  (champ A)
            //   len               : &mut usize      (champ B)
            //
            // `let path = *rel_from_manifest` copie le &'src str hors du borrow
            // (Copy). NLL libère immédiatement le borrow de `rel_from_manifest`.
            // `*len = size` opère sur le champ B, indépendant (field splitting).
            // Pas de conflit de borrow.
            FlatPageToken::StaticInclude { rel_from_manifest, len, .. } => {
                let path = *rel_from_manifest;          // &'src str, copie sans alloc
                match get_file_size(path) {             // coercion &'src str → &str
                    Ok(size) => {
                        *len = size;                    // mutation en place du champ len
                        metrics.total_static_bytes += size;
                        metrics.include_count      += 1;
                    }
                    Err(details) => {
                        errors.push(ResolverError::IoError { path, details });
                    }
                }
            }

            // Field, IfBool, EndIf : coût mémoire runtime-dépendant.
            // Non comptabilisés dans PAGE_STATIC_CAP. Phase 2.2.
            _ => {}
        }
    }

    if errors.is_empty() { Ok(metrics) } else { Err(errors) }
}

// =============================================================================
// Tests — Phase 2.1
// =============================================================================

#[cfg(test)]
mod tests_phase_2_1 {
    use super::{resolve_and_measure, ResolverError, TemplateMetrics, FlatPageToken};

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

        let result = resolve_and_measure(&mut tokens, |path| match path {
            "a.html" => Ok(10),
            other    => Err(format!("unknown : {other}")),
        });

        // Métriques correctes.
        assert_eq!(
            result,
            Ok(TemplateMetrics { total_static_bytes: 16, include_count: 1 }),
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

        let result = resolve_and_measure(&mut tokens, |path| match path {
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
    /// Field ne contribue pas.
    #[test]
    fn test_resolve_no_includes() {
        let mut tokens = vec![
            FlatPageToken::Static("<p>"),
            FlatPageToken::Field { entity: "user", field: "name" },
            FlatPageToken::Static("</p>"),
        ];

        assert_eq!(
            resolve_and_measure(
                &mut tokens,
                |_| unreachable!("get_file_size ne doit pas être appelé sans StaticInclude"),
            ),
            Ok(TemplateMetrics { total_static_bytes: 7, include_count: 0 }),
        );
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
/// # Sortie
/// Une `String` de code source Rust. Chaque ligne est terminée par `\n`.
/// Le snippet est prévu pour être encapsulé dans le corps d'une fonction
/// `render_page()` par l'orchestrateur `build.rs`.
///
/// # Allocation de `out` (build-time uniquement)
/// La contrainte "zéro allocation" s'applique au code *généré* (runtime).
/// Le générateur lui-même s'exécute à la compilation et peut allouer librement.
/// La capacité initiale de `out` est estimée pour éviter les réallocations
/// internes à la génération : un `with_capacity` par invocation de la fonction.
pub fn generate_aot_snippet<'src>(
    tokens:  &[FlatPageToken<'src>],
    metrics: &TemplateMetrics,
) -> String {
    // Import du trait Write pour `writeln!` sur String.
    // `as _` : trait importé pour ses méthodes uniquement,
    // nom non introduit dans le scope (évite le conflit avec std::io::Write).
    use std::fmt::Write as _;

    // Estimation de la taille du code source généré :
    //   ~25 bytes pour le prologue buf.reserve()
    //   ~40 bytes par token (heuristique conservative)
    // Surestimer → zéro réallocation de `out` dans le cas nominal.
    let mut out = String::with_capacity(25 + tokens.len() * 40);

    // ── Prologue : instruction de pré-allocation ──────────────────────────────
    // Invariant DOD : toujours la première ligne du snippet.
    writeln!(out, "buf.reserve({});", metrics.total_static_bytes).unwrap();

    // ── État d'indentation ────────────────────────────────────────────────────
    // Deux valeurs possibles (Phase 1.4 garantit zéro imbrication) :
    //   ""     → flux principal
    //   "    " → intérieur d'un bloc IfBool
    //
    // `&'static str` : aucune allocation pour l'indentation elle-même.
    let mut indent: &str = "";

    // ── Parcours linéaire de l'AST ─────────────────────────────────────────
    for token in tokens {
        match token {

            // ── Static ─────────────────────────────────────────────────────
            // `{:?}` sur &str → littéral Rust entre guillemets avec escapes.
            // Exemples :
            //   "<div>"         →  buf.push_str("<div>");
            //   "O'Brien & Co." →  buf.push_str("O'Brien & Co.");
            //   "a\nb"          →  buf.push_str("a\nb");
            FlatPageToken::Static(s) => {
                writeln!(out, "{}buf.push_str({:?});", indent, s).unwrap();
            }

            // ── Field ──────────────────────────────────────────────────────
            // L'appelant garantit que `entity.field` implémente AsRef<str>
            // ou expose une méthode retournant &str.
            FlatPageToken::Field { entity, field } => {
                writeln!(out, "{}buf.push_str(&{}.{});", indent, entity, field).unwrap();
            }

            // ── IfBool ─────────────────────────────────────────────────────
            // Émet la ligne `if e.f {` AU NIVEAU COURANT,
            // puis bascule l'indentation au niveau 1 pour les tokens suivants.
            FlatPageToken::IfBool { entity, field } => {
                writeln!(out, "{}if {}.{} {{", indent, entity, field).unwrap();
                indent = "    ";
            }

            // ── EndIf ──────────────────────────────────────────────────────
            // Rétablit le niveau 0 AVANT d'émettre `}` :
            // l'accolade fermante est toujours au niveau principal.
            // `push_str` évite le formatage inutile sur un littéral connu.
            FlatPageToken::EndIf => {
                indent = "";
                out.push_str("}\n");
            }

            // ── StaticInclude ──────────────────────────────────────────────
            // `include_str!` est une macro Rust résolue à la compilation du
            // code *généré* (second passage du compilateur).
            // `{:?}` sur le chemin → guillemets + échappement des backslashes
            // (chemins Windows : r"dir\file" → "dir\\file" dans le source).
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
    use super::{generate_aot_snippet, FlatPageToken, TemplateMetrics};

    /// Jalon Vert Phase 2.2 — sortie caractère par caractère.
    ///
    /// Séquence : Static, Field, IfBool, Static (indenté), EndIf, StaticInclude.
    /// Vérifie l'exactitude du prologue, des niveaux d'indentation,
    /// des guillemets injectés par {;?} et du format include_str!.
    #[test]
    fn test_generate_aot_snippet() {
        let metrics = TemplateMetrics { total_static_bytes: 120, include_count: 1 };

        let tokens: &[FlatPageToken<'_>] = &[
            FlatPageToken::Static("<div>"),
            FlatPageToken::Field  { entity: "user", field: "name"   },
            FlatPageToken::IfBool { entity: "user", field: "active" },
            FlatPageToken::Static("<p>ON</p>"),
            FlatPageToken::EndIf,
            FlatPageToken::StaticInclude {
                original_path:     "...",
                rel_from_manifest: "../frag.html",
                len:               42,
            },
        ];

        let got = generate_aot_snippet(tokens, &metrics);

        // Le raw string r#"..."# préserve les `"` littéraux,
        // ce qui permet d'asserter les guillemets injectés par {;?}
        // sans séquences d'échappement supplémentaires dans le test.
        let expected = r#"buf.reserve(120);
buf.push_str("<div>");
buf.push_str(&user.name);
if user.active {
    buf.push_str("<p>ON</p>");
}
buf.push_str(include_str!("../frag.html"));
"#;

        assert_eq!(
            got, expected,
            "\n\nSortie obtenue :\n{got}\n\nSortie attendue :\n{expected}"
        );
    }

    /// Snippet sans bloc conditionnel : vérifie que l'indentation reste à 0
    /// et que la sortie ne contient aucune accolade.
    #[test]
    fn test_generate_flat_snippet() {
        let metrics = TemplateMetrics { total_static_bytes: 13, include_count: 0 };

        let tokens: &[FlatPageToken<'_>] = &[
            FlatPageToken::Static("<p>"),
            FlatPageToken::Field { entity: "article", field: "title" },
            FlatPageToken::Static("</p>"),
        ];

        let got = generate_aot_snippet(tokens, &metrics);

        let expected = r#"buf.reserve(13);
buf.push_str("<p>");
buf.push_str(&article.title);
buf.push_str("</p>");
"#;

        assert_eq!(got, expected);
        assert!(
            !got.contains('{'),
            "un snippet sans IfBool ne doit contenir aucune accolade",
        );
    }

    /// Vérifie l'échappement automatique via Debug :
    /// une chaîne contenant `"` et `\n` doit produire un littéral Rust valide.
    #[test]
    fn test_debug_escaping_in_static() {
        let metrics  = TemplateMetrics { total_static_bytes: 0, include_count: 0 };
        let tokens: &[FlatPageToken<'_>] = &[
            FlatPageToken::Static("a\"b\nc"),
        ];

        let got = generate_aot_snippet(tokens, &metrics);

        // {;?} sur r#"a"b\nc"# produit "a\"b\nc" (littéral Rust syntaxiquement valide).
        assert!(
            got.contains(r#""a\"b\nc""#),
            "les guillemets et newlines doivent être échappés par Debug : {got:?}",
        );
    }
}







