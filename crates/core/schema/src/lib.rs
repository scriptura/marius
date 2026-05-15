// marius-schema
// Ce crate ne contient pas de code écrit à la main.
// Toutes les structs, statics et stubs Projection sont générés par DB-Forge
// via build.rs → $OUT_DIR/generated_schema.rs.

// Réexporte le trait Projection pour que les implémentations générées
// puissent y accéder via `crate::projection::Projection`.
pub mod projection {
    pub use marius_collector::Projection;
}

// Réexporte le Collector pour les statics générés.
pub mod collector {
    pub use marius_collector::Collector;
}

// Point d'entrée de la génération.
// Le compilateur voit toutes les structs et implémentations comme
// si elles étaient écrites ici.
include!(concat!(env!("OUT_DIR"), "/generated_schema.rs"));
