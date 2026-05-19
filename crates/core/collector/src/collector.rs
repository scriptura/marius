// marius-collector · collector.rs
// Table de présence lock-free par Bit-Vector.
// Spécification : ADR-002 / session de conception Mai 2026.
//
// Deux const generics stables :
//   MAX  : identifiant maximal accepté (borne domaine)
//   WORDS: taille du tableau = ceil(MAX / 64), arrondi power-of-two par la Forge.
// La relation WORDS == (MAX + 63) / 64 est imposée par la Forge au build-time.
// Elle n'est pas encodée dans le type system (generic_const_exprs instable).

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::*};
use tokio::sync::Notify;

pub struct Collector<const MAX: usize, const WORDS: usize> {
    /// Bit-vector de présence. Bit (id-1) = signal en attente.
    presence: [AtomicU64; WORDS],
    /// Approximation du nombre de bits positionnés (non garanti exact sous contention).
    count:    AtomicUsize,
    /// Signaux ignorés car id > MAX — indicateur de désynchronisation de configuration.
    /// Action corrective : relancer la Forge avec MAX_ENTITY_ID élargi (Article Zéro).
    dropped:  AtomicU64,
}

impl<const MAX: usize, const WORDS: usize> Collector<MAX, WORDS> {
    /// Constructeur const — pour les statics générés par la Forge.
    /// `AtomicU64::new(0)` est const : aucun unsafe requis.
    pub const fn new_zeroed() -> Self {
        Self {
            // [expr; N] avec expr const et N const generic : stable depuis Rust 1.79.
            // AtomicU64 n'est pas Copy mais `AtomicU64::new(0)` est une expr const.
            presence: [const { AtomicU64::new(0) }; WORDS],
            count:    AtomicUsize::new(0),
            dropped:  AtomicU64::new(0),
        }
    }

    /// Insère un signal pour l'entité `id`.
    /// Idempotent : si le bit est déjà positionné, aucun effet sur `count`.
    /// Déclenche `notify` si le seuil volumétrique `threshold` est atteint.
    pub fn insert(&self, id: i64, threshold: usize, notify: &Notify) {
        if id < 1 || id as usize > MAX {
            self.dropped.fetch_add(1, Relaxed);
            return;
        }

        let idx  = (id - 1) as usize;
        let word = idx / 64;
        let bit  = 1u64 << (idx % 64);

        let old = self.presence[word].fetch_or(bit, Release);
        if old & bit == 0 {
            // Premier signal pour cet ID : incrémenter et vérifier le seuil.
            let prev = self.count.fetch_add(1, AcqRel);
            if prev + 1 >= threshold {
                notify.notify_one();
            }
        }
        // Sinon : déduplication native, O(1).
    }

    /// Vide le Collector et retourne les IDs en attente.
    /// swap(0, AcqRel) sur chaque word libère les slots pour les inserts concurrents.
    /// Scan en O(bits_set) via trailing_zeros() (TZCNT sur x86).
    pub fn flush(&self) -> Vec<i64> {
        let mut ids = Vec::with_capacity(self.count.load(Relaxed));

        for w in 0..WORDS {
            let mut word = self.presence[w].swap(0, AcqRel);
            while word != 0 {
                let bit = word.trailing_zeros() as usize;
                ids.push((w * 64 + bit + 1) as i64);
                word &= word - 1; // clear du bit le plus bas
            }
        }

        self.count.store(0, Release);
        ids
    }

    /// Nombre de signaux ignorés depuis le démarrage.
    /// > 0 : MAX doit être élargi via la Forge.
    pub fn dropped_total(&self) -> u64 {
        self.dropped.load(Relaxed)
    }
}
