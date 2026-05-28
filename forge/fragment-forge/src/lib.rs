// marius-fragment-forge
// Générateur de code de rendu HTML pour le moteur Marius.
//
// Appelé depuis crates/core/schema/build.rs au moment de la compilation.
// Produit pour chaque table surveillée :
//   1. const {NAME}_STATIC_CAP  : taille des chaînes HTML statiques (octets)
//   2. const {NAME}_DYNAMIC_CAP : largeur maximale des champs dynamiques (octets)
//   3. fn render(record: &{Name}, buf: &mut String)
//      → serie de push_str (statique) + write! (dynamique)
//      → zéro allocation intermédiaire (pas de html! { } → String → push_str)
//
// Contraintes (directives Session 5) :
//   - Zéro logique dans le template : aucun if/match dans le corps généré.
//   - Zéro runtime de templating : le code généré est des appels directs push_str/write!
//   - Capacité calculée statiquement : STATIC_CAP + DYNAMIC_CAP = borne supérieure exacte.

// =============================================================================
// I. Types internes
// =============================================================================

/// Catégorie d'un champ fixed-length pour le calcul de capacité dynamique.
#[derive(Debug, Clone, Copy)]
pub enum FieldKind {
    I64,  // TIMESTAMPTZ, BIGINT  → i64::MIN = -9223372036854775808 → 20 chars
    I32,  // INTEGER              → i32::MIN = -2147483648          → 11 chars
    I16,  // SMALLINT             → i16::MIN = -32768               → 6 chars
    Bool, // BOOLEAN              → "false"                         → 5 chars
    F32,  // REAL                 → représentation flottante max     → 14 chars
    F64,  // DOUBLE PRECISION     → représentation flottante max     → 24 chars
}

impl FieldKind {
    /// Largeur d'affichage maximale du champ (pire cas, pas de padding HTML).
    /// Utilisée pour calculer DYNAMIC_CAP sans sous-estimer.
    pub const fn max_display_width(self) -> usize {
        match self {
            Self::I64  => 20,  // "-9223372036854775808"
            Self::I32  => 11,  // "-2147483648"
            Self::I16  => 6,   // "-32768"
            Self::Bool => 5,   // "false"
            Self::F32  => 14,  // "-3.40282347e38" (approx)
            Self::F64  => 24,  // représentation longue
        }
    }

    /// Construit un FieldKind depuis le type SQL retourné par format_type().
    pub fn from_sql_type(sql_type: &str) -> Option<Self> {
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
            _ => None, // varlena, pg_lsn, inconnu → exclu du rendu dynamique
        }
    }
}

/// Spécification d'un champ fixed-length pour le Fragment-Forge.
#[derive(Debug, Clone)]
pub struct FieldSpec {
    pub name:      String,
    pub kind:      FieldKind,
    pub attnum:    i16,
}

// =============================================================================
// II. Calcul de capacité
// =============================================================================

/// Taille en octets de la chaîne HTML statique entourant les valeurs dynamiques.
/// Calcul purement arithmétique — zéro allocation, zéro construction de String.
pub fn static_capacity(schema: &str, table: &str, fields: &[FieldSpec]) -> usize {
    // "<article class=\"{schema}-{table}\" data-id=\""
    //  = "<article class=\"" (16) + schema + "-" (1) + table + "\" data-id=\"" (11)
    let open_tag = 16 + schema.len() + 1 + table.len() + 11;

    // "\"><dl>" = '"' + '>' + "<dl>" = 1 + 1 + 4 = 6
    // (ferme la valeur de data-id, ferme le tag article, ouvre dl)
    let after_id = 6;

    let mut cap = open_tag + after_id;

    // Par champ : "<dt>" (4) + nom + "</dt><dd>" (9) + "</dd>" (5)
    for f in fields {
        cap += 4 + f.name.len() + 9 + 5;
    }

    // "</dl></article>" = 15
    cap += 15;

    cap
}

/// Somme des largeurs maximales de tous les champs dynamiques.
/// Inclut les champs utilisés dans les attributs data (data-id) en plus du corps.
pub fn dynamic_capacity(fields: &[FieldSpec]) -> usize {
    let body_cap: usize = fields.iter()
        .map(|f| f.kind.max_display_width())
        .sum();

    // data-id dans le tag ouvrant = PK field (premier champ)
    let data_id_cap = fields.first()
        .map(|f| f.kind.max_display_width())
        .unwrap_or(0);

    body_cap + data_id_cap
}

// =============================================================================
// III. Génération du corps de render()
// =============================================================================

/// Génère le corps complet de la fonction render().
///
/// `pk_field` : nom du champ PK transmis par write_projection_stub depuis pg_constraint.
/// Utilisé pour l'attribut data-id — ne doit pas être déduit de l'ordre attnum.
pub fn generate_render(
    schema:   &str,
    table:    &str,
    _name:     &str,
    fields:   &[FieldSpec],
    pk_field: &str,
) -> (usize, usize, String) {
    let static_cap  = static_capacity(schema, table, fields);
    let dynamic_cap = dynamic_capacity(fields);
    let css_class   = format!("{schema}-{table}");

    let mut code = String::new();

    // Constantes de capacité — émises dans le corps de la fonction.
    code.push_str(&format!(
        "    // Capacités calculées par Fragment-Forge à la compilation.\n\
         // STATIC_CAP : somme exacte des octets HTML statiques.\n\
         // DYNAMIC_CAP : somme des largeurs maximales des champs (pire cas).\n\
         // Invariant : buf.capacity() NE DOIT PAS augmenter pendant render().\n\
         const STATIC_CAP:  usize = {static_cap};\n\
         const DYNAMIC_CAP: usize = {dynamic_cap};\n\
         buf.reserve(STATIC_CAP + DYNAMIC_CAP);\n"
    ));

    // Ouverture du tag + data-id (PK)
    code.push_str(&format!(
        "buf.push_str(\"<article class=\\\"{css_class}\\\" data-id=\\\"\");\n\
         ::std::fmt::Write::write_fmt(buf, format_args!(\"{{}}\", record.{pk_field})).ok();\n\
         buf.push_str(\"\\\"><dl>\");\n"
    ));

    // Corps : un <dt>/<dd> par champ
    for f in fields {
        code.push_str(&format!(
            "buf.push_str(\"<dt>{name}</dt><dd>\", );\n\
             ::std::fmt::Write::write_fmt(buf, format_args!(\"{{}}\", record.{name})).ok();\n\
             buf.push_str(\"</dd>\");\n",
            name = f.name
        ));
    }

    // Fermeture
    code.push_str("buf.push_str(\"</dl></article>\");\n");

    (static_cap, dynamic_cap, code)
}

// =============================================================================
// IV. Constantes émises dans le fichier généré (scope module)
// =============================================================================

/// Génère les constantes de capacité au niveau du module (pas dans la fonction).
/// Utilisées par les tests d'intégration pour vérifier l'absence de realloc.
pub fn generate_capacity_consts(screaming_name: &str, static_cap: usize, dynamic_cap: usize) -> String {
    format!(
        "pub const {screaming_name}_STATIC_CAP:  usize = {static_cap};\n\
         pub const {screaming_name}_DYNAMIC_CAP: usize = {dynamic_cap};\n\
         pub const {screaming_name}_TOTAL_CAP:   usize = {total};\n",
        total = static_cap + dynamic_cap,
    )
}
