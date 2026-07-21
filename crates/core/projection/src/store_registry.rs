// marius-projection — crates/core/projection/src/store_registry.rs
//
// StoreRegistry — cf. DESIGN-store-registry.md
//
// Registre mono-slot, atomiquement remplaçable, pour un PackfileReader<P>.
// Remplace le `static OnceLock<PackfileReader<P>>` généré aujourd'hui par
// codegen/projection.rs (db-forge) — cf. Contrat d'Implémentation, Étape 3.
//
// INV-1..INV-5 : cf. DESIGN-store-registry.md §4, §7.

use std::path::Path;
use std::sync::{Arc, RwLock};

use bytemuck::Pod;

use crate::Projection;
use crate::packfile_reader::PackfileReader;

pub struct StoreRegistry<P: Projection>
where
    P::Record: Pod,
{
    current: RwLock<Option<Arc<PackfileReader<P>>>>,
}

impl<P: Projection> StoreRegistry<P>
where
    P::Record: Pod,
{
    /// État initial : aucune version montée. `const fn` — compatible `static`
    /// (`std::sync::RwLock::new` est `const` depuis Rust 1.63).
    pub const fn new() -> Self {
        Self {
            current: RwLock::new(None),
        }
    }

    /// Provisionnement à froid — à appeler une fois, avant toute requête,
    /// jamais en cours de service. Échoue si `store.bin` est absent ou
    /// invalide (magic/version/stride/longueur — validations déjà portées
    /// par `PackfileReader::open`).
    ///
    /// N'échoue jamais silencieusement : l'appelant (bootstrap, Étape 7 du
    /// Contrat d'Implémentation) doit traiter tout `Err` comme fatal.
    pub fn cold_start(&self, path: &Path) -> std::io::Result<()> {
        let reader = PackfileReader::<P>::open(path)?;
        let mut guard = self
            .current
            .write()
            .expect("[StoreRegistry] verrou empoisonné pendant cold_start");
        *guard = Some(Arc::new(reader));
        Ok(())
    }

    /// Lecture — chemin appelé par `fetch_batch`. Doit être appelé une seule
    /// fois par batch (INV-5) ; jamais dans une boucle sur `ids`.
    ///
    /// # Panics
    /// Si `cold_start` n'a jamais réussi. Un registre non provisionné est un
    /// bug d'intégration (bootstrap incomplet), pas un état à tolérer
    /// silencieusement sur le chemin de rendu.
    pub fn load(&self) -> Arc<PackfileReader<P>> {
        let guard = self
            .current
            .read()
            .expect("[StoreRegistry] verrou empoisonné pendant load");
        guard
            .as_ref()
            .expect("[StoreRegistry] load() appelé avant un cold_start() réussi")
            .clone()
    }

    /// Écriture — chemin appelé par `ingest_and_swap`, après succès complet
    /// de merge_store + write + fsync + rename + réouverture de validation
    /// (cf. DESIGN-store-registry.md §6). Le verrou n'est tenu que le temps
    /// du remplacement du pointeur — jamais pendant l'I/O qui précède.
    pub fn swap(&self, new: Arc<PackfileReader<P>>) {
        let mut guard = self
            .current
            .write()
            .expect("[StoreRegistry] verrou empoisonné pendant swap");
        *guard = Some(new);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PackfileStoreHeader, align8};
    use std::path::PathBuf;

    // ── Projection minimale pour les tests, sans varlena ──────────────────

    #[repr(C)]
    #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
    struct TestRecord {
        id: i64,
        val: i64,
    }

    struct TestProj;

    impl Projection for TestProj {
        type Record = TestRecord;
        type VarlenOwned = ();

        fn fetch_batch(
            _pool: &sqlx::PgPool,
            _ids: &[i64],
        ) -> impl std::future::Future<Output = crate::BatchResult<Self>> + Send {
            std::future::ready(Ok(Vec::new()))
        }

        fn render(_record: &Self::Record, _varlena: &Self::VarlenOwned, _buf: &mut String) {}

        fn record_id(record: &Self::Record) -> i64 {
            record.id
        }

        fn packfile_path() -> PathBuf {
            PathBuf::new()
        }

        fn store_path() -> PathBuf {
            PathBuf::new()
        }
    }

    /// Construit un store.bin minimal et valide (aucun champ varlena),
    /// pour exercer StoreRegistry sans dépendre de PackfileBuilder
    /// (crate marius_render, non disponible dans ce scratch).
    /// Respecte exactement le format lu par PackfileReader::open (lib.rs).
    fn write_minimal_store(path: &Path, rows: &[(i64, i64)]) {
        let header_size = std::mem::size_of::<PackfileStoreHeader>();
        let stride = std::mem::size_of::<TestRecord>();
        let row_count = rows.len();

        let rows_offset = header_size;
        let id_index_offset = align8((rows_offset + row_count * stride) as u64) as usize;
        let toc_offset = align8((id_index_offset + row_count * 8) as u64) as usize;
        // varlena_field_count = 0 → aucune entrée TOC, heap immédiatement après.
        let heap_offset = align8(toc_offset as u64) as usize;
        let heap_len = 0usize;

        let mut buf = vec![0u8; heap_offset + heap_len];

        let header = PackfileStoreHeader {
            magic: *b"MARIUSDB",
            version: 1,
            stride: stride as u32,
            row_count: row_count as u64,
            varlena_field_count: 0,
            _pad: [0; 6],
            id_index_section: id_index_offset as u64,
            varlena_toc_section: toc_offset as u64,
            varlena_heap_section: heap_offset as u64,
            varlena_heap_len: heap_len as u64,
        };
        buf[..header_size].copy_from_slice(bytemuck::bytes_of(&header));

        for (i, (id, val)) in rows.iter().enumerate() {
            let rec = TestRecord { id: *id, val: *val };
            let off = rows_offset + i * stride;
            buf[off..off + stride].copy_from_slice(bytemuck::bytes_of(&rec));
            let idx_off = id_index_offset + i * 8;
            buf[idx_off..idx_off + 8].copy_from_slice(&id.to_ne_bytes());
        }

        std::fs::write(path, buf).unwrap();
    }

    fn tmp_path(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "store_registry_test_{}_{}",
            std::process::id(),
            name
        ));
        p
    }

    // ── Critères de complétion, Étape 1 du Contrat d'Implémentation ───────

    #[test]
    #[should_panic(expected = "load() appelé avant un cold_start")]
    fn load_panics_before_cold_start() {
        let reg = StoreRegistry::<TestProj>::new();
        reg.load();
    }

    #[test]
    fn cold_start_fails_on_missing_file() {
        let reg = StoreRegistry::<TestProj>::new();
        let missing = tmp_path("does_not_exist.bin");
        assert!(reg.cold_start(&missing).is_err());
    }

    #[test]
    fn cold_start_fails_on_invalid_header() {
        let reg = StoreRegistry::<TestProj>::new();
        let path = tmp_path("invalid_header.bin");
        std::fs::write(&path, b"not a valid store.bin at all, too short").unwrap();
        assert!(reg.cold_start(&path).is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_after_cold_start_resolves_rows() {
        let reg = StoreRegistry::<TestProj>::new();
        let path = tmp_path("basic.bin");
        write_minimal_store(&path, &[(1, 10), (2, 20), (3, 30)]);

        reg.cold_start(&path)
            .expect("cold_start doit réussir sur un store.bin valide");
        let reader = reg.load();
        let (record, _varlena) = reader.lookup(2).expect("id 2 doit être présent");
        assert_eq!(record.val, 20);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_after_swap_returns_new_version() {
        let reg = StoreRegistry::<TestProj>::new();
        let path_v1 = tmp_path("swap_v1.bin");
        let path_v2 = tmp_path("swap_v2.bin");
        write_minimal_store(&path_v1, &[(1, 100)]);
        write_minimal_store(&path_v2, &[(1, 999)]);

        reg.cold_start(&path_v1).unwrap();
        assert_eq!(reg.load().lookup(1).unwrap().0.val, 100);

        let new_reader = PackfileReader::<TestProj>::open(&path_v2).unwrap();
        reg.swap(Arc::new(new_reader));

        assert_eq!(reg.load().lookup(1).unwrap().0.val, 999);

        let _ = std::fs::remove_file(&path_v1);
        let _ = std::fs::remove_file(&path_v2);
    }

    #[test]
    fn arc_held_before_swap_stays_valid_and_unchanged() {
        // INV-3 : un Arc obtenu par load() avant un swap() reste valide et
        // inchangé après — même si le fichier disque a été remplacé.
        let reg = StoreRegistry::<TestProj>::new();
        let path_v1 = tmp_path("inv3_v1.bin");
        let path_v2 = tmp_path("inv3_v2.bin");
        write_minimal_store(&path_v1, &[(1, 1)]);
        write_minimal_store(&path_v2, &[(1, 2)]);

        reg.cold_start(&path_v1).unwrap();
        let held = reg.load(); // référence conservée avant le swap

        let new_reader = PackfileReader::<TestProj>::open(&path_v2).unwrap();
        reg.swap(Arc::new(new_reader));

        // La référence détenue depuis avant le swap voit toujours l'ancienne valeur.
        assert_eq!(held.lookup(1).unwrap().0.val, 1);
        // Un nouveau load() voit la nouvelle valeur.
        assert_eq!(reg.load().lookup(1).unwrap().0.val, 2);

        let _ = std::fs::remove_file(&path_v1);
        let _ = std::fs::remove_file(&path_v2);
    }

    #[test]
    fn two_loads_without_swap_return_same_arc() {
        let reg = StoreRegistry::<TestProj>::new();
        let path = tmp_path("ptr_eq.bin");
        write_minimal_store(&path, &[(1, 1)]);
        reg.cold_start(&path).unwrap();

        let a = reg.load();
        let b = reg.load();
        assert!(Arc::ptr_eq(&a, &b));

        let _ = std::fs::remove_file(&path);
    }
}
