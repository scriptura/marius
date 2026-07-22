// crates/shell/render/src/store_provisioning.rs
//
// Correctif d'une erreur de conception de l'Étape 7 (Contrat d'Implémentation,
// Phase 1 — réactivité CoW) : l'affirmation « store.bin n'a pas d'équivalent
// auto-provisionné, contrairement à pack.bin » était fausse. Un store.bin
// VIDE (row_count = 0) est un fichier parfaitement valide au sens du format
// (PackfileStoreHeader), constructible sans aucune donnée PostgreSQL — la
// confusion venait d'avoir mélangé deux affirmations distinctes : « store.bin
// ne peut pas être rempli de données sans interroger Postgres » (vrai) et
// « store.bin ne peut pas avoir d'état initial valide sans Postgres » (faux).
//
// Découvert en confrontant un test préexistant
// (server_provisioning_and_supervision.rs,
// provisioning_on_empty_environment_starts_cleanly_and_serves_404) qui exige
// que le serveur démarre proprement sur un environnement entièrement vierge
// — contrat antérieur à cette session, que l'ajout non symétrique de
// cold_start_store() (Étape 7) a cassé sans que je le sache, faute d'avoir
// vu ce test avant.
//
// Motif strictement calqué sur ensure_provisioned (regenerate.rs) : .tmp +
// fsync + rename atomique, jamais de patch in-place, jamais de contact avec
// un fichier déjà présent (même invalide — cold_start_store() le détectera
// et échouera alors, symétrique à LiveRegistry::cold_start pour pack.bin).

use std::fs::{self, OpenOptions};
use std::io::{self, BufWriter, Write};

use bytemuck::Pod;
use marius_projection::Projection;

use crate::packfile_builder::PackfileBuilder;

/// Réutilise l'énumération déjà publique de `regenerate.rs` — structurellement
/// identique (un provisionnement a réussi, ou le fichier existait déjà),
/// aucune raison d'introduire un second type pour la même information.
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
