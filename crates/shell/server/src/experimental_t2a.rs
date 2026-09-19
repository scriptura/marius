// crates/shell/server/src/experimental_t2a.rs

//! PROVISOIRE — EXPÉRIMENTAL — NON NORMATIF.
//!
//! Premier raccordement réel de la frontière transport T2A
//! (SPECIFICATION-transport-segmente-t2a.md v2) dans le chemin HTTP. Le
//! `RouteDescriptor` ci-dessous est un fixture statique écrit à la main,
//! PAS une sortie de la Forge — ne préjuge d'aucun mécanisme Forge futur,
//! d'aucun catalogue SourceKey→packfile_key générique, d'aucun format de
//! configuration de routes.
//!
//! Route isolée, montée hors ROUTE_TABLE/RouteEntry/IdSource (main.rs) —
//! ni `build_router`, ni `handlers::serve_route`, ni le chemin monolithique
//! existant ne sont touchés ni consultés par ce module.
//!
//! Incrément I1 : un segment, une source réelle (packfile_key
//! "content_core", id=1). Chaîne exercée :
//! SourceSpec → resolve_generation → MaterializedSource::Mmap
//!   → resolve_range → ResolvedRange (vérifié)
//!   → MmapOwner (Arc<PackHtmlIndex> cloné + offset/len)
//!   → Bytes::from_owner → Body::from → Response → Hyper (Phase 5, inchangé).

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;

use marius_projection::{
    EmissionBackendKind, RouteDescriptor, SegmentDescriptor, SegmentFlags, SegmentSelection,
    SourceId, SourceKey, SourceSpec,
};
use marius_render::{
    LiveRegistry, MaterializedSource, PackHtmlIndex, resolve_generation, resolve_range,
    source_spec_for,
};

// ─── Fixture statique — un seul segment, une seule source ──────────────────

static EXPERIMENTAL_SOURCES: &[SourceSpec] = &[SourceSpec::StaticArtifact { key: SourceKey(0) }];

static EXPERIMENTAL_SEGMENTS: &[SegmentDescriptor] = &[SegmentDescriptor {
    source: SourceId(0),
    selection: SegmentSelection::Constant(1),
    flags: SegmentFlags::NONE,
}];

/// `backend_kind` n'est consommé par aucun code de ce module (T2A ne
/// bifurque jamais dessus — sendfile/SingleFile reste hors périmètre de
/// cette phase). Scatter est la valeur honnête : rien ici ne prouve une
/// compatibilité SingleFile (une seule route, mais l'IR ne certifie rien
/// au-delà de ce que `is_single_file_compatible` établirait explicitement,
/// jamais appelé ici).
static EXPERIMENTAL_ROUTE: RouteDescriptor = RouteDescriptor {
    segments: EXPERIMENTAL_SEGMENTS,
    sources: EXPERIMENTAL_SOURCES,
    backend_kind: EmissionBackendKind::Scatter,
    volatile_capacity: 0,
};

// ─── MmapOwner — pont de durée de vie pour Bytes::from_owner ───────────────

/// Local à ce module — pas une primitive Marius, pas ajouté à emission.rs.
/// Clone d'un `Arc<PackHtmlIndex>` déjà détenu par `MaterializedSource::Mmap`
/// (incrément atomique, jamais un nouvel `open()`/`mmap`) + une plage déjà
/// validée par un `resolve_range` réussi sur ce même `Arc`.
struct MmapOwner {
    handle: Arc<PackHtmlIndex>,
    offset: u64,
    len: u32,
}

impl AsRef<[u8]> for MmapOwner {
    fn as_ref(&self) -> &[u8] {
        // Sûr : (offset, len) proviennent d'un resolve_range() déjà réussi
        // sur ce même Arc dans serve_t2a_experimental, jamais forgés
        // indépendamment — invariant local à ce module, pas une garantie
        // générale de blob().
        self.handle
            .blob(self.offset, self.len)
            .expect("MmapOwner: (offset, len) validés par resolve_range sur ce même Arc")
    }
}

// ─── Handler ─────────────────────────────────────────────────────────────

async fn serve_t2a_experimental(State(registry): State<Arc<LiveRegistry>>) -> Response {
    let route = &EXPERIMENTAL_ROUTE;

    let Some(segment) = route.segments.first() else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };

    let Some(spec) = source_spec_for(route, segment.source) else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };

    // Stub de catalogue local : la fermeture ignore la SourceKey reçue et
    // capture directement le packfile_key réel — pas de mapping générique
    // SourceKey→packfile_key introduit (Strategy v2 §3).
    let Some(source) = resolve_generation(spec, |_key| registry.load("content_core")) else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };

    let SegmentSelection::Constant(id) = segment.selection else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };

    let Some(range) = resolve_range(&source, id) else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let MaterializedSource::Mmap { handle } = &source else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };

    // ResolvedRange n'expose que (ptr, len), jamais offset (DESIGN §3.2) —
    // un second lookup() (même mapping, même O(log N), aucune I/O, aucune
    // copie) est la seule voie publique pour reconstituer l'offset
    // nécessaire à MmapOwner.
    let Some((offset, len)) = handle.lookup(id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    debug_assert_eq!(
        len as usize,
        range.len(),
        "resolve_range et lookup doivent s'accorder sur len pour le même id"
    );

    let owner = MmapOwner {
        handle: Arc::clone(handle),
        offset,
        len,
    };
    let bytes = Bytes::from_owner(owner);
    let content_length = bytes.len() as u64;

    (
        [
            (header::CONTENT_LENGTH, HeaderValue::from(content_length)),
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            ),
        ],
        Body::from(bytes),
    )
        .into_response()
}

/// Monte la route expérimentale — hors ROUTE_TABLE, jamais consultée par
/// build_router()/serve_route(). Appelé depuis main() en plus de (jamais à
/// la place de) build_router().
pub(crate) fn mount_experimental(registry: Arc<LiveRegistry>) -> Router {
    Router::new()
        .route("/__experimental/t2a", get(serve_t2a_experimental))
        .with_state(registry)
}
