// Collector<MAX> — table de présence lock-free par Bit-Vector.
// Spécification : ADR-002 / session de conception Mai 2026.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::*};
use tokio::sync::Notify;

pub struct Collector<const MAX: usize>
where
    [(); (MAX + 63) / 64]: Sized,
{
    /// Bit-vector de présence. Bit (id-1) = signal en attente.
    /// Zero-initialized : sentinel = bit absent = pas de signal.
    presence: [AtomicU64; (MAX + 63) / 64],

    /// Approximation du nombre de bits positionnés.
    /// Utilisé pour le seuil volumétrique. Non garanti exact sous contention.
    count: AtomicUsize,

    /// Signaux ignorés car id > MAX.
    /// Sémantique : désynchronisation de configuration, pas saturation.
    /// Action corrective : relancer la Forge avec MAX_ENTITY_ID élargi.
    dropped: AtomicU64,
}

impl<const MAX: usize> Collector<MAX>
where
    [(); (MAX + 63) / 64]: Sized,
{
    /// Constructeur const — utilisé pour les statics générés par la Forge.
    /// Tous les AtomicU64 sont zero-initialized (aucun signal en attente).
    pub const fn new_zeroed() -> Self {
        // SAFETY : AtomicU64 est repr(transparent) sur u64.
        // zeroed() produit un tableau de 0u64, valeur valide pour AtomicU64.
        // Cette initialisation const est nécessaire pour les statics.
        //
        // Note : `[const { AtomicU64::new(0) }; N]` n'est pas stable
        // pour des tailles génériques. On utilise unsafe zeroed() en const.
        Self {
            presence: unsafe { std::mem::zeroed() },
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

        // fetch_or retourne l'ancienne valeur du mot.
        // Si le bit n'était pas positionné : nouveau signal, incrémenter count.
        let old = self.presence[word].fetch_or(bit, Release);
        if old & bit == 0 {
            let prev = self.count.fetch_add(1, AcqRel);
            if prev + 1 >= threshold {
                notify.notify_one();
            }
        }
        // Sinon : déduplication native, O(1), aucune action.
    }

    /// Vide le Collector et retourne les IDs en attente.
    /// Atomique : swap(0) sur chaque word libère les slots pour les inserts concurrents.
    /// Scan via TZCNT (trailing_zeros) : O(bits_set), pas O(MAX).
    pub fn flush(&self) -> Vec<i64> {
        let mut ids = Vec::with_capacity(self.count.load(Relaxed));
        let words   = (MAX + 63) / 64;

        for w in 0..words {
            // swap(0, AcqRel) : capture atomique + libération pour les inserts suivants.
            let mut word = self.presence[w].swap(0, AcqRel);

            while word != 0 {
                let bit = word.trailing_zeros() as usize; // TZCNT
                ids.push((w * 64 + bit + 1) as i64);
                word &= word - 1; // clear du bit le plus bas
            }
        }

        self.count.store(0, Release);
        ids
    }

    /// Nombre de signaux ignorés depuis le démarrage.
    /// > 0 indique que MAX_ENTITY_ID doit être élargi via la Forge.
    pub fn dropped_total(&self) -> u64 {
        self.dropped.load(Relaxed)
    }
}
