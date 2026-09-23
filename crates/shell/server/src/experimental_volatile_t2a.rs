// crates/shell/server/src/experimental_volatile_t2a.rs

//! PROVISOIRE — EXPÉRIMENTAL — NON NORMATIF — TEST-ONLY (V1c).
//!
//! Démonstration réelle de la chaîne :
//!
//! ```text
//! Static → Volatile → Static
//!   → ResolvedRange[]
//!   → Bytes::from_owner
//!   → Body
//!   → HTTP
//! ```
//!
//! avec un producteur Volatile **injecté** (contrat Volatile V1c,
//! handoff-volatile-vertical-slice.md, NOTE-contrat-volatile-v1.md).
//!
//! ## Portée volontairement distincte d'`experimental_t2a.rs`
//!
//! Ce module ne touche ni ne réutilise `experimental_t2a.rs` (K=1/K=3
//! purement statique, I1→I6, non modifié) : clé de packfile propre
//! (`VOLATILE_FIXTURE_PACKFILE_KEY`, jamais `"content_core"`), route
//! distincte, fixture producteur distincte. Aucune collision possible entre
//! les deux suites de tests, y compris sous parallélisme `cargo test`.
//!
//! ## Non monté en production (`main()`)
//!
//! Contrairement à `experimental_t2a::mount_experimental` (mergé dans
//! `main()`, réutilisant un packfile déjà provisionné par `ROUTE_TABLE`),
//! ce module reste **`#[cfg(test)]`** de bout en bout : monter une route
//! réelle ici exigerait de provisionner/cold-start un nouvel artefact
//! (`VOLATILE_FIXTURE_PACKFILE_KEY`) au démarrage — c'est-à-dire toucher au
//! bootstrap de production pour un artefact qui n'existe encore que comme
//! fixture. Le contrat V1c (§5) exclut explicitement « production réelle » :
//! ce module démontre la chaîne via un vrai `TcpListener`/`reqwest`, dans
//! un test, jamais via le binaire servi. Voir le rapport de session pour
//! cette décision — signalée, pas silencieuse.
//!
//! ## Fixture K=3
//!
//! ```text
//! segment 0 = StaticArtifact (packfile réel, id=1)   — prefix
//! segment 1 = VolatileSlot   (producteur injecté)     — volatile
//! segment 2 = StaticArtifact (packfile réel, id=2)   — suffix
//! ```
//!
//! Ordre des segments = ordre d'émission (ADR-011 §5) : le volatile n'est
//! jamais déplacé en fin de réponse (contrat V1c §1).
//!
//! ## Producteur
//!
//! `std::sync::RwLock<Vec<u8>>` global à ce module, muté uniquement par les
//! tests (`set_volatile_fixture_payload`) — une fixture injectable au sens
//! de la contrainte 6 du contrat V1c, pas un catalogue `ProducerKey →
//! implémentation` réel (V3 : SQL/`account_core`, hors périmètre).

use std::pin::Pin;
use std::sync::{Arc, OnceLock, RwLock};
use std::task::{Context as TaskContext, Poll};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use futures_core::Stream;

use marius_projection::{
    EmissionBackendKind, ProducerKey, RouteDescriptor, SegmentDescriptor, SegmentFlags,
    SegmentSelection, SourceId, SourceKey, SourceSpec,
};
use marius_render::{
    LiveRegistry, MaterializedSource, PackHtmlIndex, SourceResolutionContext, VolatileStorage,
    resolve_generation, resolve_range, resolve_volatile_generation, resolve_volatile_range,
    source_spec_for,
};

/// Clé de packfile propre à ce module — jamais `"content_core"`
/// (`experimental_t2a.rs`). `pub(crate)` : réutilisée par la fixture de
/// test dans `main.rs` (écriture du packfile synthétique avant démarrage
/// du serveur de test).
pub(crate) const VOLATILE_FIXTURE_PACKFILE_KEY: &str = "content_core_volatile_fixture";

/// Borne AOT du segment `Volatile` de la fixture — volontairement petite
/// (256 octets) : assez pour les payloads de test « normaux », assez
/// petite pour que le test d'overflow (contrat V1c, test E) reste lisible
/// sans construire un payload énorme.
const VOLATILE_CAPACITY: u32 = 256;

static SOURCES: &[SourceSpec] = &[
    SourceSpec::StaticArtifact { key: SourceKey(0) },
    SourceSpec::VolatileSlot {
        capacity: VOLATILE_CAPACITY,
        producer: ProducerKey(0),
    },
];

/// K=3 : prefix (statique) → volatile → suffix (statique). L'ordre de ce
/// slice EST l'ordre d'émission — ne jamais le réordonner pour « simplifier »
/// la boucle de résolution (contrat V1c §1 : ne pas déplacer le volatile en
/// fin de réponse).
static SEGMENTS: &[SegmentDescriptor] = &[
    SegmentDescriptor {
        source: SourceId(0),
        selection: SegmentSelection::Constant(1), // prefix, id=1
        flags: SegmentFlags::NONE,
    },
    SegmentDescriptor {
        source: SourceId(1),
        selection: SegmentSelection::NotApplicable, // P7 : jamais Constant(0)
        flags: SegmentFlags::VOLATILE,
    },
    SegmentDescriptor {
        source: SourceId(0),
        selection: SegmentSelection::Constant(2), // suffix, id=2
        flags: SegmentFlags::NONE,
    },
];

/// `volatile_capacity` = somme des capacités des segments `Volatile` de la
/// route (RouteDescriptor, doc de champ) — ici un seul segment volatile,
/// donc égale à `VOLATILE_CAPACITY` exactement.
static ROUTE: RouteDescriptor = RouteDescriptor {
    segments: SEGMENTS,
    sources: SOURCES,
    backend_kind: EmissionBackendKind::Scatter,
    volatile_capacity: VOLATILE_CAPACITY,
};

// ─── MmapOwner — identique en substance à experimental_t2a.rs, dupliqué
//     localement (type privé là-bas, même discipline que sa propre
//     duplication de write_fixture_packfile) ──────────────────────────────

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

// ─── VolatileOwner — pont de durée de vie pour Bytes::from_owner (P1/P3/P4)

/// Clone d'un `Arc<VolatileStorage>` déjà détenu par
/// `MaterializedSource::Volatile` (incrément atomique, jamais une nouvelle
/// production) — même rôle que `MmapOwner` pour le chemin statique, mais
/// pour un stockage possédé plutôt qu'un mapping. `Bytes::from_owner`
/// conserve ce owner vivant jusqu'à ce que tous les `Bytes`/`Body` qui en
/// dérivent soient droppés (P4) — c'est cette garantie de la crate `bytes`,
/// pas un mécanisme ajouté ici, qui tient P4/P5 en aval de cette frontière.
struct VolatileOwner {
    storage: Arc<VolatileStorage>,
}

impl AsRef<[u8]> for VolatileOwner {
    fn as_ref(&self) -> &[u8] {
        self.storage.as_slice()
    }
}

// ─── FrameStream — identique à experimental_t2a.rs, dupliqué localement
//     (type privé là-bas) ───────────────────────────────────────────────

struct FrameStream {
    frames: std::vec::IntoIter<Bytes>,
}

impl Stream for FrameStream {
    type Item = Result<Bytes, std::convert::Infallible>;

    fn poll_next(self: Pin<&mut Self>, _cx: &mut TaskContext<'_>) -> Poll<Option<Self::Item>> {
        Poll::Ready(self.get_mut().frames.next().map(Ok))
    }
}

// ─── Producteur volatile injectable (fixture V1c — pas un catalogue réel) ──

fn volatile_fixture_state() -> &'static RwLock<Vec<u8>> {
    static STATE: OnceLock<RwLock<Vec<u8>>> = OnceLock::new();
    STATE.get_or_init(|| RwLock::new(b"Alice".to_vec()))
}

/// `produce` injecté dans `resolve_volatile_generation` — ignore
/// délibérément la `ProducerKey` reçue (fixture à un seul producteur, même
/// discipline que le stub `"content_core"` d'`experimental_t2a.rs` qui
/// ignore la `SourceKey` reçue) : aucun catalogue `ProducerKey →
/// implémentation` n'existe avant V3.
fn volatile_fixture_produce(_producer: ProducerKey) -> Vec<u8> {
    volatile_fixture_state()
        .read()
        .expect("[experimental_volatile_t2a] verrou empoisonné (lecture fixture)")
        .clone()
}

/// Mute la fixture — jamais appelé par le handler HTTP lui-même, seulement
/// par les tests qui simulent un changement de production entre deux
/// résolutions (contrat V1c, tests C/D/G).
pub(crate) fn set_volatile_fixture_payload(payload: Vec<u8>) {
    *volatile_fixture_state()
        .write()
        .expect("[experimental_volatile_t2a] verrou empoisonné (écriture fixture)") = payload;
}

// ─── Résolution ─────────────────────────────────────────────────────────

/// Résout la route mixte Static→Volatile→Static en `Response` — pendant de
/// `resolve_route_to_response` (`experimental_t2a.rs`), étendu au cas
/// `VolatileSlot`. Chaque branche du `match` est exhaustive sur
/// (Source, Sélection) : aucune combinaison incohérente ne tombe dans un
/// comportement de secours implicite (contrat V1c §4, P7) — toute
/// incohérence retourne 500, jamais un panic.
async fn resolve_volatile_route_to_response(
    route: &'static RouteDescriptor,
    registry: Arc<LiveRegistry>,
) -> Response {
    if route.segments.is_empty() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    // Un seul SourceKey distinct référencé par les segments StaticArtifact
    // de cette route (SourceId(0), deux fois) — capacité N=1, même
    // invariant qu'experimental_t2a.rs.
    let mut ctx: SourceResolutionContext<1> = SourceResolutionContext::new();
    let mut frames: Vec<Bytes> = Vec::with_capacity(route.segments.len());

    for segment in route.segments {
        let Some(spec) = source_spec_for(route, segment.source) else {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        };

        match (spec, segment.selection) {
            // ── chemin statique — inchangé par rapport à experimental_t2a.rs
            (SourceSpec::StaticArtifact { key }, SegmentSelection::Constant(id)) => {
                if ctx.get(*key).is_none() {
                    let Some(source) = resolve_generation(spec, |_key| {
                        registry.load(VOLATILE_FIXTURE_PACKFILE_KEY)
                    }) else {
                        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                    };
                    if !ctx.insert(*key, source) {
                        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                    }
                }
                let source = ctx
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
                debug_assert_eq!(
                    len as usize,
                    range.len(),
                    "resolve_range et lookup doivent s'accorder sur len pour le même id"
                );

                frames.push(Bytes::from_owner(MmapOwner {
                    handle: Arc::clone(handle),
                    offset,
                    len,
                }));
            }

            // ── chemin volatile — P5 : production directe, jamais un
            //    lookup par sélection. Le producteur est appelé et son
            //    résultat entièrement possédé (VolatileStorage) avant que
            //    la boucle ne reprenne vers le segment statique suivant —
            //    aucun emprunt (ResolvedRange d'un segment précédent) n'est
            //    en vol pendant cet appel.
            (SourceSpec::VolatileSlot { .. }, SegmentSelection::NotApplicable) => {
                match resolve_volatile_generation(spec, volatile_fixture_produce) {
                    Some(Ok(source)) => {
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
                    // P2 : effective_len > capacity — erreur contrôlée,
                    // jamais de troncature, jamais de panic
                    // (NOTE-contrat-volatile-v1.md).
                    Some(Err(_capacity_exceeded)) => {
                        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                    }
                    // spec n'est pas VolatileSlot : ne peut pas survenir
                    // dans cette branche (déjà filtrée par le match
                    // ci-dessus) — conservé pour l'exhaustivité de
                    // resolve_volatile_generation, jamais un panic.
                    None => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
                }
            }

            // ── P7 : toute autre combinaison (Volatile+Constant,
            //    Static+NotApplicable...) est une incohérence de contrat —
            //    jamais devinée, jamais de secours implicite.
            _ => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        }
    }

    // Longueur totale connue avant construction du Body — somme des
    // longueurs déjà résolues (statiques ET volatile), jamais la capacité
    // maximale du segment volatile (contrat V1c, test B).
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

// ─── Handler / montage — test-only (cf. en-tête de module) ─────────────────

async fn serve_t2a_experimental_volatile(State(registry): State<Arc<LiveRegistry>>) -> Response {
    resolve_volatile_route_to_response(&ROUTE, registry).await
}

/// `pub(crate)` — appelé uniquement depuis le module de tests de
/// `main.rs` (jamais depuis `main()` lui-même, cf. en-tête de module).
pub(crate) fn mount_experimental_volatile(registry: Arc<LiveRegistry>) -> Router {
    Router::new()
        .route(
            "/__experimental/t2a/volatile",
            get(serve_t2a_experimental_volatile),
        )
        .with_state(registry)
}

// =============================================================================
// Tests unitaires — D (drop observable) et F (absence de copie)
//
// Portée volontairement non-HTTP : les deux propriétés démontrées ici sont
// garanties par le type (VolatileStorage/Arc) ou par le contrat documenté
// de `Bytes::from_owner` (bytes crate) — pas par un comportement observable
// uniquement au travers d'un aller-retour réseau. Les tests HTTP (A/B/C/E/G)
// vivent dans main.rs (harnais TcpListener/reqwest déjà en place).
// =============================================================================
#[cfg(test)]
mod tests {
    use super::*;

    /// D — drop observable : le owner reste vivant tant qu'un `Bytes`
    /// construit dessus existe, et se libère une fois ce `Bytes` (et ses
    /// clones) droppés. Démontre exactement la propriété nécessaire
    /// (P4 : « le owner passé à Bytes::from_owner doit conserver le
    /// VolatileStorage vivant jusqu'à ce que les octets ne puissent plus
    /// être lus »), sans imposer de moment de drop que l'API ne garantit
    /// pas (pas d'assertion sur un compteur d'allocations global, pas sur
    /// une temporalité tokio).
    #[test]
    fn volatile_owner_keeps_storage_alive_while_bytes_exist_and_releases_after_drop() {
        let storage = Arc::new(
            VolatileStorage::from_produced(b"drop-observable".to_vec(), 64)
                .expect("64 >= 16 : la production doit réussir"),
        );
        assert_eq!(Arc::strong_count(&storage), 1);

        let bytes = Bytes::from_owner(VolatileOwner {
            storage: Arc::clone(&storage),
        });
        // Body vivant → storage vivant : le clone détenu par VolatileOwner
        // (maintenant possédé par `bytes`) porte le compte à 2.
        assert_eq!(Arc::strong_count(&storage), 2);
        assert_eq!(&bytes[..], b"drop-observable");

        drop(bytes);
        // Body consommé/abandonné → owner libéré : retour à 1 (notre seul
        // Arc restant).
        assert_eq!(Arc::strong_count(&storage), 1);
    }

    /// F — absence de copie : égalité de POINTEUR entre ce que
    /// `VolatileStorage::as_slice()` expose et ce que `VolatileOwner`
    /// remet effectivement à `Bytes::from_owner` (son propre `as_ref()`) —
    /// même méthode que l'invariant I5 d'`experimental_t2a.rs`
    /// (`owner_as_ref_matches_resolved_range_pointer_no_copy`). Ne
    /// compare PAS le pointeur interne du `Bytes` construit : ce détail
    /// n'est pas un engagement contractuel de la crate `bytes` (mise en
    /// garde du contrat V1c, test F) — seule la frontière que ce module
    /// contrôle (VolatileStorage → VolatileOwner) est vérifiée.
    #[test]
    fn volatile_owner_as_ref_matches_resolved_range_pointer_no_copy() {
        let spec = SourceSpec::VolatileSlot {
            capacity: 64,
            producer: ProducerKey(0),
        };
        let source = resolve_volatile_generation(&spec, |_| b"pas de copie ici".to_vec())
            .expect("VolatileSlot doit produire Some(..)")
            .expect("17 <= 64 : la production doit réussir");

        let range = resolve_volatile_range(&source).expect("Volatile doit se résoudre");

        let MaterializedSource::Volatile { storage } = &source else {
            panic!("attendu Volatile");
        };
        let owner = VolatileOwner {
            storage: Arc::clone(storage),
        };

        assert_eq!(
            owner.as_ref().as_ptr(),
            range.ptr(),
            "VolatileOwner::as_ref() doit pointer exactement dans le buffer \
             de VolatileStorage — aucune recopie entre les deux"
        );
        assert_eq!(owner.as_ref().len(), range.len());
        assert_eq!(owner.as_ref(), b"pas de copie ici");
    }
}
