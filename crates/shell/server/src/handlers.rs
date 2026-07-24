// marius-render · crates/shell/server/src/handlers.rs

//! Routeurs HTTP & Livraison d'Empreintes (*Hot Path*).
//!
//! Point de terminaison du cycle de vie des requêtes (Spécification §6).
//! Le *Hot Path* est strictement dépourvu de tout calcul de rendu HTML : il se limite
//! à une projection Identifiant $\rightarrow$ Offset mémoire.
//!
//! ## Invariants I/O & Sympathie Concurrente
//!
//! - **Lecture Sans État (`read_at`) :** L'accès physique au flux binaire s'effectue exclusivement
//!   via `os::unix::fs::FileExt::read_at` (encapsulé dans `spawn_blocking` pour préserver
//!   l'exécuteur Tokio). L'usage d'opérations à état comme `seek()` est structurellement
//!   interdit pour prévenir toute *race condition* d'offset sur le descripteur de fichier partagé.
//! - **Complexité Bornée :** La localisation d'un fragment HTML cible s'opère par
//!   recherche dichotomique $O(\log N)$ dans l'index projeté en mémoire (*mmap*),
//!   sans allocation ni traversée de graphe.

use std::collections::HashMap;
use std::os::unix::fs::FileExt;
use std::sync::Arc;

use axum::extract::{Extension, Path, State};
use axum::http::{HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};

use marius_render::{IdSource, LiveRegistry, PackHtmlIndex, RouteEntry};

use crate::{ASSET_ROUTES, AssetRoute};

/// Gestionnaire de distribution des assets statiques (*Fallback* de routage, Spec §7/§9).
///
/// ## Sécurité Structurelle & Résolution AOT
///
/// - **Zéro Concaténation (Anti-Traversal) :** L'URI (`uri.path()`) est consommée comme une clé
///   logique opaque. Le système ne procède à aucune reconstruction dynamique de chemin de type
///   `base_dir + user_input`. Les vulnérabilités de *Path Traversal* sont neutralisées par le
///   modèle de données, sans dépendre d'une validation conditionnelle *a posteriori*.
/// - **Perfect Hash Function ($O(1)$) :** L'index `ASSET_ROUTES` est figé *Ahead-of-Time*
///   (via `build.rs` et la crate `phf`). La recherche de la ressource s'exécute en temps constant.
/// - **Court-circuit d'I/O (Zéro Appel Système) :** Toute requête ciblant une clé absente de la
///   table PHF est immédiatement avortée avec un code HTTP 404 en espace utilisateur, avant
///   même d'interagir avec le système de fichiers.
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
