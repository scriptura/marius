// =============================================================================
// crates/shell/render/src/pack_html_format.rs
//
// Source de vérité unique du format on-disk du packfile HTML — spec
// specification-marius-render-shell.md §3. Importé par batch_renderer.rs
// (écriture) et pack_html_index.rs (lecture). Aucun des deux ne redéfinit
// ces types — même discipline que PackfileStoreHeader dans marius_projection
// (format store.bin) : un seul endroit définit le format, deux le consomment.
//
// Format on-disk (footer en fin de fichier, pas en tête — le streaming par
// BatchRenderer::render_batch ne connaît jamais la longueur totale du blob
// par avance ; imposer un header obligerait un Seek ou un double parcours) :
//
//   [ HTML blob, fragments concatenés, sans padding         ]
//   [ padding 0..7B — aligne le début de l'index sur 8B     ]
//   [ PackfileEntry[], entry_count × 24B, id ASC             ]
//   [ PackfileFooter, 32B fixe, toujours en dernier          ]
// =============================================================================

use std::io::{BufWriter, Write};

/// Entrée d'index physique pour un fragment HTML dans le packfile.
///
/// #[repr(C)] + bytemuck::Pod/Zeroable : castable directement depuis un mmap
/// au moment de la lecture (cold start du Render Shell), zéro désérialisation.
/// _pad explicite : bytemuck::Pod interdit tout padding non initialisé —
/// même discipline que PackfileStoreHeader/VarlenSlot (marius_projection).
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
