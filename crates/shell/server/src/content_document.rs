// crates/shell/server/src/content_document.rs

//! V3b — montage réel de `/content/{id}` sur le pipeline runtime T2A.
//!
//! ```text
//! HTTP request
//!     ↓
//! marius_schema::ROUTE_DESCRIPTORS["content_document"]  (K=3, généré, V2c/V2d)
//!     ↓
//! Source 0 : StaticArtifact(content_core_head)   RequestSlot(0)
//! Source 1 : VolatileSlot(nav_profile)            NotApplicable, ProducerKey(0)
//! Source 2 : StaticArtifact(content_core_tail)   RequestSlot(0)
//!     ↓
//! resolve_generation/resolve_range (statique) · materialize_volatile (V3a)
//!     ↓
//! Bytes::from_owner (MmapOwner / VolatileOwner) → Body → HTTP
//! ```
//!
//! Remplace, pour cette seule route, le montage `ROUTE_TABLE`/`serve_route`
//! historique (voir `main.rs` : `content_core` retiré de `ROUTE_TABLE`,
//! toujours servi pour comparaison à `/__monolithic/content/{id}`, même
//! `handlers::serve_route`, jamais une seconde implémentation de lecture).
//! « Le reste des routes » (`pages_homepage`) n'est pas concerné.
//!
//! Aucune dépendance inverse `marius-render` → Axum/Hyper : ce module vit
//! dans `marius-server`, appelle `marius-render` en lecture seule
//! (`resolve_generation`/`resolve_range`/`materialize_volatile`/
//! `resolve_volatile_range`), jamais l'inverse.
//!
//! ## Contexte applicatif (V3b)
//!
//! Expérimental et déterministe, comme convenu : un paramètre de requête
//! `?user=...`, jamais une authentification réelle (hors périmètre). Le
//! chemin reste néanmoins réel — requête HTTP → paramètre → `VolatileContext`
//! → `materialize_volatile` — jamais un username injecté au moment de
//! construire le `Body`.
//!
//! ## Erreurs
//!
//! Toute incohérence (segment/source, capacité dépassée, producteur
//! inconnu, id absent du static) devient une réponse HTTP contrôlée — jamais
//! un `unwrap`/`expect` sur ce chemin.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context as TaskContext, Poll};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use futures_core::Stream;

use marius_projection::publication::artifact_for_source;
use marius_projection::{SegmentSelection, SourceKey, SourceSpec};
use marius_render::{
    LiveRegistry, MaterializedSource, PackHtmlIndex, SourceResolutionContext, VolatileContext,
    materialize_volatile, resolve_generation, resolve_range, resolve_volatile_range,
    source_spec_for,
};
use marius_schema::{ARTIFACTS, ROUTE_DESCRIPTORS, ROUTES};

// ─── Owners — mêmes patrons qu'experimental_volatile_t2a.rs (I1→I6, V1c),
//     dupliqués localement (types privés là-bas) ──────────────────────────

struct MmapOwner {
    handle: Arc<PackHtmlIndex>,
    offset: u64,
    len: u32,
}

impl AsRef<[u8]> for MmapOwner {
    fn as_ref(&self) -> &[u8] {
        self.handle
            .blob(self.offset, self.len)
            .expect("MmapOwner: (offset, len) validés par resolve_range sur ce même Arc")
    }
}

struct VolatileOwner {
    storage: Arc<marius_render::VolatileStorage>,
}

impl AsRef<[u8]> for VolatileOwner {
    fn as_ref(&self) -> &[u8] {
        self.storage.as_slice()
    }
}

struct FrameStream {
    frames: std::vec::IntoIter<Bytes>,
}

impl Stream for FrameStream {
    type Item = Result<Bytes, std::convert::Infallible>;

    fn poll_next(self: Pin<&mut Self>, _cx: &mut TaskContext<'_>) -> Poll<Option<Self::Item>> {
        Poll::Ready(self.get_mut().frames.next().map(Ok))
    }
}

/// `SourceKey` (position dans `ARTIFACTS`) → clé de packfile — résolution
/// réelle via le catalogue généré (`marius_projection::publication`,
/// écrite en V1a, jamais câblée jusqu'ici : la fixture T2A expérimentale
/// n'avait qu'un seul artefact, un stub `"content_core"` en dur lui
/// suffisait. Cette route en a deux (head/tail) : plus possible de deviner,
/// la résolution réelle devient nécessaire.
fn resolve_static_packfile_key(key: SourceKey) -> Option<&'static str> {
    artifact_for_source(ARTIFACTS, key).map(|artifact| artifact.key.as_str())
}

async fn serve_content_document(
    Path(path_params): Path<HashMap<String, String>>,
    Query(query): Query<HashMap<String, String>>,
    State(registry): State<Arc<LiveRegistry>>,
) -> Response {
    // Slot 0 : l'unique paramètre HTTP de cette route, partagé par les deux
    // segments statiques (RequestSlot(0), même sélection, deux artefacts
    // distincts — contrat P7, forme verrouillée en V2c §8).
    let Some(id) = path_params.get("id").and_then(|s| s.parse::<i64>().ok()) else {
        return StatusCode::BAD_REQUEST.into_response();
    };

    let Some(route_index) = ROUTES.iter().position(|r| r.name == "content_document") else {
        // Doit toujours exister : généré par la Forge depuis publication.toml.
        // Son absence est un bug de build, jamais une route inconnue côté
        // client.
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let route = &ROUTE_DESCRIPTORS[route_index];

    // Contexte applicatif — expérimental et déterministe (V3b) : un
    // paramètre de requête, jamais une authentification réelle. Lu ici,
    // jamais recalculé plus loin : un seul `VolatileContext`, possédé, pour
    // toute la durée de la résolution des segments ci-dessous.
    let ctx = VolatileContext {
        username: query.get("user").cloned(),
    };

    // Un seul `SourceKey` statique distinct référencé deux fois
    // (content_core_head, content_core_tail : deux `SourceKey` *positions*
    // différentes en réalité — N=2 ici, jamais mutualisées entre elles,
    // seulement dédupliquées si la MÊME position revenait dans une future
    // route).
    let mut static_ctx: SourceResolutionContext<2> = SourceResolutionContext::new();
    let mut frames: Vec<Bytes> = Vec::with_capacity(route.segments.len());

    for segment in route.segments {
        let Some(spec) = source_spec_for(route, segment.source) else {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        };

        match (spec, segment.selection) {
            // ── segment statique (head ou tail) ─────────────────────────
            (SourceSpec::StaticArtifact { key }, SegmentSelection::RequestSlot(_)) => {
                if static_ctx.get(*key).is_none() {
                    let Some(source) = resolve_generation(spec, |source_key| {
                        resolve_static_packfile_key(source_key)
                            .and_then(|packfile_key| registry.load(packfile_key))
                    }) else {
                        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                    };
                    if !static_ctx.insert(*key, source) {
                        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                    }
                }
                let source = static_ctx
                    .get(*key)
                    .expect("clé insérée juste au-dessus, ou déjà présente");

                let Some(range) = resolve_range(source, id) else {
                    return StatusCode::NOT_FOUND.into_response();
                };
                let MaterializedSource::Mmap { handle } = source else {
                    return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                };
                let Some((offset, len)) = handle.lookup(id) else {
                    return StatusCode::NOT_FOUND.into_response();
                };
                debug_assert_eq!(len as usize, range.len());

                frames.push(Bytes::from_owner(MmapOwner {
                    handle: Arc::clone(handle),
                    offset,
                    len,
                }));
            }

            // ── segment volatile (nav_profile) — P5 : production directe,
            //    entièrement avant que la boucle ne reprenne. Aucune erreur
            //    (producteur inconnu, dépassement de capacité, source non
            //    volatile) ne panique — toutes deviennent un 500 contrôlé.
            (SourceSpec::VolatileSlot { .. }, SegmentSelection::NotApplicable) => {
                match materialize_volatile(spec, &ctx) {
                    Ok(source) => {
                        let Some(range) = resolve_volatile_range(&source) else {
                            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                        };
                        let MaterializedSource::Volatile { storage } = &source else {
                            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                        };
                        debug_assert_eq!(range.len(), storage.effective_len());

                        frames.push(Bytes::from_owner(VolatileOwner {
                            storage: Arc::clone(storage),
                        }));
                    }
                    // Dépassement de capacité, producteur inconnu, source
                    // mal formée — erreur contrôlée (contrat P2/P7), jamais
                    // un panic ni une troncature.
                    Err(_production_error) => {
                        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                    }
                }
            }

            // Toute autre combinaison (incohérence P7) : jamais devinée,
            // jamais de secours implicite. Garde runtime explicite :
            // `segment_matches_source` reste le prédicat pur de cohérence
            // (garde-fou testé), pas un appel de ce handler.
            _ => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        }
    }

    // Longueur totale connue avant construction du Body — somme des
    // longueurs réellement résolues (statiques ET volatile), jamais la
    // capacité maximale du segment volatile.
    let content_length: u64 = frames.iter().map(|b| b.len() as u64).sum();

    let body_stream = FrameStream {
        frames: frames.into_iter(),
    };

    (
        [
            (header::CONTENT_LENGTH, HeaderValue::from(content_length)),
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            ),
        ],
        Body::from_stream(body_stream),
    )
        .into_response()
}

/// Monte `/content/{id}` sur le pipeline runtime T2A — état résolu
/// localement (même patron que `experimental_t2a::mount_experimental`) :
/// mergé dans `app`, jamais construit à l'intérieur de `build_router()`.
pub(crate) fn mount(registry: Arc<LiveRegistry>) -> Router {
    Router::new()
        .route("/content/{id}", get(serve_content_document))
        .with_state(registry)
}
