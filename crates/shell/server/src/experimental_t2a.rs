// crates/shell/server/src/experimental_t2a.rs

//! PROVISOIRE — EXPÉRIMENTAL — NON NORMATIF.
//!
//! Raccordement réel de la frontière transport T2A
//! (SPECIFICATION-transport-segmente-t2a.md v2) dans le chemin HTTP. Les
//! `RouteDescriptor` ci-dessous sont des fixtures statiques écrits à la
//! main, PAS une sortie de la Forge — ne préjugent d'aucun mécanisme Forge
//! futur, d'aucun catalogue SourceKey→packfile_key générique, d'aucun
//! format de configuration de routes.
//!
//! Routes isolées, montées hors ROUTE_TABLE/RouteEntry/IdSource (main.rs) —
//! ni `build_router`, ni `handlers::serve_route`, ni le chemin monolithique
//! existant ne sont touchés ni consultés par ce module.
//!
//! Deux routes, même source réelle partagée (packfile_key "content_core") :
//! - `GET /__experimental/t2a/single` — K=1 (id=1). Régression explicite du
//!   comportement mono-segment d'origine (I1).
//! - `GET /__experimental/t2a` — K=3 (ids 1/2/3), même source partagée par
//!   les trois segments (DESIGN §3 : "N segments" ≠ "N sources") (I3).
//!
//! Les deux routes partagent la même logique de résolution
//! (`resolve_route_to_response`), paramétrée par le `RouteDescriptor`
//! statique — pas une nouvelle abstraction de runtime, une factorisation
//! locale à ce seul module pour éviter de dupliquer la boucle de
//! résolution entre les deux routes.
//!
//! Chaîne exercée par segment, dans l'ordre :
//! SourceSpec → resolve_generation (une fois par SourceKey distinct, via
//!   SourceResolutionContext) → MaterializedSource::Mmap
//!   → resolve_range → ResolvedRange (vérifié)
//!   → MmapOwner (Arc<PackHtmlIndex> cloné + offset/len)
//!   → Bytes::from_owner
//! puis, une fois tous les segments de la route résolus :
//!   Vec<Bytes> (ordre préservé) → Body::from_stream → Response → Hyper
//!   (Phase 5, inchangé).

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context as TaskContext, Poll};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use futures_core::Stream;

use marius_projection::{
    EmissionBackendKind, RouteDescriptor, SegmentDescriptor, SegmentFlags, SegmentSelection,
    SourceId, SourceKey, SourceSpec,
};
use marius_render::{
    LiveRegistry, MaterializedSource, PackHtmlIndex, SourceResolutionContext, resolve_generation,
    resolve_range, source_spec_for,
};

// ─── Fixtures statiques ─────────────────────────────────────────────────

/// Une seule Source, réutilisée par les deux routes ci-dessous.
static EXPERIMENTAL_SOURCES: &[SourceSpec] = &[SourceSpec::StaticArtifact { key: SourceKey(0) }];

/// K=3 — trois segments partageant la même source (id=1). C'est précisément
/// le cas que le DESIGN distingue explicitement (§3 : le nombre de segments
/// d'une réponse n'est jamais égal, par construction, au nombre de sources
/// distinctes qui les alimentent).
static EXPERIMENTAL_SEGMENTS_MULTI: &[SegmentDescriptor] = &[
    SegmentDescriptor {
        source: SourceId(0),
        selection: SegmentSelection::Constant(1),
        flags: SegmentFlags::NONE,
    },
    SegmentDescriptor {
        source: SourceId(0),
        selection: SegmentSelection::Constant(2),
        flags: SegmentFlags::NONE,
    },
    SegmentDescriptor {
        source: SourceId(0),
        selection: SegmentSelection::Constant(3),
        flags: SegmentFlags::NONE,
    },
];

/// K=1 — un seul segment (id=1). Déclaré séparément plutôt que dérivé par
/// slicing de EXPERIMENTAL_SEGMENTS_MULTI : garde la construction `static`
/// triviale à vérifier par lecture, sans dépendre d'une garantie de
/// const-évaluation d'un découpage de slice.
static EXPERIMENTAL_SEGMENTS_SINGLE: &[SegmentDescriptor] = &[SegmentDescriptor {
    source: SourceId(0),
    selection: SegmentSelection::Constant(1),
    flags: SegmentFlags::NONE,
}];

/// `backend_kind` n'est consommé par aucun code de ce module (T2A ne
/// bifurque jamais dessus — sendfile/SingleFile reste hors périmètre de
/// cette phase). Scatter est ici la valeur honnête pour les deux routes :
/// aucune n'est certifiée SingleFile par `is_single_file_compatible`
/// (jamais appelé ici, y compris pour K=1).
static EXPERIMENTAL_ROUTE_MULTI: RouteDescriptor = RouteDescriptor {
    segments: EXPERIMENTAL_SEGMENTS_MULTI,
    sources: EXPERIMENTAL_SOURCES,
    backend_kind: EmissionBackendKind::Scatter,
    volatile_capacity: 0,
};

static EXPERIMENTAL_ROUTE_SINGLE: RouteDescriptor = RouteDescriptor {
    segments: EXPERIMENTAL_SEGMENTS_SINGLE,
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
        // sur ce même Arc dans resolve_route_to_response, jamais forgés
        // indépendamment — invariant local à ce module, pas une garantie
        // générale de blob().
        self.handle
            .blob(self.offset, self.len)
            .expect("MmapOwner: (offset, len) validés par resolve_range sur ce même Arc")
    }
}

// ─── FrameStream — flux minimal pour Body::from_stream ─────────────────────

/// Énumère un `Vec<Bytes>` déjà résolu, un élément par appel à
/// `poll_next` — jamais asynchrone au sens propre (retourne toujours
/// `Poll::Ready` immédiatement), seulement l'habillage minimal exigé par la
/// signature de `Body::from_stream` (borne `S: TryStream`).
///
/// N'utilise PAS `futures_util::stream::iter` : `futures_core::TryStream`
/// (avec son impl générique pour tout `Stream<Item = Result<T, E>>`) est
/// défini directement dans `futures-core`, jamais dans `futures-util` —
/// vérifié sur la documentation de `futures-core`. `axum-core` lui-même ne
/// dépend que de `futures-core` pour ce même besoin, jamais de
/// `futures-util` (vérifié dans Cargo.lock : `futures-util` n'apparaît que
/// dans les dépendances d'`axum`, pas d'`axum-core`). `futures-core` seul
/// (traits purs, sans combinators/sink/io/macros) est donc la dépendance
/// directe strictement nécessaire ici, pas `futures-util`.
struct FrameStream {
    frames: std::vec::IntoIter<Bytes>,
}

impl Stream for FrameStream {
    type Item = Result<Bytes, std::convert::Infallible>;

    fn poll_next(self: Pin<&mut Self>, _cx: &mut TaskContext<'_>) -> Poll<Option<Self::Item>> {
        Poll::Ready(self.get_mut().frames.next().map(Ok))
    }
}

// ─── Résolution partagée ────────────────────────────────────────────────

/// Résout une route T2A expérimentale complète (1 à K segments) en
/// `Response`. Partagée par les deux routes montées ci-dessous — pas une
/// nouvelle abstraction de runtime, une factorisation locale à ce seul
/// module pour éviter de dupliquer la boucle de résolution entre la route
/// K=1 et la route K=3.
async fn resolve_route_to_response(
    route: &'static RouteDescriptor,
    registry: Arc<LiveRegistry>,
) -> Response {
    if route.segments.is_empty() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    // Un seul SourceKey distinct référencé par ces routes (tous les
    // segments partagent SourceId(0)) — capacité N=1, cohérente avec
    // l'invariant "une résolution par SourceKey par requête" (DESIGN §3).
    let mut ctx: SourceResolutionContext<1> = SourceResolutionContext::new();

    // Coût borné par K (le nombre de segments de la route) — accepté
    // explicitement par la SPEC T2A v2 §4/§8 — jamais une copie du
    // payload : chaque Bytes reste une vue sur le mapping existant (cf.
    // MmapOwner ci-dessus).
    let mut frames: Vec<Bytes> = Vec::with_capacity(route.segments.len());

    // Itération séquentielle sur route.segments, dans l'ordre déclaré —
    // c'est cet ordre, et lui seul, qui détermine l'ordre des frames
    // poussées dans `frames`, donc l'ordre d'émission du Body.
    for segment in route.segments {
        let Some(spec) = source_spec_for(route, segment.source) else {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        };
        let SourceSpec::StaticArtifact { key } = spec else {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        };

        if ctx.get(*key).is_none() {
            // Stub de catalogue local : la fermeture ignore la SourceKey
            // reçue et capture directement le packfile_key réel — pas de
            // mapping générique SourceKey→packfile_key introduit
            // (Strategy v2 §3).
            let Some(source) = resolve_generation(spec, |_key| registry.load("content_core"))
            else {
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            };
            if !ctx.insert(*key, source) {
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
        }
        // Invariant : la branche ci-dessus garantit que `key` est
        // désormais résolue dans `ctx` — cet accès ne peut pas échouer.
        let source = ctx
            .get(*key)
            .expect("clé insérée juste au-dessus, ou déjà présente");

        let SegmentSelection::Constant(id) = segment.selection else {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        };

        let Some(range) = resolve_range(source, id) else {
            return StatusCode::NOT_FOUND.into_response();
        };
        let MaterializedSource::Mmap { handle } = source else {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        };

        // ResolvedRange n'expose que (ptr, len), jamais offset (DESIGN
        // §3.2) — un second lookup() (même mapping, même O(log N), aucune
        // I/O, aucune copie) est la seule voie publique pour reconstituer
        // l'offset nécessaire à MmapOwner.
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

    // Longueur totale connue avant construction du Body — somme des
    // longueurs déjà résolues, émise via Content-Length (SPEC T2A v2 §5,
    // hypothèse opérationnelle de cet incrément).
    let content_length: u64 = frames.iter().map(|b| b.len() as u64).sum();

    // Body::from_stream — mécanisme d'implémentation, pas une décision
    // architecturale (SPEC §9). Chaque Bytes reste une frame distincte,
    // jamais recopiée dans un tampon alloué intermédiaire (aucun vecteur
    // d'octets possédé) : c'est précisément ce qui évite de recopier le
    // payload pour fusionner plusieurs plages mmapées en un seul buffer
    // contigu (vrai aussi pour K=1 : une seule frame dans le flux).
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

// ─── Handlers ────────────────────────────────────────────────────────────

async fn serve_t2a_experimental_multi(State(registry): State<Arc<LiveRegistry>>) -> Response {
    resolve_route_to_response(&EXPERIMENTAL_ROUTE_MULTI, registry).await
}

async fn serve_t2a_experimental_single(State(registry): State<Arc<LiveRegistry>>) -> Response {
    resolve_route_to_response(&EXPERIMENTAL_ROUTE_SINGLE, registry).await
}

/// Monte les deux routes expérimentales — hors ROUTE_TABLE, jamais
/// consultées par build_router()/serve_route(). Appelé depuis main() en
/// plus de (jamais à la place de) build_router().
pub(crate) fn mount_experimental(registry: Arc<LiveRegistry>) -> Router {
    Router::new()
        .route("/__experimental/t2a", get(serve_t2a_experimental_multi))
        .route(
            "/__experimental/t2a/single",
            get(serve_t2a_experimental_single),
        )
        .with_state(registry)
}

// =============================================================================
// Tests — I5 : audit non-copie (T3, SPEC T2A v2 §7)
//
// Portée volontairement limitée à ce qui est défini par la stratégie
// initiale/Strategy v2 pour I5 : preuve empirique de non-copie par égalité
// de pointeur, plus un audit textuel du fichier lui-même (absence de lecture positionnelle système, absence de tampon intermédiaire possédé).
// Aucune mesure d'allocations en trois couches n'est instrumentée ici — la
// SPEC/le handoff la qualifient d'optionnelle ("éventuellement"), et un
// compteur d'allocations global affecterait tout le binaire de test
// marius-server (sqlx/tokio compris), pas seulement ce module : une
// extension disproportionnée pour cet incrément, pas une nécessité
// démontrée. La séparation en trois couches (Marius/Core, Bytes/Body,
// Hyper) reste donc qualitative ici, pas mesurée — voir le rapport de
// session pour la distinction exacte entre ce qui est démontré et ce qui
// reste déduit du code.
// =============================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufWriter, Write};

    /// Réplique locale minimale de write_fixture_packfile (main.rs, mod
    /// tests — privée à ce module-là, jamais partagée entre les deux).
    /// Dupliquée ici plutôt que de changer sa visibilité pour ce seul
    /// appelant supplémentaire. Même format réel (packfile_path_for +
    /// PackfileEntry + write_packfile_footer) que le bootstrap production —
    /// ces tests prouvent quelque chose sur le vrai pipeline mmap, pas sur
    /// un raccourci synthétique.
    fn write_fixture_packfile(packfile_key: &'static str, ids_and_fragments: &[(i64, &[u8])]) {
        let path = marius_render::packfile_path_for(packfile_key);
        std::fs::create_dir_all(path.parent().expect("chemin toujours pourvu d'un parent"))
            .expect("création du répertoire de test");

        let mut blob = Vec::new();
        let mut entries = Vec::with_capacity(ids_and_fragments.len());
        let mut offset = 0u64;
        for (id, frag) in ids_and_fragments {
            blob.extend_from_slice(frag);
            entries.push(marius_render::PackfileEntry {
                id: *id,
                offset,
                len: frag.len() as u32,
                _pad: [0u8; 4],
            });
            offset += frag.len() as u64;
        }

        let file = std::fs::File::create(&path).expect("création packfile de test");
        let mut writer = BufWriter::new(file);
        writer.write_all(&blob).expect("écriture blob");
        marius_render::pack_html_format::write_packfile_footer(
            &mut writer,
            blob.len() as u64,
            &entries,
        )
        .expect("écriture footer+index");
        writer.flush().expect("flush");
    }

    /// T3 (empirique) — non-copie du payload mmap, prouvée par égalité de
    /// POINTEUR entre `owner.as_ref()` (le MmapOwner tel que
    /// resolve_route_to_response le construit réellement) et la plage déjà
    /// résolue par `resolve_range()`. Une égalité de contenu seule ne
    /// suffirait pas ici : une copie produirait aussi le bon contenu,
    /// ailleurs en mémoire — seule l'égalité d'adresse exclut
    /// structurellement une copie.
    ///
    /// Clé dédiée ("t2a_i5_non_copy_probe"), jamais "content_core" : ce
    /// test exerce resolve_generation/resolve_range/MmapOwner directement,
    /// sans passer par LiveRegistry ni par le stub de catalogue de
    /// resolve_route_to_response (qui capture "content_core" en dur) —
    /// aucune collision possible avec les tests HTTP de main.rs, qui
    /// restent seuls à toucher "content_core".
    #[test]
    fn owner_as_ref_matches_resolved_range_pointer_no_copy() {
        const KEY: &str = "t2a_i5_non_copy_probe";
        const FRAGMENT: &[u8] = b"<p>t2a-i5-no-copy-probe</p>";
        write_fixture_packfile(KEY, &[(1, FRAGMENT)]);

        let index = Arc::new(
            PackHtmlIndex::open(&marius_render::packfile_path_for(KEY))
                .expect("ouverture du packfile de test"),
        );

        // Même chemin que resolve_route_to_response : SourceSpec →
        // resolve_generation (fetch renvoie ici directement l'index déjà
        // ouvert, pas de registry impliqué) → MaterializedSource::Mmap.
        let spec = SourceSpec::StaticArtifact { key: SourceKey(0) };
        let source = resolve_generation(&spec, |_key| Some(Arc::clone(&index)))
            .expect("resolve_generation doit réussir");

        let range = resolve_range(&source, 1).expect("resolve_range doit réussir sur id=1");

        let MaterializedSource::Mmap { handle } = &source else {
            panic!("MaterializedSource::Mmap attendu");
        };
        let (offset, len) = handle.lookup(1).expect("lookup doit réussir sur id=1");

        let owner = MmapOwner {
            handle: Arc::clone(handle),
            offset,
            len,
        };

        // Non-copie — égalité de pointeur et de longueur, pas de contenu.
        assert_eq!(owner.as_ref().as_ptr(), range.ptr());
        assert_eq!(owner.as_ref().len(), range.len());

        // Le contenu, lui, confirme qu'on pointe bien sur le bon fragment
        // (nécessaire mais pas suffisant à lui seul — cf. commentaire
        // ci-dessus).
        assert_eq!(owner.as_ref(), FRAGMENT);
    }

    /// T3 (audit textuel) — absence de `read_at`/`Vec<u8>` dans ce fichier.
    /// L'absence d'un appel ne se mesure pas à l'exécution ; elle se
    /// vérifie sur le texte source. Portée strictement limitée à ce
    /// fichier — ne prouve rien sur emission.rs (déjà inchangé pour cet
    /// incrément) ni sur les dépendances (Hyper/Tokio/OS, hors périmètre,
    /// non mesurées ici).
    #[test]
    fn source_never_uses_read_at_or_vec_u8_payload() {
        let full_source = include_str!("experimental_t2a.rs");
        // Ne vérifie que le code de production, avant le module de tests
        // lui-même — celui-ci documente précisément ce qu'il vérifie, donc
        // contient nécessairement ces motifs en toutes lettres dans ses
        // propres commentaires et messages d'assertion (faux positif
        // garanti si on ne les exclut pas).
        let production_code = full_source
            .split("#[cfg(test)]")
            .next()
            .expect("le fichier contient toujours une section avant #[cfg(test)]");

        assert!(
            !production_code.contains("read_at"),
            "experimental_t2a.rs (code de production) ne doit jamais appeler read_at (T3, SPEC §7)"
        );
        assert!(
            !production_code.contains("Vec<u8>"),
            "experimental_t2a.rs (code de production) ne doit jamais matérialiser le payload dans un Vec<u8> (T3, SPEC §7)"
        );
    }
}
