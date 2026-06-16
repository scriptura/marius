// =============================================================================
// marius-db-forge · codegen/projection.rs
// Génération du stub impl Projection pour une table.
// =============================================================================

use std::fmt::Write as _;

use crate::mapping::{Column, PrimaryKey, map_type};
use crate::naming::{to_pascal, to_screaming};
use marius_fragment_forge::{FieldSpec, FieldKind, VarlenField, generate_render, generate_capacity_consts};

/// Génère le stub `impl Projection` complet pour une table.
///
/// Émet dans l'ordre :
///   1. `pub struct {Name}Projection;`
///   2. Constantes de capacité (`{NAME}_STATIC_CAP`, `_DYNAMIC_CAP`, `_TOTAL_CAP`)
///   3. `impl crate::projection::Projection for {Name}Projection { … }`
///      - type Record, type VarlenOwned
///      - fetch_batch() avec SELECT + FROM + WHERE construits depuis le schéma
///      - render() avec corps généré par Fragment-Forge
///      - artifact_path()
///
/// `varlena_join` : Option<(schema, table, fk_col)> — None si pas de JOIN.
pub fn write_projection_stub(
    out:          &mut String,
    schema:       &str,
    table:        &str,
    columns:      &[Column],
    pk:           &PrimaryKey,
    varlena:      &[VarlenField],
    varlena_join: Option<(&str, &str, &str)>,
) {
    let name      = to_pascal(&format!("{schema}_{table}"));
    let proj_name = format!("{name}Projection");
    let screaming = to_screaming(&format!("{schema}_{table}"));

    let varlen_owned_type = if varlena.is_empty() {
        "()".to_string()
    } else {
        format!("{name}VarlenOwned")
    };

    // ── Colonnes fixed-length pour le SELECT ─────────────────────────────────
    let fixed_cols: Vec<&str> = columns.iter()
        .filter(|c| map_type(&c.sql_type).is_fixed)
        .map(|c| c.name.as_str())
        .collect();

    if fixed_cols.is_empty() {
        eprintln!(
            "cargo:warning=DB-Forge [{schema}.{table}] : \
             aucune colonne fixed-length — stub incomplet généré."
        );
    }

    // ── Construction SELECT + FROM (conservé pour référence — inutilisé en Phase 2 AOT) ─
    // fetch_batch Phase 2 lit le store.bin via PackfileReader, pas via SQLx.
    // Ces variables sont préfixées _ pour supprimer les warnings du compilateur.
    let (_select, _from_clause) = if let Some((vs, vt, _fk)) = varlena_join {
        let varlena_cols: Vec<String> = varlena.iter()
            .map(|v| format!("{vt}.{}", v.name))
            .collect();
        let all_cols: Vec<String> = fixed_cols.iter()
            .map(|c| format!("{schema}.{table}.{c}"))
            .chain(varlena_cols)
            .collect();
        let from = format!(
            "{schema}.{table} LEFT JOIN {vs}.{vt} ON {schema}.{table}.{_fk} = {vs}.{vt}.{_fk}"
        );
        (all_cols.join(", "), from)
    } else {
        (fixed_cols.join(", "), format!("{schema}.{table}"))
    };

    let _where_clause = match pk {
        PrimaryKey::Single(col) => format!(
            "WHERE {schema}.{table}.{col} = ANY($1) ORDER BY {schema}.{table}.{col} ASC"
        ),
        PrimaryKey::Composite => "WHERE 1=1 /* PK composite: adapter */".to_string(),
    };

    // ── Fragment-Forge : corps render() + constantes capacité ────────────────
    // ── Construction des FieldSpecs ───────────────────────────────────────────
    // Seules les colonnes fixed-length connues de FieldKind sont incluses.
    // Les types PHASE2_ONLY (pg_lsn) ou inconnus sont silencieusement exclus.
    let field_specs: Vec<FieldSpec> = columns.iter()
        .filter(|c| map_type(&c.sql_type).is_fixed)
        .filter_map(|c| {
            FieldKind::from_sql_type(&c.sql_type).map(|kind| FieldSpec {
                name:   c.name.clone(),
                kind,
                attnum: c.attnum,
            })
        })
        .collect();

    // pk_field : &FieldSpec — contrat generate_render (signature scellée).
    // Résolution : chercher dans field_specs le champ portant le nom PK.
    // Fallback sur le premier FieldSpec si PK composite (cas dégradé).
    let pk_col_name: &str = match pk {
        PrimaryKey::Single(col) => col.as_str(),
        PrimaryKey::Composite   => fixed_cols.first().copied().unwrap_or("id"),
    };
    let pk_field: &FieldSpec = field_specs
        .iter()
        .find(|f| f.name == pk_col_name)
        .unwrap_or_else(|| {
            // Invariant : toute table avec PK Single a son champ PK dans field_specs.
            // Un panic ici indique un type PK non supporté par FieldKind (ex: uuid).
            panic!(
                "DB-Forge [{schema}.{table}]: champ PK '{pk_col_name}' \
                 absent des FieldSpecs — type PK non supporté par FieldKind. \
                 Déclarer la colonne PK avec un type fixed-length reconnu."
            )
        });

    let (static_cap, dynamic_cap, render_body) = generate_render(
        schema, table, &name,
        &field_specs,
        pk_field,
        varlena,
    );
    let cap_consts = generate_capacity_consts(&screaming, static_cap, dynamic_cap);

    // ── Émission ──────────────────────────────────────────────────────────────
    writeln!(out, "pub struct {proj_name};").unwrap();
    writeln!(out).unwrap();

    // Constantes au niveau module (pas dans le bloc impl).
    writeln!(out, "{cap_consts}").unwrap();

    // ── OnceLock statique — PackfileReader monté au premier appel de fetch_batch ──
    // Déclaration au niveau module : durée de vie 'static garantie.
    // OnceLock est thread-safe sans verrou — Mmap est Send + Sync.
    writeln!(out,
        "static {screaming}_STORE: std::sync::OnceLock<\
         marius_projection::PackfileReader<{proj_name}>> = std::sync::OnceLock::new();"
    ).unwrap();
    writeln!(out).unwrap();

    writeln!(out, "// Phase 2 AOT : _pool ignoré — lecture via OnceLock<PackfileReader>.").unwrap();
    writeln!(out, "// RLS         : voir 09_rls/01_policies.sql").unwrap();
    writeln!(out, "impl crate::projection::Projection for {proj_name} {{").unwrap();
    writeln!(out, "    type Record = {name}StorageRow;").unwrap();
    writeln!(out, "    type VarlenOwned = {varlen_owned_type};").unwrap();
    writeln!(out).unwrap();

    // ── fetch_batch (Phase 2 AOT) ─────────────────────────────────────────────
    // _pool : paramètre de trait conservé pour compatibilité de signature.
    //         Jamais utilisé en production — le pool SQLx est absent du hot path.
    //         Fail-fast : si store.bin est absent au premier appel, panic immédiat.
    //         Pas de fallback réseau — l'absence de store est une erreur fatale AOT.
    writeln!(out, "    async fn fetch_batch(").unwrap();
    writeln!(out, "        _pool: &sqlx::PgPool,").unwrap();
    writeln!(out, "        ids:   &[i64],").unwrap();
    writeln!(out, "    ) -> Result<Vec<(Self::Record, Self::VarlenOwned)>, sqlx::Error> {{").unwrap();

    if fixed_cols.is_empty() {
        writeln!(out,
            "        todo!(\"DB-Forge: aucune colonne fixed-length pour {schema}.{table}\")"
        ).unwrap();
    } else {
        // Montage du PackfileReader au premier appel — OnceLock garantit l'unicité.
        writeln!(out, "        let reader = {screaming}_STORE.get_or_init(|| {{").unwrap();
        writeln!(out,
            "            marius_projection::PackfileReader::open(&{proj_name}::store_path())"
        ).unwrap();
        writeln!(out,
            "                .expect(\"[fetch_batch:{schema}.{table}] store.bin absent \
             — exécuter marius-dump avant de démarrer le serveur\")"
        ).unwrap();
        writeln!(out, "        }});").unwrap();
        writeln!(out).unwrap();

        // Itération sur les ids demandés — lookup O(log N) par binary search.
        // Les ids absents du store sont silencieusement ignorés (enreg. supprimé).
        writeln!(out, "        let mut batch = Vec::with_capacity(ids.len());").unwrap();
        writeln!(out, "        for &id in ids {{").unwrap();
        writeln!(out, "            if let Some((record, vrefs)) = reader.lookup(id) {{").unwrap();

        if varlena.is_empty() {
            // Table sans varlena : copie du Record, VarlenOwned = ().
            writeln!(out, "                batch.push((*record, ()));").unwrap();
        } else {
            // Construction VarlenOwned depuis VarlenRefs (vues mmap → String owned).
            // to_owned() : unique allocation tolérée — bornée, isolée avant render().
            writeln!(out, "                let owned = {name}VarlenOwned {{").unwrap();
            for (i, v) in varlena.iter().enumerate() {
                writeln!(out,
                    "                    {}: vrefs.get({i}).map(str::to_owned),",
                    v.name
                ).unwrap();
            }
            writeln!(out, "                }};").unwrap();
            writeln!(out, "                batch.push((*record, owned));").unwrap();
        }

        writeln!(out, "            }}").unwrap();
        writeln!(out, "        }}").unwrap();
        writeln!(out, "        Ok(batch)").unwrap();
    }

    writeln!(out, "    }}").unwrap();
    writeln!(out).unwrap();

    // ── render() ─────────────────────────────────────────────────────────────
    let varlena_param = if varlena.is_empty() {
        "_varlena: &()".to_string()
    } else {
        format!("varlena: &{name}VarlenOwned")
    };
    writeln!(out,
        "    fn render(record: &Self::Record, {varlena_param}, buf: &mut String) {{"
    ).unwrap();
    for line in render_body.lines() {
        writeln!(out, "    {line}").unwrap();
    }
    writeln!(out, "    }}").unwrap();
    writeln!(out).unwrap();

    // ── record_id() ───────────────────────────────────────────────────────────
    // Accesseur direct du champ PK — coût zéro, inliné par LLVM.
    // Requis par BatchRenderer::render_batch pour construire PackfileEntry.id
    // sans connaissance du nom du champ PK au niveau du trait générique.
    writeln!(out, "    #[inline(always)]").unwrap();
    writeln!(out, "    fn record_id(record: &Self::Record) -> i64 {{").unwrap();
    // On retrouve la colonne d'origine dans le slice pour lire son sql_type
    let pk_column = columns
        .iter()
        .find(|c| c.name == pk_field.name)
        .expect("DB-Forge : colonne PK absente du slice columns — incohérence fetch_pk_column/fetch_columns");
    let pk_mapping = crate::mapping::map_type(&pk_column.sql_type);

    // Résolution du cast selon la taille native (zéro overhead si déjà i64)
    let pk_expr = if pk_mapping.store_type == "i64" {
        format!("record.{}", pk_field.name)
    } else {
        format!("record.{} as i64", pk_field.name)
    };
    writeln!(out, "        {}", pk_expr).unwrap();
    writeln!(out, "    }}").unwrap();
    writeln!(out).unwrap();

    // ── packfile_path() (HTML Fragments) ──────────────────────────────────────
    // Chemin statique par table — aucun paramètre record.
    // Un seul open() par batch (INV O(1) syscalls).
    writeln!(out, "    fn packfile_path() -> ::std::path::PathBuf {{").unwrap();
    writeln!(out, "        let root = std::env::var(\"MARIUS_ARTIFACTS_DIR\")").unwrap();
    writeln!(out, "            .unwrap_or_else(|_| \"artifacts\".to_string());").unwrap();
    writeln!(out,
        "        ::std::path::PathBuf::from(format!(\"{{root}}/{schema}_{table}_pack.bin\"))"
    ).unwrap();
    writeln!(out, "    }}").unwrap();
    writeln!(out).unwrap();

    // ── store_path() (Phase 1.4 : binary dump) ────────────────────────────────
    writeln!(out, "    fn store_path() -> ::std::path::PathBuf {{").unwrap();
    writeln!(out, "        let root = std::env::var(\"MARIUS_ARTIFACTS_DIR\")").unwrap();
    writeln!(out, "            .unwrap_or_else(|_| \"artifacts\".to_string());").unwrap();
    writeln!(out,
        "        ::std::path::PathBuf::from(format!(\"{{root}}/{schema}_{table}_store.bin\"))"
    ).unwrap();
    writeln!(out, "    }}").unwrap();
    writeln!(out).unwrap();

    // ── varlena_field_count() + encode_varlena() ─────────────────────────────
    // Émis uniquement si la table a des colonnes varlena.
    // Sans ces overrides, le trait applique les defaults (0 / no-op) →
    // PackfileBuilder n'écrit ni TOC ni Heap → store tronqué.
    //
    // Sentinel : offset=u32::MAX, len=0 pour None OU Some("").
    // Some("") est sémantiquement absent : écrire un slot len=0 dans le heap
    // consomme une entrée TOC pour zéro octet utile et bloque le chemin sentinel
    // côté reader (qui teste offset == u32::MAX, pas len == 0).
    if !varlena.is_empty() {
        let vf = varlena.len();

        writeln!(out, "    #[inline(always)]").unwrap();
        writeln!(out, "    fn varlena_field_count() -> u16 {{ {vf} }}").unwrap();
        writeln!(out).unwrap();

        writeln!(out, "    fn encode_varlena(").unwrap();
        writeln!(out, "        owned: &Self::VarlenOwned,").unwrap();
        writeln!(out, "        heap:  &mut Vec<u8>,").unwrap();
        writeln!(out, "        toc:   &mut Vec<marius_projection::VarlenSlot>,").unwrap();
        writeln!(out, "    ) {{").unwrap();
        for v in varlena {
            writeln!(out, "        match owned.{}.as_deref() {{", v.name).unwrap();
            writeln!(out, "            Some(s) if !s.is_empty() => {{").unwrap();
            writeln!(out, "                let offset = heap.len() as u32;").unwrap();
            writeln!(out, "                let len    = s.len() as u32;").unwrap();
            writeln!(out, "                heap.extend_from_slice(s.as_bytes());").unwrap();
            writeln!(out, "                toc.push(marius_projection::VarlenSlot {{ offset, len }});").unwrap();
            writeln!(out, "            }}").unwrap();
            writeln!(out, "            _ => toc.push(marius_projection::VarlenSlot {{ offset: u32::MAX, len: 0 }}),").unwrap();
            writeln!(out, "        }}").unwrap();
        }
        writeln!(out, "    }}").unwrap();
    }

    writeln!(out, "}}\n").unwrap();
}
