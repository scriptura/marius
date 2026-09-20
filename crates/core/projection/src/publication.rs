// crates/core/projection/src/publication.rs

//! Vocabulaire neutre de publication/exposition AOT.
//!
//! Ce module ne contient que des DONNÉES et des fonctions pures. Aucun type
//! de `marius-render`, `marius-server`, Axum, Hyper, SQL ou `.marius` n'y
//! apparaît : c'est la condition pour que la même déclaration serve à
//! dériver, séparément, la représentation serveur (`RouteEntry`, dans
//! `marius-render`) et la représentation T2A (`RouteDescriptor`, ici même
//! dans `marius-projection`).
//!
//! ## Trois identités, trois niveaux (arbitrage d'architecture)
//!
//! ```text
//! component_id   identité logique du composant Forge / modèle de données
//!                ("content.core") — vit dans le registre `meta`, pas ici.
//! ArtifactKey    identité logique de l'artefact publiable ("content_core").
//! SourceKey(u16) handle compact du catalogue runtime pour cet artefact.
//! ```
//!
//! `ArtifactKey` n'est PAS `component_id` : un artefact peut exister sans
//! composant Forge (cas `pages_homepage`) et, conceptuellement, un composant
//! peut produire plusieurs artefacts. `SourceKey` n'est pas une identité
//! persistante : c'est la POSITION de l'artefact dans le catalogue
//! (`ARTIFACTS`) généré par le build. Son numéro peut changer entre deux
//! builds ; aucun mécanisme de stabilisation n'est introduit.
//!
//! ## Deux relations distinctes, une jonction
//!
//! ```text
//! RouteSpec     route    → artefact + sélection     (exposition)
//! ArtifactSpec  artefact → composant producteur     (publication)
//!                     jonction : ArtifactKey
//! ```
//!
//! Les deux sont déclarées dans le même manifeste de build
//! (`crates/core/schema/publication.toml`) sans être fusionnées
//! sémantiquement. Les instances de ces types sont GÉNÉRÉES par
//! `crates/core/schema/build/` — jamais écrites à la main.

use crate::SourceKey;

/// Identité logique d'un artefact publiable (ex. `"content_core"`).
///
/// Enveloppe volontairement opaque d'un `&'static str` : construite par le
/// code généré, comparée et convertie en clé de résolution runtime par les
/// couches aval.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct ArtifactKey(&'static str);

impl ArtifactKey {
    /// Construction `const` — le code généré en dépend pour ses `const`/`static`.
    pub const fn new(key: &'static str) -> Self {
        ArtifactKey(key)
    }

    /// Forme textuelle de la clé — celle que le runtime utilise pour
    /// résoudre la génération publiée de l'artefact.
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

/// Un artefact publiable : sa clé, et le composant Forge qui le produit s'il
/// en a un. `component == None` est un cas légitime (artefact sans composant).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArtifactSpec {
    pub key: ArtifactKey,
    pub component: Option<&'static str>,
}

/// Mode de sélection de l'enregistrement servi par une route.
///
/// `PrimaryKey` : la valeur du paramètre HTTP EST la clé primaire du
/// composant producteur de l'artefact. `column` est la colonne SQL de cette
/// PK telle que résolue par la Forge à la compilation — elle n'a AUCUN
/// rapport de nom avec le paramètre HTTP (`id` côté URL, `document_id` côté
/// SQL pour `content.core`), et ne doit pas être renommée pour coïncider.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RouteSelection {
    PrimaryKey { column: &'static str },
}

/// Déclaration neutre d'une route : quel artefact, sélectionné comment,
/// exposé sous quel motif et via quel paramètre.
///
/// Ne porte ni politique de parsing HTTP, ni code de statut, ni type de
/// contenu : ces éléments appartiennent à la représentation serveur.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RouteSpec {
    /// Identifiant Rust-safe de la route (nom des constantes générées).
    pub name: &'static str,
    /// Motif d'URL, syntaxe `{param}`.
    pub pattern: &'static str,
    /// Artefact servi.
    pub artifact: ArtifactKey,
    /// Nom du paramètre HTTP (partie de `pattern`).
    pub parameter: &'static str,
    /// Sélection de l'enregistrement dans l'artefact.
    pub selection: RouteSelection,
}

const _: () = assert!(
    !std::mem::needs_drop::<RouteSpec>(),
    "RouteSpec ne doit jamais nécessiter de Drop — donnée AOT pure"
);
const _: () = assert!(
    !std::mem::needs_drop::<ArtifactSpec>(),
    "ArtifactSpec ne doit jamais nécessiter de Drop — donnée AOT pure"
);

/// Artefact désigné par un `SourceKey` dans un catalogue de build.
///
/// Règle unique de numérotation : `SourceKey(n)` désigne l'entrée `n` du
/// catalogue. Un numéro hors catalogue n'a pas d'artefact (`None`).
pub fn artifact_for_source(
    catalog: &'static [ArtifactSpec],
    key: SourceKey,
) -> Option<&'static ArtifactSpec> {
    catalog.get(usize::from(key.0))
}

/// `SourceKey` d'un artefact dans un catalogue de build — inverse de
/// [`artifact_for_source`]. `None` si l'artefact n'est pas au catalogue ou si
/// sa position ne tient pas dans un `u16`.
pub fn source_key_for_artifact(catalog: &[ArtifactSpec], key: ArtifactKey) -> Option<SourceKey> {
    let index = catalog.iter().position(|a| a.key == key)?;
    u16::try_from(index).ok().map(SourceKey)
}

#[cfg(test)]
mod tests {
    use super::*;

    static CATALOG: &[ArtifactSpec] = &[
        ArtifactSpec {
            key: ArtifactKey::new("alpha"),
            component: Some("s.alpha"),
        },
        ArtifactSpec {
            key: ArtifactKey::new("beta"),
            component: None,
        },
    ];

    #[test]
    fn artifact_key_round_trips_its_text() {
        const KEY: ArtifactKey = ArtifactKey::new("content_core");
        assert_eq!(KEY.as_str(), "content_core");
    }

    #[test]
    fn source_key_is_the_catalog_position() {
        assert_eq!(
            source_key_for_artifact(CATALOG, ArtifactKey::new("alpha")),
            Some(SourceKey(0))
        );
        assert_eq!(
            source_key_for_artifact(CATALOG, ArtifactKey::new("beta")),
            Some(SourceKey(1))
        );
    }

    #[test]
    fn artifact_for_source_inverts_source_key_for_artifact() {
        for artifact in CATALOG {
            let key = source_key_for_artifact(CATALOG, artifact.key).expect("au catalogue");
            assert_eq!(artifact_for_source(CATALOG, key), Some(artifact));
        }
    }

    #[test]
    fn unknown_source_or_artifact_resolves_to_none() {
        assert_eq!(artifact_for_source(CATALOG, SourceKey(2)), None);
        assert_eq!(
            source_key_for_artifact(CATALOG, ArtifactKey::new("gamma")),
            None
        );
    }

    #[test]
    fn artifact_without_component_is_representable() {
        // Cas `pages_homepage` : un artefact peut exister sans composant.
        assert_eq!(CATALOG[1].component, None);
    }
}
