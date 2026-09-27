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
    ARTIFACTS, CONTENT_CORE_ARTIFACT, CONTENT_CORE_HEAD_SOURCE_KEY, CONTENT_CORE_SOURCE_KEY,
    CONTENT_CORE_TAIL_SOURCE_KEY, CONTENT_DOCUMENT_ROUTE, ContentCoreProjection,
    ROUTE_DESCRIPTORS, ROUTES,
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

/// `RouteDescriptor` de `content_document` — K=3 depuis `[[volatile_region]]`
/// (V2c, publication.toml) : StaticArtifact(head) → VolatileSlot →
/// StaticArtifact(tail), head et tail partageant le même `RequestSlot(0)`
/// (même sélection HTTP, deux artefacts distincts — contrat Volatile P7).
///
/// Remplace l'ancien test générique « toute route est K=1 » : `content_document`
/// est actuellement la SEULE route du manifeste réel, et elle est désormais
/// K=3 — un test bouclant sur `ROUTES` en supposant K=1 partout n'a plus de
/// route à couvrir. La forme K=1 elle-même reste vérifiée par
/// `build/publication.rs::route_without_volatile_region_still_generates_k1`
/// (manifeste synthétique, texte généré) — ce fichier-ci teste le manifeste
/// réel, qui n'a plus aucune route purement K=1 à ce jour.
#[test]
fn content_document_route_descriptor_is_k3_for_its_volatile_region() {
    assert_eq!(ROUTE_DESCRIPTORS.len(), ROUTES.len());

    let index = ROUTES
        .iter()
        .position(|r| r.name == "content_document")
        .expect("route content_document déclarée dans le manifeste réel");
    let descriptor = ROUTE_DESCRIPTORS[index];

    assert_eq!(descriptor.segments.len(), 3, "K=3 : trois segments");
    assert_eq!(descriptor.sources.len(), 3, "trois sources");
    assert_eq!(
        descriptor.volatile_capacity, 512,
        "capacity du [[volatile_region]] du manifeste réel"
    );

    // Segment 0 — StaticArtifact(head), RequestSlot(0).
    let s0 = descriptor.segments[0];
    assert_eq!(s0.source, SourceId(0));
    assert_eq!(
        s0.selection,
        SegmentSelection::RequestSlot(RequestValueId(0))
    );
    assert_eq!(s0.flags, SegmentFlags::NONE);
    assert!(!s0.flags.is_volatile());

    // Segment 1 — VolatileSlot, NotApplicable, flag VOLATILE — jamais une
    // sélection simulée (contrat P7).
    let s1 = descriptor.segments[1];
    assert_eq!(s1.source, SourceId(1));
    assert_eq!(s1.selection, SegmentSelection::NotApplicable);
    assert!(s1.flags.is_volatile());

    // Segment 2 — StaticArtifact(tail), même RequestSlot(0) que le head :
    // un seul paramètre HTTP alimente les deux artefacts statiques.
    let s2 = descriptor.segments[2];
    assert_eq!(s2.source, SourceId(2));
    assert_eq!(
        s2.selection,
        SegmentSelection::RequestSlot(RequestValueId(0))
    );
    assert_eq!(s2.flags, SegmentFlags::NONE);

    let SourceSpec::StaticArtifact { key: head_key } = descriptor.sources[0] else {
        panic!("source 0 : StaticArtifact attendu (head)");
    };
    assert_eq!(head_key, CONTENT_CORE_HEAD_SOURCE_KEY);
    assert_eq!(
        artifact_for_source(ARTIFACTS, head_key)
            .expect("head au catalogue")
            .key
            .as_str(),
        "content_core_head"
    );

    match descriptor.sources[1] {
        SourceSpec::VolatileSlot { capacity, .. } => assert_eq!(capacity, 512),
        SourceSpec::StaticArtifact { .. } => panic!("source 1 : VolatileSlot attendu"),
    }

    let SourceSpec::StaticArtifact { key: tail_key } = descriptor.sources[2] else {
        panic!("source 2 : StaticArtifact attendu (tail)");
    };
    assert_eq!(tail_key, CONTENT_CORE_TAIL_SOURCE_KEY);
    assert_eq!(
        artifact_for_source(ARTIFACTS, tail_key)
            .expect("tail au catalogue")
            .key
            .as_str(),
        "content_core_tail"
    );

    // Le monolithique reste au catalogue, intact — non retiré par la
    // segmentation T2A (« le monolithique reste valide »).
    assert!(ARTIFACTS.iter().any(|a| a.key.as_str() == "content_core"));
}
