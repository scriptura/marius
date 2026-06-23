// =============================================================================
// crates/shell/render/src/pack_html_index.rs
//
// Lecteur d'un packfile HTML — symétrique de PackfileReader<P> (store.bin),
// format différent (footer, pas header — voir pack_html_format.rs et
// specification-marius-render-shell.md §3/§5).
//
// Principe directeur (spec §5) : tout mmap se fait au démarrage du processus
// (LiveRegistry::cold_start, Phase 2), jamais au premier accès. Aucune
// requête HTTP ne doit payer le coût d'un mmap().
// =============================================================================

use std::io;
use std::os::unix::fs::FileExt;
use std::path::Path;

use crate::pack_html_format::{PackfileEntry, PackfileFooter};

const FOOTER_SIZE: usize = std::mem::size_of::<PackfileFooter>();
const ENTRY_SIZE: usize = std::mem::size_of::<PackfileEntry>();

/// Lecteur d'un packfile HTML — fd conservé ouvert, mmap borné à la seule
/// région d'index.
pub struct PackHtmlIndex {
    /// fd conservé ouvert — zéro open() par requête. Jamais de seek() sur ce
    /// fd partagé : toute lecture positionnelle passe par read_at (pread(2),
    /// voir spec §6.3) — un seek() sur un fd accédé concurremment par
    /// plusieurs requêtes Tokio est une race condition (le curseur d'I/O
    /// POSIX est un état partagé mutable).
    file: std::fs::File,

    /// BORNÉ à la seule région d'index — le blob HTML n'entre jamais dans
    /// l'espace d'adressage virtuel de ce processus.
    ///
    /// `Option` plutôt que `Mmap` nu : `mmap(2)` POSIX rejette une longueur
    /// nulle (`EINVAL`) — pas une limite arbitraire contournable par un
    /// paramètre, l'absence d'un objet à mapper. `entry_count == 0` produit
    /// `index_len == 0` (table vide, cas explicitement exigé par le Jalon 1).
    /// `None` représente fidèlement « pas de mémoire mappée » ; coût nul à
    /// l'exécution (Null Pointer Optimization sur `Option<Mmap>`, `Mmap`
    /// portant un pointeur non nul en interne).
    mmap: Option<memmap2::Mmap>,

    entry_count: usize,
}

impl PackHtmlIndex {
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = std::fs::File::open(path)?;
        let file_len = file.metadata()?.len();

        // Lecture positionnelle du footer (32 derniers octets) via pread —
        // jamais seek()+read(), même au cold start : même fd que celui
        // réutilisé ensuite côté hot path (§6.3), aucune raison de déroger
        // ici alors que la garantie est requise partout ailleurs sur ce fd.
        let footer_start = (file_len as usize)
            .checked_sub(FOOTER_SIZE)
            .ok_or_else(|| io::Error::other("packfile trop court pour contenir un footer"))?;

        let mut footer_buf = [0u8; FOOTER_SIZE];
        file.read_at(&mut footer_buf, footer_start as u64)?;

        // pod_read_unaligned, pas from_bytes : ce buffer est une variable de
        // pile ([u8; 32]), dont l'alignement n'est garanti qu'à 1 par le
        // langage — from_bytes/cast_slice exigeraient un alignement à 8
        // (champs u64 du footer) qu'aucune garantie du type ne fournit ici.
        // pod_read_unaligned copie au lieu de transmuter en place : correct
        // indépendamment de l'adresse réelle du buffer source.
        let footer: PackfileFooter = bytemuck::pod_read_unaligned(&footer_buf);

        if &footer.magic != b"MARIUSPK" {
            return Err(io::Error::other("magic invalide"));
        }
        if footer.version != 1 {
            return Err(io::Error::other(format!(
                "version de footer inconnue : {}",
                footer.version
            )));
        }

        let expected_index_len = footer
            .entry_count
            .checked_mul(ENTRY_SIZE as u64)
            .ok_or_else(|| io::Error::other("overflow : entry_count * size_of::<PackfileEntry>()"))?;
        if footer.index_len != expected_index_len {
            return Err(io::Error::other(format!(
                "index_len ({}) incohérent avec entry_count ({}) — attendu {}",
                footer.index_len, footer.entry_count, expected_index_len
            )));
        }

        let index_start = (footer_start as u64)
            .checked_sub(footer.index_len)
            .ok_or_else(|| io::Error::other("index_len dépasse la taille disponible avant le footer"))?;

        // mmap borné exactement à la région d'index — c'est cette borne,
        // pas un commentaire, qui garantit que le blob n'est jamais mappé.
        let mmap = if footer.index_len == 0 {
            None
        } else {
            let m = unsafe {
                memmap2::MmapOptions::new()
                    .offset(index_start)
                    .len(footer.index_len as usize)
                    .map(&file)?
            };
            let _ = m.advise(memmap2::Advice::WillNeed); // pré-charge l'index
            Some(m)
        };

        Ok(Self {
            file,
            mmap,
            entry_count: footer.entry_count as usize,
        })
    }

    /// Recherche O(log N). Retourne (offset, len) — jamais les octets
    /// eux-mêmes, le blob n'est pas mmap'd (spec §6.3).
    ///
    /// `entry_count == 0` (donc `mmap == None`) retombe naturellement sur
    /// `None` via `?` — pas de branche séparée à maintenir : l'absence de
    /// région mappée et l'absence de résultat sont la même information.
    pub fn lookup(&self, id: i64) -> Option<(u64, u32)> {
        let mmap = self.mmap.as_ref()?;
        let entries: &[PackfileEntry] = bytemuck::cast_slice(&mmap[..]);
        entries
            .binary_search_by_key(&id, |e| e.id)
            .ok()
            .map(|i| (entries[i].offset, entries[i].len))
    }

    /// Accès au fd partagé pour une lecture positionnelle (spec §6.3) —
    /// jamais pour un seek() direct. Pas de `raw_fd()`/`AsRawFd` exposé :
    /// non utilisé avant la Phase 3, ne pas anticiper l'API qui n'en a pas
    /// encore besoin.
    pub fn file(&self) -> &std::fs::File {
        &self.file
    }

    pub fn entry_count(&self) -> usize {
        self.entry_count
    }
}

// =============================================================================
// Tests — Jalon 1
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack_html_format::write_packfile_footer;
    use std::io::{BufWriter, Write};
    use std::path::PathBuf;

    /// Écrit un packfile synthétique sur disque (blob + footer, via
    /// write_packfile_footer — réutilisée, pas réimplémentée) et retourne son
    /// chemin. Fichier nommé de façon unique par PID + compteur pour
    /// supporter l'exécution parallèle des tests (`cargo test` par défaut).
    fn write_synthetic_packfile(name: &str, blob: &[u8], index: &[PackfileEntry]) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "marius_pack_html_index_test_{name}_{}.bin",
            std::process::id()
        ));

        let file = std::fs::File::create(&path).expect("création fichier temporaire");
        let mut writer = BufWriter::new(file);
        writer.write_all(blob).expect("écriture du blob");
        write_packfile_footer(&mut writer, blob.len() as u64, index)
            .expect("écriture footer+index");
        writer.flush().expect("flush");

        path
    }

    /// Écrit un footer brut, potentiellement invalide, sans passer par
    /// write_packfile_footer — pour les tests de corruption volontaire.
    fn write_raw_footer(name: &str, blob: &[u8], footer: &PackfileFooter) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "marius_pack_html_index_test_{name}_{}.bin",
            std::process::id()
        ));

        let file = std::fs::File::create(&path).expect("création fichier temporaire");
        let mut writer = BufWriter::new(file);
        writer.write_all(blob).expect("écriture du blob");
        writer
            .write_all(bytemuck::bytes_of(footer))
            .expect("écriture footer brut");
        writer.flush().expect("flush");

        path
    }

    /// Supprime le fichier temporaire — best-effort, ignore l'échec (le test
    /// a déjà produit son verdict ; un résidu de /tmp n'est pas une raison de
    /// faire échouer la suite).
    fn cleanup(path: &Path) {
        let _ = std::fs::remove_file(path);
    }

    // ── Cas limite : table vide ────────────────────────────────────────────

    #[test]
    fn entry_count_zero_opens_and_always_misses() {
        let path = write_synthetic_packfile("empty", b"", &[]);

        let index = PackHtmlIndex::open(&path).expect("open() doit réussir sur table vide");
        assert_eq!(index.entry_count(), 0);

        assert_eq!(index.lookup(0), None);
        assert_eq!(index.lookup(42), None);
        assert_eq!(index.lookup(i64::MIN), None);
        assert_eq!(index.lookup(i64::MAX), None);

        cleanup(&path);
    }

    // ── Cas limite : une seule entrée ──────────────────────────────────────

    #[test]
    fn entry_count_one_lookup_hit_and_miss() {
        let blob = b"<article>seul fragment</article>".to_vec();
        let entry = PackfileEntry {
            id: 7,
            offset: 0,
            len: blob.len() as u32,
            _pad: [0u8; 4],
        };
        let path = write_synthetic_packfile("single", &blob, &[entry]);

        let index = PackHtmlIndex::open(&path).expect("open() doit réussir");
        assert_eq!(index.entry_count(), 1);

        assert_eq!(index.lookup(7), Some((0, blob.len() as u32)));
        assert_eq!(index.lookup(6), None);
        assert_eq!(index.lookup(8), None);

        cleanup(&path);
    }

    // ── Plusieurs entrées : binary_search correct sur tout id présent/absent ─

    #[test]
    fn binary_search_resolves_every_present_id_and_rejects_absent_ids() {
        let fragments: Vec<&[u8]> = vec![b"<a/>", b"<bb/>", b"<ccc/>", b"<dddd/>", b"<eeeee/>"];
        let ids = [10i64, 20, 30, 40, 50];

        let mut blob = Vec::new();
        let mut entries = Vec::new();
        let mut offset = 0u64;
        for (id, frag) in ids.iter().zip(fragments.iter()) {
            blob.extend_from_slice(frag);
            entries.push(PackfileEntry {
                id: *id,
                offset,
                len: frag.len() as u32,
                _pad: [0u8; 4],
            });
            offset += frag.len() as u64;
        }

        let path = write_synthetic_packfile("multi", &blob, &entries);
        let index = PackHtmlIndex::open(&path).expect("open() doit réussir");

        for (entry, frag) in entries.iter().zip(fragments.iter()) {
            assert_eq!(
                index.lookup(entry.id),
                Some((entry.offset, entry.len)),
                "lookup incorrect pour id={}",
                entry.id
            );
            let start = entry.offset as usize;
            let end = start + entry.len as usize;
            assert_eq!(&blob[start..end], *frag);
        }

        for absent in [0i64, 5, 15, 25, 35, 45, 55, 999] {
            assert_eq!(index.lookup(absent), None, "id={absent} ne devrait jamais matcher");
        }

        cleanup(&path);
    }

    // ── Blob massif, index minuscule — preuve que mmap reste petit ───────────

    #[test]
    fn mmap_stays_bounded_to_index_regardless_of_blob_size() {
        // ~200 MiB de blob, généré par répétition — streamé directement sur
        // disque par chunks, jamais matérialisé en un seul Vec<u8> en RAM :
        // le test prouve une propriété sur le fichier, pas sur la mémoire de
        // ce process de test.
        const CHUNK: &[u8] = &[b'x'; 65_536];
        const TARGET_LEN: u64 = 200 * 1024 * 1024;

        let path = std::env::temp_dir().join(format!(
            "marius_pack_html_index_test_massive_{}.bin",
            std::process::id()
        ));

        {
            let file = std::fs::File::create(&path).expect("création fichier temporaire");
            let mut writer = BufWriter::new(file);

            let mut written = 0u64;
            while written < TARGET_LEN {
                writer.write_all(CHUNK).expect("écriture chunk");
                written += CHUNK.len() as u64;
            }

            let entry = PackfileEntry {
                id: 1,
                offset: 0,
                len: written as u32,
                _pad: [0u8; 4],
            };
            write_packfile_footer(&mut writer, written, std::slice::from_ref(&entry))
                .expect("écriture footer+index");
            writer.flush().expect("flush");
        }

        let file_len = std::fs::metadata(&path).unwrap().len();
        assert!(
            file_len >= TARGET_LEN,
            "le fichier de test devrait dépasser TARGET_LEN, taille réelle={file_len}"
        );

        let index = PackHtmlIndex::open(&path).expect("open() doit réussir sur blob massif");
        assert_eq!(index.entry_count(), 1);

        // L'assertion qui rend l'invariant vérifiable plutôt que déclaratif :
        // la région mappée correspond à l'index (24B ici), jamais au blob
        // (~200 MiB) — accès au champ privé, légitime depuis ce sous-module
        // de test (même convention que batch_renderer.rs : renderer.buf,
        // renderer.index).
        let mapped_len = index.mmap.as_ref().map(|m| m.len()).unwrap_or(0);
        assert_eq!(mapped_len, ENTRY_SIZE, "mmap doit couvrir exactement 1 entrée (24B)");
        assert!(
            mapped_len < (file_len / 1000) as usize,
            "mmap ({mapped_len}B) devrait être négligeable face au fichier ({file_len}B)"
        );

        let (offset, len) = index.lookup(1).expect("id=1 doit être trouvé");
        assert_eq!(offset, 0);
        assert_eq!(len as u64, written_len_for(&path));

        cleanup(&path);
    }

    /// Relit la longueur réelle écrite (file_len - footer - index), pour
    /// l'assertion finale du test précédent sans dépendre d'une variable
    /// capturée hors de sa portée.
    fn written_len_for(path: &Path) -> u64 {
        let file_len = std::fs::metadata(path).unwrap().len();
        file_len - FOOTER_SIZE as u64 - ENTRY_SIZE as u64
    }

    // ── Footer corrompu : magic invalide ──────────────────────────────────

    #[test]
    fn corrupted_footer_invalid_magic_returns_err_never_panics() {
        let footer = PackfileFooter {
            magic: *b"BADMAGIC",
            version: 1,
            _pad: [0u8; 4],
            entry_count: 0,
            index_len: 0,
        };
        let path = write_raw_footer("bad_magic", b"", &footer);

        let result = PackHtmlIndex::open(&path);
        assert!(result.is_err(), "magic invalide doit produire une erreur, pas un panic");

        cleanup(&path);
    }

    // ── Footer corrompu : version inconnue ─────────────────────────────────

    #[test]
    fn corrupted_footer_unknown_version_returns_err_never_panics() {
        let footer = PackfileFooter {
            magic: *b"MARIUSPK",
            version: 99,
            _pad: [0u8; 4],
            entry_count: 0,
            index_len: 0,
        };
        let path = write_raw_footer("bad_version", b"", &footer);

        let result = PackHtmlIndex::open(&path);
        assert!(result.is_err(), "version inconnue doit produire une erreur, pas un panic");

        cleanup(&path);
    }

    // ── Footer corrompu : index_len incohérent avec entry_count ────────────

    #[test]
    fn corrupted_footer_inconsistent_index_len_returns_err_never_panics() {
        let footer = PackfileFooter {
            magic: *b"MARIUSPK",
            version: 1,
            _pad: [0u8; 4],
            entry_count: 3,
            index_len: 1, // devrait être 3 * 24 = 72
        };
        let path = write_raw_footer("bad_index_len", b"", &footer);

        let result = PackHtmlIndex::open(&path);
        assert!(
            result.is_err(),
            "index_len incohérent avec entry_count doit produire une erreur, pas un panic"
        );

        cleanup(&path);
    }

    // ── Fichier trop court pour contenir un footer ─────────────────────────

    #[test]
    fn file_shorter_than_footer_returns_err_never_panics() {
        let path = std::env::temp_dir().join(format!(
            "marius_pack_html_index_test_too_short_{}.bin",
            std::process::id()
        ));
        std::fs::write(&path, b"trop court").expect("écriture fichier minimal");

        let result = PackHtmlIndex::open(&path);
        assert!(
            result.is_err(),
            "fichier plus court que le footer doit produire une erreur, pas un panic"
        );

        cleanup(&path);
    }
}
