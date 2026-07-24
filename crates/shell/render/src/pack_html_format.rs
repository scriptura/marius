// crates/shell/render/src/pack_html_format.rs

//! Spécification physique et Layout Mémoire du Packfile HTML (`pack.bin`).
//!
//! Contrat d'encodage binaire AOT (*on-disk format*) du conteneur de fragments HTML.
//! Constitue la **source de vérité unique** partagée entre le producteur (`batch_renderer.rs`)
//! et le consommateur (*mmap* runtime, `pack_html_index.rs`).
//!
//! ## Topologie Mémoire du Fichier (*Bottom-Up Layout*)
//!
//! Contrairement aux structures à en-tête frontal, le format place son footer à la fin absolue
//! du fichier. Cette disposition permet un streaming séquentiel lors du rendu sans imposer de `Seek`
//! disk ni d'allocation intermédiaire pour calculer la taille totale du payload.
//!
//! ```text
//! +-----------------------------------------------------------------------+
//! |  HTML Blob (Fragments concaténés contigus, sans padding interne)      |
//! +-----------------------------------------------------------------------+
//! |  Padding de Remplissage (0 à 7 octets : alignement strict 8-bytes)      |
//! +-----------------------------------------------------------------------+
//! |  Index physique : PackfileEntry[] (entry_count × 24B, trié par ID ASC) |
//! +-----------------------------------------------------------------------+
//! |  PackfileFooter (32B fixe, toujours aux 32 derniers octets du fichier) |
//! +-----------------------------------------------------------------------+
//! ```
//!
//! ## Invariants & Zero-Copy Alignment
//!
//! - **Lecture Instantanée ($O(1)$ Cold Start) :** Les structures portent `#[repr(C)]`
//!   et implémentent `bytemuck::Pod` / `bytemuck::Zeroable`. La table d'index peut être projetée
//!   directement en mémoire depuis un *mmap* sous forme de tranche (`&[PackfileEntry]`) sans étape de désérialisation.
//! - **Discipline de Padding Strict :** Tous les champs de bourrage (`_pad`) sont explicites.
//!   `bytemuck::Pod` interdit la présence d'octets de padding non initialisés par le compilateur,
//!   garantissant l'absence de fuite mémoire et la Stabilité Binaire AOT entre différentes versions du compilateur.

use std::io::{BufWriter, Write};

/// Entrée d'index physique décrivant la position d'un fragment HTML dans le blob.
///
/// Alignée à 24 octets en mémoire. Structurée pour être castée sans copie depuis une région *mmap*.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct PackfileEntry {
    /// PK de l'enregistrement (i64 par convention — downcast depuis record_id).
    pub id: i64,
    /// Offset absolu du fragment dans le packfile, en octets.
    pub offset: u64,
    /// Longueur du fragment HTML en octets (u32 : max ~4 GB par fragment).
    pub len: u32,
    /// Tail padding explicite — requis par bytemuck::Pod, jamais lu.
    pub _pad: [u8; 4],
}

const _: () = assert!(
    std::mem::size_of::<PackfileEntry>() == 24,
    "PackfileEntry doit être exactement 24B (8+8+4+4)"
);

/// Footer fixe clôturant un packfile HTML — toujours les 32 derniers octets
/// du fichier. Le lecteur lit ces 32B en dernier (un seul mmap, offset connu
/// = file_len - 32), valide magic/version, puis dérive index_start =
/// file_len - 32 - index_len pour localiser l'index sans jamais avoir eu
/// besoin de le connaître pendant l'écriture.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct PackfileFooter {
    pub magic: [u8; 8], // b"MARIUSPK"
    pub version: u32,   // = 1
    pub _pad: [u8; 4],
    pub entry_count: u64,
    pub index_len: u64, // = entry_count * size_of::<PackfileEntry>() — redondant
                        // mais explicite : permet de faire évoluer la taille
                        // d'une entrée sans recalcul implicite côté lecteur.
}

const _: () = assert!(
    std::mem::size_of::<PackfileFooter>() == 32,
    "PackfileFooter doit être exactement 32B"
);

/// Arrondit `x` au prochain multiple de 8.
///
/// Même fonction que `marius_projection::align8` (store.bin) — dupliquée ici
/// plutôt que partagée : les deux formats vivent dans des crates distinctes
/// (`marius_render` vs `marius_projection`) sans dépendance dans ce sens.
#[inline(always)]
const fn align8(x: u64) -> u64 {
    (x + 7) & !7
}

/// Écrit l'index physique puis le footer à la suite du blob HTML déjà
/// streamé par un ou plusieurs appels à `BatchRenderer::render_batch`.
///
/// `blob_len` : offset final retourné par le dernier `render_batch` —
/// longueur exacte du blob déjà écrit. Requis pour insérer le padding qui
/// aligne le début de l'index sur une frontière de 8 octets : sans lui,
/// `bytemuck::from_bytes`/`cast_slice` paniquent au premier blob dont la
/// longueur n'est pas multiple de 8 (le cas général — un fragment HTML n'a
/// aucune raison d'avoir une longueur ronde).
///
/// Appelé une seule fois, après la dernière écriture de blob — jamais entre
/// deux chunks d'un même fichier. `index` doit être l'accumulation complète
/// de toutes les entrées du fichier (le caller est responsable de l'avoir
/// collecté lui-même si plusieurs `reset()` ont eu lieu entre-temps).
///
/// Invariant requis, non vérifié ici (responsabilité de l'appelant) :
/// `index` trié par `id` ASC — hérité de `dumper.rs` (`ORDER BY id ASC`).
pub fn write_packfile_footer<W: Write>(
    writer: &mut BufWriter<W>,
    blob_len: u64,
    index: &[PackfileEntry],
) -> std::io::Result<()> {
    // Padding jusqu'au prochain multiple de 8 — garantit que l'index, puis
    // le footer, démarrent tous deux sur une frontière 8B.
    let pad = align8(blob_len) - blob_len;
    if pad > 0 {
        const ZERO: [u8; 8] = [0u8; 8];
        writer.write_all(&ZERO[..pad as usize])?;
    }

    writer.write_all(bytemuck::cast_slice(index))?;

    let footer = PackfileFooter {
        magic: *b"MARIUSPK",
        version: 1,
        _pad: [0u8; 4],
        entry_count: index.len() as u64,
        index_len: std::mem::size_of_val(index) as u64,
    };
    writer.write_all(bytemuck::bytes_of(&footer))?;

    Ok(())
}
