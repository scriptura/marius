// crates/core/schema/src/publication_tests.rs

//! Tests de la publication AOT GÉNÉRÉE (`publication.toml` →
//! `generated_schema.rs`) : ce que le build a réellement produit, vérifié
//! contre le catalogue et contre la projection réelle du composant.
//!
//! La logique pure de génération/validation (parsing, erreurs, texte émis)
//! est testée dans `build/publication.rs`, monté sous `cfg(test)` par
//! `lib.rs` (module `publication_build`).

use marius_projection::publication::{artifact_for_source, source_key_for_artifact};
use marius_projection::{
    Projection, RequestValueId, RouteSelection, SegmentFlags, SegmentSelection, SourceId,
    SourceSpec,
};

use crate::{
    ARTIFACTS, CONTENT_CORE_ARTIFACT, CONTENT_CORE_SOURCE_KEY, CONTENT_DOCUMENT_ROUTE,
    ContentCoreProjection, ROUTE_DESCRIPTORS, ROUTES,
};

#[test]
fn content_route_is_declared_over_the_content_core_artifact() {
    let route = CONTENT_DOCUMENT_ROUTE;
    assert_eq!(route.name, "content_document");
    assert_eq!(route.pattern, "/content/{id}");
    assert_eq!(route.artifact, CONTENT_CORE_ARTIFACT);
    assert_eq!(route.parameter, "id");
    assert_eq!(ROUTES, &[route]);
}

#[test]
fn referenced_artifact_exists_and_names_its_producing_component() {
    let artifact = ARTIFACTS
        .iter()
        .find(|a| a.key == CONTENT_CORE_ARTIFACT)
        .expect("l'artefact référencé par la route doit être au catalogue");
    assert_eq!(artifact.key.as_str(), "content_core");
    assert_eq!(artifact.component, Some("content.core"));
}

#[test]
fn every_route_targets_a_catalogued_artifact() {
    for route in ROUTES {
        assert!(
            ARTIFACTS.iter().any(|a| a.key == route.artifact),
            "route «{}» : artefact «{}» absent du catalogue",
            route.name,
            route.artifact.as_str()
        );
    }
}

#[test]
fn artifact_keys_are_unique() {
    for (i, a) in ARTIFACTS.iter().enumerate() {
        assert!(
            ARTIFACTS[i + 1..].iter().all(|b| b.key != a.key),
            "clé d'artefact dupliquée : {}",
            a.key.as_str()
        );
    }
}

/// La sélection `primary_key` porte la colonne SQL résolue par la Forge
/// (`document_id`), distincte du paramètre HTTP (`id`) — et cette colonne est
/// bien celle dont la projection réelle tire `record_id()`.
#[test]
fn primary_key_selection_is_document_id_behind_http_parameter_id() {
    let RouteSelection::PrimaryKey { column } = CONTENT_DOCUMENT_ROUTE.selection;
    assert_eq!(column, "document_id");
    assert_ne!(
        column, CONTENT_DOCUMENT_ROUTE.parameter,
        "colonne SQL et paramètre HTTP sont deux identités distinctes"
    );

    // Preuve contre la projection réelle : `record_id()` lit `document_id`.
    let mut record: <ContentCoreProjection as Projection>::Record = bytemuck::Zeroable::zeroed();
    record.document_id = 42;
    assert_eq!(ContentCoreProjection::record_id(&record), 42);
}

#[test]
fn source_key_resolves_to_the_artifact_and_back() {
    let artifact = artifact_for_source(ARTIFACTS, CONTENT_CORE_SOURCE_KEY)
        .expect("le SourceKey généré doit désigner une entrée du catalogue");
    assert_eq!(artifact.key, CONTENT_CORE_ARTIFACT);
    assert_eq!(
        source_key_for_artifact(ARTIFACTS, CONTENT_CORE_ARTIFACT),
        Some(CONTENT_CORE_SOURCE_KEY)
    );
}

/// `RouteDescriptor` K=1 dérivé de la déclaration : un segment, une source,
/// sélection par le slot 0 (rempli par le paramètre HTTP côté serveur), aucun
/// Volatile.
#[test]
fn route_descriptor_is_k1_and_derives_from_the_route_spec() {
    assert_eq!(ROUTE_DESCRIPTORS.len(), ROUTES.len());

    for (route, descriptor) in ROUTES.iter().zip(ROUTE_DESCRIPTORS) {
        assert_eq!(descriptor.segments.len(), 1, "K=1 : un seul segment");
        assert_eq!(descriptor.sources.len(), 1, "une seule source");
        assert_eq!(descriptor.volatile_capacity, 0);

        let segment = descriptor.segments[0];
        assert_eq!(segment.source, SourceId(0));
        assert_eq!(
            segment.selection,
            SegmentSelection::RequestSlot(RequestValueId(0))
        );
        assert_eq!(segment.flags, SegmentFlags::NONE);
        assert!(!segment.flags.is_volatile());

        // La source du descripteur désigne, via le catalogue, l'artefact de
        // la route.
        let SourceSpec::StaticArtifact { key } = descriptor.sources[0] else {
            panic!("source statique attendue pour la route «{}»", route.name);
        };
        let artifact = artifact_for_source(ARTIFACTS, key)
            .expect("le SourceKey du descripteur doit être au catalogue");
        assert_eq!(artifact.key, route.artifact);
    }
}
