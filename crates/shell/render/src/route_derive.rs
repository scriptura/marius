// crates/shell/render/src/route_derive.rs

//! Dérivation de la représentation SERVEUR d'une route (`RouteEntry`) depuis
//! la déclaration neutre (`marius_projection::publication::RouteSpec`).
//!
//! `RouteSpec` est GÉNÉRÉE par le build de `marius-schema` à partir de
//! `publication.toml` (seule définition de la relation route → artefact →
//! paramètre). Ce module est le seul endroit où elle devient un `RouteEntry` :
//! ni `marius-server` ni `marius-dump` n'en écrivent plus une copie à la main.
//!
//! ## Frontière
//!
//! - Cette dérivation est PURE et `const` : elle sert dans les initialiseurs
//!   de `static` (`ROUTE_TABLE`, `DUMP_ROUTE_TABLE`).
//! - Elle ne porte que ce que `RouteEntry` demande. La politique de parsing
//!   du paramètre HTTP et le code 400 restent côté serveur (`handlers.rs`) :
//!   `RouteSpec` n'en porte aucune.
//! - La dérivation vers `RouteDescriptor` (T2A) n'est PAS ici : elle est
//!   générée directement par le build (`ROUTE_DESCRIPTORS`), `marius-render`
//!   restant transport-agnostique.

use marius_projection::publication::{RouteSelection, RouteSpec};

use crate::registry::{IdSource, RouteEntry};

/// Type de contenu des routes HTML servies par le chemin monolithique.
///
/// `RouteEntry.content_type` n'est lu par aucun handler aujourd'hui
/// (`deliver` fixe son propre en-tête) ; la valeur reprend exactement celle de
/// l'ancienne `ROUTE_TABLE` écrite à la main, pour que la dérivation soit à
/// l'identique de ce qu'elle remplace. `RouteSpec` étant neutre du format,
/// elle ne le porte pas.
const HTML_CONTENT_TYPE: &str = "text/html; charset=utf-8";

/// `RouteEntry` correspondant à une `RouteSpec`.
///
/// - `pattern`      ← `spec.pattern`
/// - `packfile_key` ← clé de l'artefact (`spec.artifact`)
/// - `id_source`    ← `PathParam(spec.parameter)` : le nom du paramètre HTTP,
///   JAMAIS la colonne SQL de la clé primaire (`RouteSelection::PrimaryKey`
///   porte celle-ci, sans rapport de nom avec le paramètre).
pub const fn route_entry_from_spec(spec: &RouteSpec) -> RouteEntry {
    match spec.selection {
        RouteSelection::PrimaryKey { .. } => RouteEntry {
            pattern: spec.pattern,
            packfile_key: spec.artifact.as_str(),
            id_source: IdSource::PathParam(spec.parameter),
            content_type: HTML_CONTENT_TYPE,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use marius_projection::publication::ArtifactKey;

    /// La route réelle, telle que générée depuis `publication.toml`.
    #[test]
    fn derives_the_server_entry_from_the_generated_content_route() {
        let entry = route_entry_from_spec(&marius_schema::CONTENT_DOCUMENT_ROUTE);

        assert_eq!(entry.pattern, "/content/{id}");
        assert_eq!(entry.packfile_key, "content_core");
        assert!(
            matches!(entry.id_source, IdSource::PathParam("id")),
            "le paramètre HTTP est `id`"
        );
        assert_eq!(entry.content_type, "text/html; charset=utf-8");
    }

    /// Forme exacte d'usage dans `main.rs` et `dump.rs` : la dérivation doit
    /// être évaluable dans un initialiseur de `static`.
    #[test]
    fn derivation_is_usable_in_a_static_initializer() {
        static TABLE: &[RouteEntry] = &[route_entry_from_spec(
            &marius_schema::CONTENT_DOCUMENT_ROUTE,
        )];

        assert_eq!(TABLE.len(), 1);
        assert_eq!(TABLE[0].pattern, "/content/{id}");
        assert_eq!(TABLE[0].packfile_key, "content_core");
    }

    /// Le paramètre HTTP et la colonne PK sont deux identités : c'est le
    /// paramètre qui devient `PathParam`, jamais la colonne.
    #[test]
    fn path_param_carries_the_http_parameter_not_the_pk_column() {
        const SPEC: RouteSpec = RouteSpec {
            name: "sample",
            pattern: "/things/{thing}",
            artifact: ArtifactKey::new("things"),
            parameter: "thing",
            selection: RouteSelection::PrimaryKey { column: "thing_pk" },
        };
        let entry = route_entry_from_spec(&SPEC);

        assert_eq!(entry.pattern, "/things/{thing}");
        assert_eq!(entry.packfile_key, "things");
        assert!(matches!(entry.id_source, IdSource::PathParam("thing")));
    }

    /// `marius-dump` ne garde plus de copie de la définition de route : ni le
    /// motif d'URL, ni la clé d'artefact n'y figurent en littéral (audit
    /// textuel, même technique que celui d'`experimental_t2a.rs`).
    #[test]
    fn dump_binary_holds_no_copy_of_the_route_definition() {
        let dump = include_str!("bin/dump.rs");
        assert!(
            !dump.contains("\"/content/{id}\""),
            "dump.rs ne doit plus redéclarer le motif de la route"
        );
        assert!(
            !dump.contains("\"content_core\""),
            "dump.rs ne doit plus écrire la clé d'artefact en littéral"
        );
    }
}
