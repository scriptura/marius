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
// ─── ADR-003 : Dualité Record / VarlenOwned ───────────────────────────────────
//
//   L'introduction des champs varlena brise l'invariant monolithique
//   où type Record gérait une structure unique 'static.
//   Le chemin critique distingue désormais deux natures de données :
//
//   Record      : struct #[repr(C)], fixed-length, layout miroir PostgreSQL.
//                 Possédée, 'static, Send. Vit en mémoire contiguë.
//                 Exemple : {Name}StorageRow.
//
//   VarlenOwned : struct possédée portant les données varlena (Option<String>).
//                 'static, Send : peut traverser tokio::spawn et rayon::par_iter.
//                 () pour les tables sans varlena (coût zéro à la compilation).
//                 Exemple : {Name}VarlenOwned, ou () si pas de JOIN varlena.
//
// ─── Payload<'a> hors du trait ───────────────────────────────────────────────
//
//   {Name}RenderPayload<'a> (Option<&'a str>) n'est PAS un type associé du trait.
//   Il est construit localement dans render() via varlena.as_deref() sur chaque
//   thread Rayon, sans traversée de frontière de lifetime.
//   Raison : un GAT avec lifetime dans le trait contraint fortement les bounds
//   sur le Dispatcher générique pour un gain nul côté API publique.
//
// ─── Transition Phase 2 ──────────────────────────────────────────────────────
//
//   fetch_batch : signature inchangée côté Dispatcher.
//   L'implémentation générée substituera sqlx::query_as par un lecteur mmap.
//   VarlenOwned sera produit depuis le buffer de page WAL, pas depuis une Row sqlx.
//   render() : inchangé — ne dépend pas de la source des données.
//
// ─── Signature render — pattern buffer ───────────────────────────────────────
//
//   fn render(&Self::Record, &Self::VarlenOwned, &mut String)
//   Permet à Fragment-Forge d'utiliser String::with_capacity(STATIC + DYNAMIC)
//   et d'écrire via write_fmt() sans allocation intermédiaire.
//   Contraste avec -> String : alloue systématiquement une nouvelle String.

use std::path::PathBuf;

/// Alias de type pour le retour de fetch_batch.
/// Évite la répétition du type complexe dans le trait et les implémentations.
/// Nommé explicitement pour la lisibilité dans les bounds du Dispatcher.
pub type BatchResult<P> = Result<
    Vec<(<P as Projection>::Record, <P as Projection>::VarlenOwned)>,
    sqlx::Error,
>;

pub trait Projection: Sized + Send + Sync + 'static {
    /// Layout fixed-length, #[repr(C)], miroir du heap tuple PostgreSQL.
    /// Send + 'static : peut traverser tokio::spawn et rayon::par_iter.
    type Record: Sized + Send + 'static;

    /// Données varlena possédées (Option<String>), issues du fetch SQLx.
    /// Send + 'static : même contrainte que Record pour la traversée de threads.
    /// () pour les tables sans colonnes varlena (coût zéro, optimisé par le compilateur).
    type VarlenOwned: Sized + Send + 'static;

    /// Extraction batch depuis PostgreSQL (Phase 1 : SQLx).
    /// Retourne le couple (Record, VarlenOwned) possédé par enregistrement.
    /// Le Dispatcher reconstruit le RenderPayload (&str) localement sur chaque
    /// thread Rayon depuis VarlenOwned via as_deref() — sans traversée de lifetime.
    ///
    /// Phase 2 : l'implémentation utilisera des offsets mmap — même signature.
    ///
    /// impl Future<Output> + Send explicite : async fn dans un trait public ne
    /// permet pas de contraindre Send sur le Future retourné, bloquant tokio::spawn.
    fn fetch_batch(
        pool: &sqlx::PgPool,
        ids:  &[i64],
    ) -> impl std::future::Future<Output = BatchResult<Self>> + Send;

    /// Rendu HTML du record dans le buffer fourni.
    ///
    /// `record`  : données fixed-length (StorageRow, repr(C)).
    /// `varlena` : données varlena possédées. Le corps généré par Fragment-Forge
    ///             effectue les as_deref() localement pour construire les &str.
    ///             Passé comme &VarlenOwned plutôt que &Payload<'_> pour éviter
    ///             un GAT dans le trait (détail d'implémentation interne à render).
    ///
    /// Invariant no-realloc : buf.capacity() == TOTAL_CAP avant et après.
    /// Le Dispatcher passe un buffer réutilisable entre les records du même batch.
    fn render(record: &Self::Record, varlena: &Self::VarlenOwned, buf: &mut String);

    /// Chemin déterministe de l'artefact produit.
    /// Racine via MARIUS_ARTIFACTS_DIR (défaut : ./artifacts).
    fn artifact_path(record: &Self::Record) -> PathBuf;
}
