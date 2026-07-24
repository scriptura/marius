// crates/shell/render/src/dumper.rs

//! Pipeline `[dumper]`.
//!
//! Extraction AOT complète (*Full Dump*). Matérialise les tables PostgreSQL
//! en un stockage binaire plat (`Packfile`), formaté spécifiquement pour un
//! accès futur sans allocation via projection mémoire (*mmap*).
//!
//! ## Invariants & Topologie
//!
//! - **Ségrégation Stricte (Cold/Hot Path) :** Ce module opère exclusivement sur la
//!   Voie d'Extraction (*Cold Path*). Il gère le réseau, SQLx, et les allocations
//!   (`Projection::fetch_from_pg`). Il n'interfère jamais avec la Voie d'Exécution
//!   (*Hot Path*, `fetch_batch`), garantissant que le serveur final reste déchargé
//!   de toute logique d'hydratation.
//! - **Stabilité Mémoire (INV-5) :** Le `PackfileBuilder` exige une pré-allocation
//!   explicite (`capacity`). Tant que le volume réel ne dépasse pas cette jauge,
//!   le pipeline garantit $O(1)$ allocation durant l'ingestion (zéro *resize* du backing buffer).
//! - **Discipline I/O (INV-6) :** L'empreinte système est plafonnée à un seul appel
//!   système `open()` (`File::create`) et un seul verrou de synchronisation
//!   (`BufWriter::flush`) par table traitée.

use std::fs::File;
use std::io::{BufWriter, Write};

use bytemuck::Pod;
use sqlx::PgPool;

use crate::packfile_builder::PackfileBuilder;
use marius_projection::Projection;

/// Dimensionnement de la fenêtre de rapatriement réseau.
/// Restreint l'empreinte mémoire transitoire en fragmentant les requêtes PG par blocs de 4096 IDs.
const CHUNK_SIZE: usize = 4096;

/// Exécute le rapatriement AOT et l'assemblage binaire d'une projection.
///
/// ## Sympathie Mécanique & Data Layout
///
/// L'exigence de la contrainte `P::Record: Pod` (*Plain Old Data*) est fondamentale :
/// elle prouve au compilateur que l'enregistrement ne contient aucun pointeur ni padding
/// indéfini. La structure mémoire reçue de PostgreSQL (après parsing SQLx) est ainsi
/// bit-à-bit équivalente à sa représentation de destination. Le `PackfileBuilder` peut
/// opérer une copie de blocs brute (*memcpy*) sans étape de sérialisation intermédiaire.
pub async fn dump_table<P>(pool: &PgPool, all_ids: &[i64], capacity: usize) -> std::io::Result<()>
where
    P: Projection,
    P::Record: Pod,
{
    // Pré-allocation déterministe du contiguous buffer.
    let mut builder = PackfileBuilder::<P>::new(capacity);

    // ── Voie d'Extraction (Réseau) ────────────────────────────────────────────
    for chunk in all_ids.chunks(CHUNK_SIZE) {
        let batch = P::fetch_from_pg(pool, chunk)
            .await
            .map_err(|e| std::io::Error::other(e.to_string()))?;

        if batch.is_empty() {
            continue;
        }

        // Copie des bytes du batch vers la section "Records" du builder.
        builder.push_batch(&batch);
    }

    // ── Matérialisation Disque ────────────────────────────────────────────────
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
