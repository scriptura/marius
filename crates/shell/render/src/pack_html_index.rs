// crates/shell/render/src/pack_html_index.rs

//! Lecteur $O(1)$ Zero-Copy pour le Packfile HTML (`pack.bin`).
//!
//! Représente l'opposé symétrique de `PackfileReader<P>` (dédié à `store.bin`).
//! Il consomme le layout binaire inversé avec en-tête terminal (*footer-based*, cf. `pack_html_format.rs`).
//!
//! ## Phase 0.B — mapping persistant du blob (option A)
//!
//! Décision arbitrée (2026-09, confrontation ADR-011) : le mapping ne se limite
//! plus à la seule région d'index. Une seule projection mémoire, ouverte une
//! fois à `open()` et maintenue vivante pour toute la durée de vie de cette
//! génération, couvre `[0, footer_start)` — c'est-à-dire le blob HTML et
//! l'index, jamais le footer. Le format physique on-disk (`pack_html_format.rs`)
//! n'est pas modifié.
//!
//! Motivation : le futur `SegmentDescriptor { offset, len }` (DESIGN Runtime,
//! post-ADR-011) doit pouvoir désigner directement une plage mémoire d'une
//! source statique publiée, sans repasser par un appel système. `blob()`
//! ci-dessous est la primitive sûre qui résout `(offset, len)` vers une
//! tranche de ce mapping — elle n'est pas elle-même `SegmentDescriptor` ni
//! une abstraction générale de source : cette résolution reste hors du
//! périmètre de ce module (DESIGN Runtime, Phase 3+).
//!
//! Propriété de durée de vie : une tranche retournée par `blob()` emprunte
//! `&self` — sa validité est donc celle de l'objet `PackHtmlIndex` qui la
//! produit, typiquement possédé via `Arc<PackHtmlIndex>` par le code
//! appelant (cf. `LiveRegistry::load`). Un remplacement ultérieur de la
//! génération publiée (`LiveRegistry::store`) n'invalide jamais une tranche
//! déjà empruntée : `store()` substitue le pointeur dans le registre, il ne
//! touche jamais aux instances déjà chargées par un appelant antérieur —
//! celles-ci restent vivantes tant que leur `Arc` l'est (cf. tests de
//! concurrence, `registry.rs`).
//!
//! ## Invariants de Performance & Sympathie Mécanique
//!
//! - **Injection Mémoire Préalable ($O(1)$ Cold Start) :** La projection mémoire (*mmap*) est
//!   effectuée exhaustivement à l'initialisation de l'application (`LiveRegistry::cold_start`).
//!   Aucun appel système `mmap()` n'est toléré dans la boucle de traitement des requêtes HTTP (*Hot Path*).
//! - **Recherche Binaire Zéro Allocation :** L'index, sous-tranche du mapping trié par identifiant
//!   (`ID ASC`) et casté en tranche mémoire contiguë (`&[PackfileEntry]`), permet une localisation
//!   d'un fragment HTML par recherche dichotomique en $O(\log N)$ instructions CPU, sans traversée
//!   de pointeurs ni allocation dans le *heap*.
//! - **Accès Blob Zéro Copie :** `blob()` retourne une tranche empruntée directement sur le mapping —
//!   aucune copie, aucune lecture système (`pread`) sur le chemin chaud une fois `lookup()` résolu.

use std::io;
use std::os::unix::fs::FileExt;
use std::path::Path;

use crate::pack_html_format::{PackfileEntry, PackfileFooter};

const FOOTER_SIZE: usize = std::mem::size_of::<PackfileFooter>();
const ENTRY_SIZE: usize = std::mem::size_of::<PackfileEntry>();

/// Compteur d'instances vivantes — instrumentation de test exclusivement,
/// arbitrage Phase 2 (handoff-render-shell-phase2.md, design figé) :
/// instrumentation interne plutôt qu'un wrapper externe, pour ne pas forcer
/// LiveRegistry à accepter un type modifié.
///
/// Segment BSS, n'affecte pas `sizeof(PackHtmlIndex)`. `pub(crate)` — pas
/// `pub` : lu depuis le module de test de registry.rs (Jalon 2), jamais
/// depuis l'extérieur du crate. Visibilité requise uniquement pour cet
/// accès inter-module ; absente de la formulation littérale du handoff,
/// ajoutée ici parce que sans elle le pilote de test de registry.rs ne
/// compile pas (élément privé d'un autre module) — pas une extension de
/// périmètre, le minimum mécanique pour que le critère d'acceptation déjà
/// fixé soit exécutable.
#[cfg(test)]
pub(crate) static ALIVE_INSTANCES: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Lecteur d'un packfile HTML — fd conservé ouvert, mapping persistant
/// couvrant blob + index (Phase 0.B, option A).
pub struct PackHtmlIndex {
    /// fd conservé ouvert — zéro open() par requête. Jamais de seek() sur ce
    /// fd partagé : toute lecture positionnelle passe par read_at (pread(2),
    /// voir spec §6.3) — un seek() sur un fd accédé concurremment par
    /// plusieurs requêtes Tokio est une race condition (le curseur d'I/O
    /// POSIX est un état partagé mutable). Conservé au-delà de `open()`
    /// (footer déjà lu) pour Phase 3+ (pread d'émission).
    file: std::fs::File,

    /// Mapping persistant unique, `[0, footer_start)` — blob HTML puis
    /// index, jamais le footer (celui-ci est lu une fois via `read_at` à
    /// l'ouverture, il n'a pas besoin d'être adressable ensuite).
    ///
    /// `Option` plutôt que `Mmap` nu : `mmap(2)` POSIX rejette une longueur
    /// nulle (`EINVAL`) — pas une limite arbitraire contournable par un
    /// paramètre, l'absence d'un objet à mapper. `footer_start == 0`
    /// (fichier réduit au seul footer : aucun blob, aucune entrée) produit
    /// `None`. `None` représente fidèlement « pas de mémoire mappée » ;
    /// coût nul à l'exécution (Null Pointer Optimization sur `Option<Mmap>`,
    /// `Mmap` portant un pointeur non nul en interne).
    mapping: Option<memmap2::Mmap>,

    /// Offset, relatif au début du mapping, où débute l'index — c'est-à-dire
    /// la longueur du blob HTML (padding d'alignement 8B inclus). Toujours
    /// un multiple de 8 (garanti par `align8` côté écriture,
    /// `pack_html_format.rs`). Sert de borne haute à `blob()` : aucune
    /// tranche retournée par `blob()` ne peut chevaucher l'index.
    index_start: usize,

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
            .ok_or_else(|| {
                io::Error::other("overflow : entry_count * size_of::<PackfileEntry>()")
            })?;
        if footer.index_len != expected_index_len {
            return Err(io::Error::other(format!(
                "index_len ({}) incohérent avec entry_count ({}) — attendu {}",
                footer.index_len, footer.entry_count, expected_index_len
            )));
        }

        let index_start = (footer_start as u64)
            .checked_sub(footer.index_len)
            .ok_or_else(|| {
                io::Error::other("index_len dépasse la taille disponible avant le footer")
            })?;

        // Mapping persistant [0, footer_start) — blob + index en une seule
        // région. C'est cette borne (footer_start, jamais file_len), pas un
        // commentaire, qui garantit que le footer n'est jamais mappé : il a
        // déjà été consommé ci-dessus via read_at et n'a besoin d'aucune
        // adressabilité ultérieure.
        let mapping = if footer_start == 0 {
            None
        } else {
            let m = unsafe {
                memmap2::MmapOptions::new()
                    .offset(0)
                    .len(footer_start)
                    .map(&file)?
            };
            let _ = m.advise(memmap2::Advice::WillNeed); // pré-charge blob + index
            Some(m)
        };

        // Incrément #[cfg(test)] exactement au point de construction
        // réussie, juste avant le Ok(Self{...}) retourné — inline, pas de
        // fonction intermédiaire : aucune branche d'erreur ci-dessus (magic
        // invalide, version inconnue, index_len incohérent, fichier trop
        // court) ne passe par cette ligne, donc aucune n'incrémente — ces
        // branches ne créent aucune instance.
        #[cfg(test)]
        ALIVE_INSTANCES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        Ok(Self {
            file,
            mapping,
            index_start: index_start as usize,
            entry_count: footer.entry_count as usize,
        })
    }

    /// Sous-tranche du mapping correspondant à l'index physique. Tranche
    /// vide (jamais de panic) si `mapping` est `None` (fichier réduit au
    /// footer) — cast_slice sur une tranche vide est un no-op valide.
    #[inline]
    fn entries(&self) -> &[PackfileEntry] {
        match &self.mapping {
            Some(m) => bytemuck::cast_slice(&m[self.index_start..]),
            None => &[],
        }
    }

    /// Recherche O(log N). Retourne (offset, len) — jamais les octets
    /// eux-mêmes ; utiliser `blob()` pour résoudre ce couple vers une
    /// tranche mémoire.
    pub fn lookup(&self, id: i64) -> Option<(u64, u32)> {
        self.entries()
            .binary_search_by_key(&id, |e| e.id)
            .ok()
            .map(|i| {
                let e = &self.entries()[i];
                (e.offset, e.len)
            })
    }

    /// Résout `(offset, len)` — typiquement obtenu via `lookup()` — vers une
    /// tranche empruntée directement sur le mapping persistant. Zéro copie,
    /// zéro appel système.
    ///
    /// Emprunte `&self` : la tranche retournée ne peut pas survivre à
    /// `self`. En pratique `self` est possédé via `Arc<PackHtmlIndex>` par
    /// l'appelant (cf. `LiveRegistry::load`), donc la tranche reste valide
    /// aussi longtemps que cet `Arc` l'est — indépendamment de tout
    /// remplacement ultérieur de la génération publiée dans le registre
    /// (`LiveRegistry::store` ne mute jamais une instance déjà chargée).
    ///
    /// Retourne `None` si `mapping` est absent, ou si `[offset, offset+len)`
    /// déborde de la région blob (chevauche l'index ou dépasse la fin du
    /// mapping) — un tel appel ne peut provenir que d'un `(offset, len)`
    /// forgé hors de `lookup()` sur cette même génération ; ce n'est pas un
    /// cas attendu du chemin chaud mais reste géré sans panic.
    pub fn blob(&self, offset: u64, len: u32) -> Option<&[u8]> {
        let mapping = self.mapping.as_ref()?;
        let start = usize::try_from(offset).ok()?;
        let end = start.checked_add(len as usize)?;
        if end > self.index_start {
            return None;
        }
        mapping.get(start..end)
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

/// Bloc `impl Drop` entier gated — pas seulement la ligne de décrément. Un
/// `impl Drop` présent en production, même vide, désactive
/// `needs_drop::<T>() == false` et empêche certaines élisions de drop du
/// compilateur — incompatible avec l'exigence de coût nul. En production,
/// `PackHtmlIndex` garde la glue de drop par défaut : `File` et `Mmap` se
/// ferment/démappent déjà tout seuls via leur propre `Drop` — rien à
/// garantir manuellement ici, ce `Drop` ne sert que l'instrumentation,
/// jamais la libération de ressources.
#[cfg(test)]
impl Drop for PackHtmlIndex {
    fn drop(&mut self) {
        ALIVE_INSTANCES.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

// =============================================================================
// Tests — Jalon 1 + Phase 0.B
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

        // footer_start == 0 ici (blob et index tous deux vides) : aucun
        // mapping — blob() doit refléter cette absence, jamais paniquer.
        assert_eq!(index.blob(0, 1), None);

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

        let (offset, len) = index.lookup(7).unwrap();
        assert_eq!(
            index.blob(offset, len),
            Some(blob.as_slice()),
            "blob() doit retourner exactement les octets du fragment"
        );

        cleanup(&path);
    }

    // ── Plusieurs entrées : binary_search correct + blob() sur tout id présent/absent ─

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
            // Vérifie le chemin zéro-copie (blob(), via le mapping
            // persistant) plutôt que de relire uniquement le blob source en
            // mémoire du test — exercice réel du code sous test.
            assert_eq!(
                index.blob(entry.offset, entry.len),
                Some(*frag),
                "blob() incorrect pour id={}",
                entry.id
            );
        }

        for absent in [0i64, 5, 15, 25, 35, 45, 55, 999] {
            assert_eq!(
                index.lookup(absent),
                None,
                "id={absent} ne devrait jamais matcher"
            );
        }

        cleanup(&path);
    }

    // ── blob() refuse tout débordement sur l'index ──────────────────────────

    #[test]
    fn blob_rejects_ranges_overlapping_or_past_the_index() {
        // blob.len() = 19 — délibérément non multiple de 8, pour que le
        // padding d'alignement (align8, pack_html_format.rs) soit non nul et
        // que ce test distingue bien "fin réelle du blob" de "frontière
        // opposable par blob()", qui est index_start (blob arrondi à 8),
        // padding inclus. Lire dans le padding n'est pas une erreur — ce
        // sont des octets valablement mappés, simplement non significatifs ;
        // seule une plage débordant sur l'index doit être refusée.
        let blob = b"<p>seul contenu</p>".to_vec();
        assert_eq!(
            blob.len(),
            19,
            "précondition du test : longueur non multiple de 8"
        );
        let entry = PackfileEntry {
            id: 1,
            offset: 0,
            len: blob.len() as u32,
            _pad: [0u8; 4],
        };
        let path = write_synthetic_packfile("overlap", &blob, &[entry]);
        let index = PackHtmlIndex::open(&path).expect("open() doit réussir");

        let index_start = index.index_start as u32;
        assert!(
            index_start > blob.len() as u32,
            "précondition du test : padding non nul entre blob et index"
        );

        // Lecture légitime jusqu'à la frontière exacte, padding inclus.
        assert!(
            index.blob(0, index_start).is_some(),
            "toute la région [0, index_start) doit être lisible, y compris le padding"
        );

        // Un octet au-delà de index_start mord sur l'index — refusé.
        assert_eq!(
            index.blob(0, index_start + 1),
            None,
            "une plage débordant sur l'index ne doit jamais être retournée"
        );
        assert_eq!(
            index.blob(index_start as u64, 1),
            None,
            "une plage démarrant exactement sur l'index ne doit jamais être retournée"
        );

        cleanup(&path);
    }

    // ── Mapping persistant : couvre blob + index, jamais le footer ─────────
    //
    // Remplace mmap_stays_bounded_to_index_regardless_of_blob_size (ancien
    // invariant, Jalon 1) — arbitrage Phase 0.B, option A (2026-09) :
    // l'ancien invariant ("mmap borné à l'index, blob jamais mappé") est
    // explicitement celui que cette phase révise. Ce test certifie
    // l'invariant qui lui succède : mapping = [0, footer_start), donc
    // strictement le fichier moins le footer (32B) — ni plus (jamais le
    // footer), ni moins (le blob y est désormais inclus).

    #[test]
    fn mapping_covers_blob_and_index_but_never_the_footer() {
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

        // Le mapping doit couvrir exactement file_len - FOOTER_SIZE : le
        // blob (~200 MiB) et l'index (24B), jamais le footer (32B).
        let mapped_len = index.mapping.as_ref().map(|m| m.len()).unwrap_or(0);
        assert_eq!(
            mapped_len,
            (file_len as usize) - FOOTER_SIZE,
            "le mapping doit couvrir le fichier entier moins le footer"
        );

        let blob_len = written_len_for(&path);
        assert_eq!(
            index.index_start, blob_len as usize,
            "index_start doit correspondre exactement à la longueur du blob"
        );

        let (offset, len) = index.lookup(1).expect("id=1 doit être trouvé");
        assert_eq!(offset, 0);
        assert_eq!(len as u64, blob_len);

        // Lecture zéro-copie du blob complet (~200 MiB) via le mapping —
        // preuve fonctionnelle que le blob est bien adressable, pas
        // seulement que sa longueur est correcte.
        let slice = index
            .blob(offset, len)
            .expect("blob() doit résoudre la plage entière du fragment unique");
        assert_eq!(slice.len(), blob_len as usize);
        assert!(
            slice.iter().all(|&b| b == b'x'),
            "le contenu relu via blob() doit correspondre exactement au chunk écrit"
        );

        cleanup(&path);
    }

    /// Relit la longueur réelle écrite (file_len - footer - index), pour
    /// l'assertion du test précédent sans dépendre d'une variable capturée
    /// hors de sa portée.
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
        assert!(
            result.is_err(),
            "magic invalide doit produire une erreur, pas un panic"
        );

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
        assert!(
            result.is_err(),
            "version inconnue doit produire une erreur, pas un panic"
        );

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
