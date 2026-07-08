// crates/shell/render/benches/counting_alloc.rs
// Allocateur global instrumenté pour la certification zéro-allocation.
//
// ─── Principe ────────────────────────────────────────────────────────────────
//
//   Un GlobalAlloc wrapper intercepte chaque appel à alloc() et dealloc().
//   Deux compteurs atomiques distincts tracent :
//     - ALLOC_COUNT : nombre d'allocations depuis le dernier reset.
//     - ALLOC_BYTES : octets alloués cumulés depuis le dernier reset.
//
//   Le benchmark délimite explicitement la zone de mesure :
//     CountingAlloc::reset();                    // remet les compteurs à 0
//     render_batch_pure::<P>(batch);             // zone instrumentée
//     let n = CountingAlloc::alloc_count();      // lit le résultat
//     assert_eq!(n, 0, "allocation détectée dans render()");
//
// ─── Ce que cette certification prouve et ne prouve pas ──────────────────────
//
//   PROUVE : render() n'alloue pas sur le tas pendant son exécution.
//            Concrètement : buf.reserve() ne déclenche pas de realloc
//            (DYNAMIC_CAP est correctement dimensionné), et aucun
//            push_str / write_fmt ne déborde la capacité pré-allouée.
//
//   NE PROUVE PAS : que le processus entier n'alloue pas.
//            Divan alloue pour ses structures de mesure.
//            Le runtime Tokio alloue pour ses tâches et son ordonnanceur.
//            Ces allocations sont réelles mais hors du chemin de rendu.
//            La fenêtre reset/read isole render() de ce bruit.
//
// ─── Activation ──────────────────────────────────────────────────────────────
//
//   Le wrapper est déclaré comme allocateur global uniquement dans ce fichier
//   de benchmark (via #[global_allocator]). Il n'affecte pas le code de
//   production : la feature n'existe que dans le contexte bench.
//
//   Dans hot_path_render.rs, déclarer en tête :
//     mod counting_alloc;
//     use counting_alloc::CountingAlloc;
//     #[global_allocator]
//     static ALLOC: CountingAlloc = CountingAlloc::new();

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};

// =============================================================================
// Compteurs globaux
// =============================================================================

/// Nombre d'allocations depuis le dernier reset().
/// AtomicU64 : lecture/écriture sans lock depuis n'importe quel thread du
/// runtime (allocateur global, appelable depuis n'importe quel contexte).
static ALLOC_COUNT: AtomicU64 = AtomicU64::new(0);

/// Octets alloués cumulés depuis le dernier reset().
/// Permet de distinguer "0 allocation" de "N petites allocations = 0 octet net".
static ALLOC_BYTES: AtomicU64 = AtomicU64::new(0);

// =============================================================================
// Wrapper allocateur
// =============================================================================

/// Allocateur global instrumenté.
///
/// Délègue chaque opération à l'allocateur système (jemalloc ou glibc malloc
/// selon la plateforme), en incrémentant les compteurs atomiques.
///
/// Déclarer comme allocateur global dans le fichier de benchmark :
/// ```
/// #[global_allocator]
/// static ALLOC: CountingAlloc = CountingAlloc::new();
/// ```
pub struct CountingAlloc;

impl CountingAlloc {
    pub const fn new() -> Self {
        Self
    }

    /// Remet les deux compteurs à zéro.
    /// Appeler immédiatement avant la zone à certifier.
    /// Ordering::SeqCst : barrière mémoire complète — garantit que toutes les
    /// opérations précédentes sont visibles sur tous les threads avant le reset.
    #[inline]
    pub fn reset() {
        ALLOC_COUNT.store(0, Ordering::SeqCst);
        ALLOC_BYTES.store(0, Ordering::SeqCst);
    }

    /// Nombre d'allocations enregistrées depuis le dernier reset().
    /// Ordering::SeqCst : garantit la visibilité de toute écriture concurrente.
    #[inline]
    pub fn alloc_count() -> u64 {
        ALLOC_COUNT.load(Ordering::SeqCst)
    }

    /// Octets alloués cumulés depuis le dernier reset().
    #[inline]
    pub fn alloc_bytes() -> u64 {
        ALLOC_BYTES.load(Ordering::SeqCst)
    }
}

// =============================================================================
// Implémentation GlobalAlloc
// =============================================================================

unsafe impl GlobalAlloc for CountingAlloc {
    /// Incrémente ALLOC_COUNT et ALLOC_BYTES, délègue à System.
    ///
    /// Ordering::Relaxed : la cohérence des compteurs entre threads est
    /// garantie par le SeqCst du reset() et du read() qui forment les
    /// bornes de la fenêtre de mesure. Les incréments intermédiaires
    /// n'ont pas besoin d'être totalement ordonnés entre eux.
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
        ALLOC_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        // SAFETY : délégation directe à l'allocateur système.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // dealloc n'est pas comptabilisé : on mesure les allocations nettes,
        // pas le turnover. Une dealloc sans alloc correspondante est impossible
        // dans du code Rust safe.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // alloc_zeroed est une allocation — on la comptabilise.
        ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
        ALLOC_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // realloc = allocation + libération de l'ancien bloc.
        // On la comptabilise comme une allocation nette (taille delta si croissance).
        if new_size > layout.size() {
            ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
            ALLOC_BYTES.fetch_add((new_size - layout.size()) as u64, Ordering::Relaxed);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}
