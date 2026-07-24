// marius-render · crates/shell/render/src/store_provisioning.rs

//! Provisionnement AOT des Fichiers `store.bin` à l'État Vierge (`ensure_store_provisioned`).
//!
//! Garantit qu'un environnement d'exécution dépourvu d'extraction PostgreSQL préalable
//! génère un `store.bin` valide (à $0$ enregistrement), permettant au serveur de démarrer
//! de manière déterministe et de répondre proprement par des codes $404$ au lieu d'échouer au *Cold Start*.
//!
//! ## Invariants & Discipline Copy-on-Write (CoW)
//!
//! - **Validité Format $O(1)$ :** Un `store.bin` initial vide respecte strictement la topologie
//!   binaire `PackfileStoreHeader` (magic, version, header de 64 octets). Il ne requiert aucune
//!   interaction réseau avec la base de données.
//! - **Garantie Atomicité POSIX :** Le motif d'initialisation suit rigoureusement la séquence CoW :
//!   `Écriture .tmp` $\rightarrow$ `fsync` $\rightarrow$ `rename` atomique.
//! - **Absence de Corruption sur Écritures Partielles :** Ne patche jamais un fichier *in-place*.
//!   Si un fichier cible existe déjà (qu'il soit valide ou corrompu), la passe de provisionnement
//!   s'interrompt préventivement ; la phase de validation `cold_start_store()` prend le relais
//!   pour lever un échec explicite au démarrage.

use std::fs::{self, OpenOptions};
use std::io::{self, BufWriter, Write};

use bytemuck::Pod;
use marius_projection::Projection;

use crate::packfile_builder::PackfileBuilder;

/// Re-exporte l'énumération de résultat de provisionnement (`ProvisionOutcome`).
/// Aligné sur l'interface de `regenerate.rs` pour préserver un contrat d'appel unifié.
pub use crate::regenerate::ProvisionOutcome;

fn ensure_store_provisioned_sync<P: Projection>() -> io::Result<ProvisionOutcome>
where
    P::Record: Pod,
{
    let final_path = P::store_path();
    match fs::metadata(&final_path) {
        Ok(_) => Ok(ProvisionOutcome::AlreadyPresent),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let tmp_path = final_path.with_extension("tmp");
            if let Some(parent) = tmp_path.parent() {
                fs::create_dir_all(parent)?;
            }

            // store.bin vide — row_count = 0, aucune ligne, aucun champ
            // varlena écrit. Valide au sens du format (PackfileStoreHeader),
            // exactement comme write_packfile_footer(0, &[]) l'est pour
            // pack.bin.
            let mut builder = PackfileBuilder::<P>::new(0);
            builder.push_batch(&[]);

            let file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&tmp_path)?;
            let mut writer = BufWriter::new(file);
            builder.write(&mut writer)?;
            writer.flush()?;
            writer.into_inner().map_err(io::Error::other)?.sync_all()?;
            fs::rename(tmp_path, final_path)?;
            Ok(ProvisionOutcome::Provisioned)
        }
        Err(e) => Err(e),
    }
}

/// Garantit qu'un `store.bin` existe pour cette Projection — l'écrit vide
/// s'il est absent, ne touche jamais un fichier déjà présent (même
/// invalide). À appeler avant `{Proj}::cold_start_store()` dans le
/// bootstrap, exactement comme `ensure_provisioned` précède
/// `LiveRegistry::cold_start` pour `pack.bin`.
pub async fn ensure_store_provisioned<P: Projection + 'static>() -> io::Result<ProvisionOutcome>
where
    P::Record: Pod,
{
    tokio::task::spawn_blocking(ensure_store_provisioned_sync::<P>)
        .await
        .map_err(io::Error::other)?
}

#[cfg(test)]
mod tests {
    use super::*;
    use marius_projection::{BatchResult, StoreRegistry, VarlenSlot};
    use std::path::PathBuf;
    use std::sync::Mutex;

    #[repr(C)]
    #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
    struct Rec {
        id: i64,
    }

    static PATH: Mutex<Option<PathBuf>> = Mutex::new(None);

    struct TestProj;

    impl Projection for TestProj {
        type Record = Rec;
        type VarlenOwned = Option<String>;

        fn fetch_batch(
            _pool: &sqlx::PgPool,
            _ids: &[i64],
        ) -> impl std::future::Future<Output = BatchResult<Self>> + Send {
            std::future::ready(Ok(Vec::new()))
        }

        fn render(_r: &Self::Record, _v: &Self::VarlenOwned, _buf: &mut String) {}
        fn record_id(record: &Self::Record) -> i64 {
            record.id
        }
        fn packfile_path() -> PathBuf {
            PathBuf::new()
        }
        fn store_path() -> PathBuf {
            PATH.lock().unwrap().clone().unwrap()
        }
        fn store_registry() -> &'static StoreRegistry<Self> {
            static REGISTRY: StoreRegistry<TestProj> = StoreRegistry::new();
            &REGISTRY
        }
        fn varlena_field_count() -> u16 {
            0
        }
        fn encode_varlena(_v: &Self::VarlenOwned, _h: &mut Vec<u8>, toc: &mut Vec<VarlenSlot>) {
            toc.push(VarlenSlot {
                offset: u32::MAX,
                len: 0,
            });
        }
    }

    fn tmp_path(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "store_provisioning_test_{}_{}",
            std::process::id(),
            name
        ));
        p
    }

    #[tokio::test]
    async fn provisions_empty_store_when_absent() {
        let path = tmp_path("absent");
        let _ = std::fs::remove_file(&path);
        *PATH.lock().unwrap() = Some(path.clone());

        let outcome = ensure_store_provisioned::<TestProj>().await.unwrap();
        assert_eq!(outcome, ProvisionOutcome::Provisioned);
        assert!(path.exists());

        // cold_start_store() (via StoreRegistry::cold_start) doit réussir
        // sur ce fichier vide — c'est exactement le point que le test
        // externe (provisioning_on_empty_environment_...) exige.
        TestProj::store_registry()
            .cold_start(&path)
            .expect("cold_start doit réussir sur un store.bin vide, valide");
        assert_eq!(TestProj::store_registry().load().row_count(), 0);

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn never_touches_an_already_present_file() {
        let path = tmp_path("present");
        std::fs::write(&path, b"contenu quelconque, pas un store.bin valide").unwrap();
        *PATH.lock().unwrap() = Some(path.clone());

        let outcome = ensure_store_provisioned::<TestProj>().await.unwrap();
        assert_eq!(outcome, ProvisionOutcome::AlreadyPresent);

        let content = std::fs::read(&path).unwrap();
        assert_eq!(content, b"contenu quelconque, pas un store.bin valide");

        let _ = std::fs::remove_file(&path);
    }
}
