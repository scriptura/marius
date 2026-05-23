// marius-projection
// Trait Projection — interface canonique entre l'Orchestrator et les
// implémentations générées par Bridge-Forge + Fragment-Forge.
//
// Ce crate est la frontière Core/Shell :
//   - Il référence sqlx::PgPool (Shell) pour fetch_batch (Phase 1)
//   - Il ne référence pas Tokio (pure logique de projection)
//   - Phase 2 (SHM) modifiera uniquement les corps de fetch_batch,
//     pas la structure du trait.
//
// Signature render — pattern buffer (Phase 2 invariant) :
//   fn render(&Self::Record, &mut String)
//   Permet à Fragment-Forge d'utiliser String::with_capacity(STATIC + DYNAMIC)
//   et d'écrire via write!() sans allocation intermédiaire.
//   Contraste avec -> String : alloue systématiquement une nouvelle String.

use std::path::PathBuf;

pub trait Projection: Sized + Send + Sync + 'static {
    /// Type généré par DB-Forge — #[repr(C)], layout miroir PostgreSQL.
    /// Send : requis par into_par_iter() dans le Dispatcher.
    type Record: Sized + Send + 'static;
 
    /// Extraction batch depuis PostgreSQL (Phase 1 : SQLx).
    /// Phase 2 : l'implémentation utilisera des offsets mmap — même signature.
    ///
    /// impl Future + Send explicite : async fn dans un trait public ne permet pas
    /// de contraindre Send sur le Future retourné, bloquant tokio::spawn.
    fn fetch_batch(
        pool: &sqlx::PgPool,
        ids:  &[i64],
    ) -> impl std::future::Future<Output = Result<Vec<Self::Record>, sqlx::Error>> + Send;
 
    /// Rendu HTML du record dans le buffer fourni.
    ///
    /// Pattern buffer (pas de valeur de retour) :
    ///   - Zéro allocation si Fragment-Forge pré-calcule with_capacity.
    ///   - Le Dispatcher passe un buffer réutilisable entre les records.
    ///   - Compatible avec Maud's render_to(&mut String).
    ///
    /// Implémentation PoC : write! du Debug formatting.
    /// Fragment-Forge générera les macros Maud compilées.
    fn render(record: &Self::Record, buf: &mut String);
 
    /// Chemin déterministe de l'artefact produit.
    /// Racine via MARIUS_ARTIFACTS_DIR (défaut : ./artifacts).
    fn artifact_path(record: &Self::Record) -> PathBuf;
}
