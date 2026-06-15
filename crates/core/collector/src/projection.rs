// marius-collector · projection.rs
// Trait Projection : interface entre le Dispatcher et les implémentations
// générées par Bridge-Forge + Fragment-Forge.
//
// fetch_batch retourne impl Future + Send explicite :
// async fn dans un trait ne permet pas de contraindre Send sur le Future retourné,
// ce qui bloquerait l'utilisation dans tokio::spawn et les contextes multi-thread.

use std::path::PathBuf;

pub trait Projection: Sized + Send + Sync + 'static {
    /// Type généré par DB-Forge — #[repr(C)], layout miroir PostgreSQL.
    type Record: Sized + Send + 'static;

    /// Extraction batch depuis PostgreSQL.
    /// Généré par Bridge-Forge — sqlx::query_as! + conversion Row→Store.
    ///
    /// `impl Future + Send` explicite : permet aux appelants d'utiliser
    /// ce Future dans des contextes multi-thread (tokio::spawn, rayon).
    /// `async fn` dans un trait ne permet pas de spécifier Send sur le Future.
    fn fetch_batch(
        pool: &sqlx::PgPool,
        ids:  &[i64],
    ) -> impl std::future::Future<Output = Result<Vec<Self::Record>, sqlx::Error>> + Send;

    /// Rendu HTML du record — généré par Fragment-Forge (écriture directe AOT).
    /// Pure transformation mémoire→String : aucun appel système, aucun I/O.
    fn render(record: &Self::Record) -> String;

    /// Chemin de l'artefact produit (fichier statique ou clé RAM).
    fn artifact_path(record: &Self::Record) -> PathBuf;

    /// Retourne le chemin où stocker les données générées
    fn store_path() -> ::std::path::PathBuf;
}
