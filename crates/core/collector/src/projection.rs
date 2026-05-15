// Trait Projection — interface unique entre le Dispatcher et les implémentations
// générées par Bridge-Forge + Fragment-Forge.
//
// Le Dispatcher est générique sur P: Projection.
// Chaque table surveillée a une implémentation concrète générée par la Forge.

use std::future::Future;
use std::path::PathBuf;

pub trait Projection: Sized + Send + Sync + 'static {
    /// Type généré par DB-Forge — #[repr(C)], layout miroir PostgreSQL.
    type Record: Sized + Send + 'static;

    /// Extraction batch depuis PostgreSQL.
    /// Généré par Bridge-Forge — sqlx::query_as! + conversion Row→Store.
    fn fetch_batch(
        pool: &sqlx::PgPool,
        ids:  &[i64],
    ) -> impl Future<Output = Result<Vec<Self::Record>, sqlx::Error>> + Send;

    /// Rendu HTML du record — généré par Fragment-Forge (macros Maud).
    /// Pas d'appel système, pas d'I/O : pure transformation mémoire→String.
    fn render(record: &Self::Record) -> String;

    /// Chemin de l'artefact produit (fichier statique ou clé RAM).
    fn artifact_path(record: &Self::Record) -> PathBuf;
}
