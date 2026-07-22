// =============================================================================
// marius-render · crates/shell/render/src/dumper.rs
//
// Full dump AOT : lit toutes les entités d'une table via Projection::fetch_from_pg,
// les sérialise en binary store (PackfileBuilder), écrit le fichier une fois.
//
// Point d'entrée : dump_table<P>(). Appelé par le binaire marius-dump au runtime,
// jamais par build.rs (compile-time) ni par le Dispatcher (incremental).
//
// Invariants :
//   INV-5 : PackfileBuilder::new(capacity) pré-alloue — aucun resize si
//            capacity >= nombre réel d'entités.
//   INV-6 : un seul File::create() + BufWriter::flush() par table.
//
// Voie d'Extraction (cold path) — séparée de la Voie d'Exécution (hot path).
// fetch_from_pg : réseau PG, allocations, SQLx.
// fetch_batch   : mmap, zéro allocation, OnceLock — réservé au serveur.
// =============================================================================

use std::fs::File;
use std::io::{BufWriter, Write};

use bytemuck::Pod;
use sqlx::PgPool;

use crate::packfile_builder::PackfileBuilder;
use marius_projection::Projection;

/// Taille d'un chunk de fetch. 4096 ids par aller-retour réseau.
const CHUNK_SIZE: usize = 4096;

/// Exécute le full dump d'une table : fetch par chunks depuis PostgreSQL,
/// accumulation, écriture binaire unique.
///
/// Utilise P::fetch_from_pg() — Voie d'Extraction AOT.
/// N'appelle jamais P::fetch_batch() (Voie d'Exécution mmap/serveur).
pub async fn dump_table<P>(pool: &PgPool, all_ids: &[i64], capacity: usize) -> std::io::Result<()>
where
    P: Projection,
    P::Record: Pod,
{
    let mut builder = PackfileBuilder::<P>::new(capacity);

    // ── Fetch par chunks depuis PostgreSQL ────────────────────────────────────
    for chunk in all_ids.chunks(CHUNK_SIZE) {
        let batch = P::fetch_from_pg(pool, chunk)
            .await
            .map_err(|e| std::io::Error::other(e.to_string()))?;

        if batch.is_empty() {
            continue;
        }
        builder.push_batch(&batch);
    }

    // ── Écriture binaire — un seul syscall open() ─────────────────────────────
    let store_path = P::store_path();
    if let Some(parent) = store_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let file = File::create(&store_path)?;
    let mut writer = BufWriter::new(file);
    builder.write(&mut writer)?;
    writer.flush()?;

    eprintln!(
        "[dumper] {} enreg. → {}",
        builder.row_count(),
        store_path.display()
    );

    Ok(())
}
