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
    pk_field: &str,
    varlena:  &[VarlenField],
) -> (usize, usize, String) {
    let sc      = static_capacity(schema, table, fields, varlena);
    let dc      = dynamic_capacity(fields, pk_field, varlena);
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
         buf.push_str(\"\\\"><dl>\");\n"
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
