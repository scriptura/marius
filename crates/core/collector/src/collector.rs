//! # marius-collector · crates/core/collector/src/collector.rs
//!
//! Table de présence lock-free par Bit-Vector.  
//! Aucune dépendance Tokio ni SQLx — Core pur.
//!
//! L'appelant (Shell / Dispatcher dans `marius-render`) reçoit `InsertResult`
//! et décide d'émettre `notify.notify_one()` si `ThresholdReached`.  
//! Ce pattern isole toute primitive de synchronisation async hors du Core.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::*};

/// Résultat d'un appel à Collector::insert.
/// Permet à l'appelant (Shell) de décider d'émettre un signal Tokio
/// sans que le Collector ne connaisse Tokio.
#[derive(Debug, PartialEq, Eq)]
pub enum InsertResult {
    /// Nouveau signal enregistré, seuil non atteint.
    Inserted,
    /// Signal ignoré : ID déjà présent dans le Bit-Vector (déduplication O(1)).
    Duplicate,
    /// Nouveau signal ET seuil volumétrique atteint — déclencher un flush immédiat.
    ThresholdReached,
    /// ID hors périmètre (id > MAX) — indicateur de désynchronisation de config.
    /// Action : relancer la Forge avec MAX_ENTITY_ID élargi (Article Zéro).
    Dropped,
}

pub struct Collector<const MAX: usize, const WORDS: usize> {
    /// Bit-vector de présence. Bit (id-1) = signal en attente.
    presence: [AtomicU64; WORDS],
    /// Approximation du nombre de bits positionnés.
    count: AtomicUsize,
    /// Signaux ignorés car id > MAX — désynchronisation de configuration.
    dropped: AtomicU64,
}

impl<const MAX: usize, const WORDS: usize> Collector<MAX, WORDS> {
    pub const fn new_zeroed() -> Self {
        Self {
            presence: [const { AtomicU64::new(0) }; WORDS],
            count: AtomicUsize::new(0),
            dropped: AtomicU64::new(0),
        }
    }

    /// Insère un signal pour l'entité `id`.
    /// Idempotent : si le bit est déjà positionné, retourne Duplicate.
    /// L'appelant appelle notify.notify_one() si ThresholdReached.
    pub fn insert(&self, id: i64, threshold: usize) -> InsertResult {
        if id < 1 || id as usize > MAX {
            self.dropped.fetch_add(1, Relaxed);
            return InsertResult::Dropped;
        }

        let idx = (id - 1) as usize;
        let word = idx / 64;
        let bit = 1u64 << (idx % 64);

        let old = self.presence[word].fetch_or(bit, Release);

        if old & bit != 0 {
            return InsertResult::Duplicate;
        }

        let prev = self.count.fetch_add(1, AcqRel);
        if prev + 1 >= threshold {
            InsertResult::ThresholdReached
        } else {
            InsertResult::Inserted
        }
    }

    /// Vide le Collector et retourne les IDs en attente.
    /// swap(0, AcqRel) libère les slots pour les inserts concurrents.
    /// Scan en O(bits_set) via trailing_zeros() (TZCNT sur x86).
    pub fn flush(&self) -> Vec<i64> {
        let mut ids = Vec::with_capacity(self.count.load(Relaxed));

        for w in 0..WORDS {
            let mut word = self.presence[w].swap(0, AcqRel);
            while word != 0 {
                let bit = word.trailing_zeros() as usize;
                ids.push((w * 64 + bit + 1) as i64);
                word &= word - 1;
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
