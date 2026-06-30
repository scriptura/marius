// =============================================================================
// crates/shell/server/src/handlers.rs
//
// Hot path — service d'une requête HTTP. specification-marius-render-shell.md
// §6. Lecture pure : aucun calcul HTML, résolution d'id → lookup O(log N) →
// livraison depuis le fd déjà ouvert (Option A retenue : read_at +
// spawn_blocking, jamais de seek() sur le fd partagé — voir pack_html_index.rs
// et la spec §6.3 pour la justification de la race condition évitée).
// =============================================================================

use std::collections::HashMap;
use std::os::unix::fs::FileExt;
use std::sync::Arc;

use axum::extract::{Extension, Path, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};

use marius_render::{IdSource, LiveRegistry, PackHtmlIndex, RouteEntry};

/// Point d'entrée Axum pour toute route de ROUTE_TABLE — la même fonction
/// sert toutes les routes, distinguées entre elles par la `RouteEntry`
/// injectée via `Extension` au montage (main.rs), pas par une closure
/// dédiée par route (spec §6.1).
pub async fn serve_route(
    Path(params): Path<HashMap<String, String>>,
    State(registry): State<Arc<LiveRegistry>>,
    Extension(route): Extension<&'static RouteEntry>,
) -> Response {
    let id = match route.id_source {
        IdSource::Fixed(n) => n,
        IdSource::PathParam(name) => match params.get(name).and_then(|s| s.parse::<i64>().ok()) {
            Some(id) => id,
            None => return StatusCode::BAD_REQUEST.into_response(),
        },
    };

    // registry.load(), pas un accès direct au champ — encapsulation retenue
    // en Phase 2 (registry.rs), conservée ici plutôt que l'extrait littéral
    // de la spec (`registry.indices[...].load()`, qui suppose le champ
    // public).
    let index_arc = match registry.load(route.packfile_key) {
        Some(idx) => idx,
        None => {
            // Atteindre cette branche signale une violation de l'invariant
            // établi par cold_start() : toute clé référencée par
            // ROUTE_TABLE a été ouverte au démarrage, sous peine d'échec
            // fatal au boot. Une clé absente ICI dénote un bug interne
            // (route enregistrée sans passer par cold_start), pas une
            // route inconnue côté client — 500, pas 404.
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    match index_arc.lookup(id) {
        Some((offset, len)) => deliver(index_arc, offset, len).await,
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Livraison depuis le fd déjà ouvert — spec §6.3, Option A (retenue pour la
/// v1). `read_at` (pread(2)) jamais `seek()+read()` : le curseur d'I/O d'un
/// fd POSIX est un état mutable partagé, inadapté à un fd accédé par des
/// requêtes Tokio concurrentes. `spawn_blocking` évite de geler un worker
/// Tokio pendant l'appel système, même backé par le page cache.
///
/// Coût concédé explicitement par la spec : une copie userspace par requête
/// (`Vec<u8>` alloué par appel) — arbitrage v1 accepté, pas un oubli (Option
/// B, `libc::sendfile` réel via `hyper::upgrade`, différée tant que l'Option
/// A n'est pas mesurée insuffisante — hors périmètre de cette session).
async fn deliver(index: Arc<PackHtmlIndex>, offset: u64, len: u32) -> Response {
    let result = tokio::task::spawn_blocking(move || -> std::io::Result<Vec<u8>> {
        let mut buf = vec![0u8; len as usize];
        index.file().read_at(&mut buf, offset)?;
        Ok(buf)
    })
    .await;

    match result {
        // Content-Length connu sans calcul (len déjà en mémoire, spec §6.2)
        // — émis directement, pas de Transfer-Encoding: chunked.
        
        Ok(Ok(bytes)) => (
            [
                // Conversion interne sans allocation tas pour les entiers courts
                (header::CONTENT_LENGTH, HeaderValue::from(len as u64)),
                // Pointage direct sur la section de données statiques (.rodata) du binaire
                (header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8")),
            ],
            bytes,
        ).into_response(),
        
        // Ok(Err(_)) : échec du pread (fd invalide, erreur disque). Err(_) :
        // la tâche spawn_blocking elle-même a paniqué. Les deux sont des
        // anomalies internes, jamais imputables à la requête du client —
        // 500 dans les deux cas, pas de distinction utile côté HTTP.
        _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
