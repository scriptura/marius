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
use axum::http::{HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};

use marius_render::{IdSource, LiveRegistry, PackHtmlIndex, RouteEntry};

use crate::{ASSET_ROUTES, AssetRoute};

/// Handler unique pour tout asset statique — fallback du routeur (spec
/// §7/§9). `ASSET_ROUTES` (générée par build.rs, phf) est la seule liste
/// blanche : `uri.path()` sert de clé opaque, jamais de fragment de chemin
/// filesystem. C'est ce qui élimine toute traversée de chemin par
/// construction (aucune concaténation `base_dir + chemin_utilisateur`
/// n'existe nulle part dans cette fonction), pas par validation a
/// posteriori — une clé absente de la table est un 404 immédiat, zéro I/O
/// disque, avant même de considérer l'idée d'un chemin réel.
pub async fn serve_asset(uri: Uri) -> Response {
    // Lookup O(1) — seule opération avant tout I/O. `uri.path()` exclut la
    // query string par construction (http::Uri), pas besoin de la retirer
    // manuellement.
    let Some(route) = ASSET_ROUTES.get(uri.path()).copied() else {
        return StatusCode::NOT_FOUND.into_response();
    };

    deliver_asset(route).await
}

/// I/O réelle, isolée pour rester symétrique avec `deliver` (fragments HTML
/// ci-dessus) : une seule fonction fait l'ouverture, l'appelant ne connaît
/// que la donnée déjà validée.
///
/// `tokio::fs::read` (async natif, pas `spawn_blocking` + `std::fs::read`) :
/// n'immobilise jamais un worker Tokio pendant l'appel système, à la
/// différence du patron `spawn_blocking` retenu pour les packfiles HTML —
/// différence justifiée par la nature de l'opération (une lecture complète
/// de fichier borné, pas un `pread` à offset sur un fd long-lived partagé :
/// aucune raison ici d'éviter l'API async standard de Tokio).
async fn deliver_asset(route: AssetRoute) -> Response {
    match tokio::fs::read(route.path).await {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, HeaderValue::from_static(route.mime)),
                (header::CONTENT_LENGTH, HeaderValue::from(route.size)),
                (header::ETAG, HeaderValue::from_static(route.etag)),
                // URL déjà versionnée par le hash de contenu (spec §9) :
                // cache agressif légitime, c'est tout l'intérêt du
                // cache-busting — jamais de revalidation nécessaire tant
                // que l'URL ne change pas.
                (
                    header::CACHE_CONTROL,
                    HeaderValue::from_static("public, max-age=31536000, immutable"),
                ),
            ],
            bytes,
        )
            .into_response(),

        // route.path absent du disque alors qu'ASSET_ROUTES l'affirme :
        // désynchronisation build/déploiement (bug interne, artefact non
        // copié), jamais imputable à la requête cliente — 500, même
        // discipline que `deliver` ci-dessus pour les fragments HTML.
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

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
                (
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("text/html; charset=utf-8"),
                ),
            ],
            bytes,
        )
            .into_response(),

        // Ok(Err(_)) : échec du pread (fd invalide, erreur disque). Err(_) :
        // la tâche spawn_blocking elle-même a paniqué. Les deux sont des
        // anomalies internes, jamais imputables à la requête du client —
        // 500 dans les deux cas, pas de distinction utile côté HTTP.
        _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
