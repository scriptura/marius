```
marius$ tree -I "docs|doc|README.md|target|logs|tests|artifacts|archives"
.
├── Cargo.lock
├── Cargo.toml
├── crates
│   ├── core
│   │   ├── collector
│   │   │   ├── Cargo.toml
│   │   │   └── src
│   │   │       ├── collector.rs
│   │   │       ├── lib.rs
│   │   │       └── projection.rs
│   │   ├── projection
│   │   │   ├── Cargo.toml
│   │   │   └── src
│   │   │       └── lib.rs
│   │   └── schema
│   │       ├── build.rs
│   │       ├── Cargo.toml
│   │       ├── src
│   │       │   └── lib.rs
│   │       └── templates
│   │           ├── base.marius
│   │           ├── commerce
│   │           │   └── product_core.marius
│   │           └── content
│   │               └── core.marius
│   ├── forge
│   │   ├── bridge-forge
│   │   │   ├── Cargo.toml
│   │   │   └── src
│   │   │       └── lib.rs
│   │   ├── db-forge
│   │   │   ├── Cargo.toml
│   │   │   └── src
│   │   │       ├── codegen
│   │   │       │   ├── collector.rs
│   │   │       │   ├── from_impl.rs
│   │   │       │   ├── mod.rs
│   │   │       │   ├── projection.rs
│   │   │       │   ├── row.rs
│   │   │       │   ├── storage.rs
│   │   │       │   └── varlen.rs
│   │   │       ├── introspect.rs
│   │   │       ├── lib.rs
│   │   │       ├── mapping.rs
│   │   │       ├── naming.rs
│   │   │       ├── registry.rs
│   │   │       └── validate.rs
│   │   ├── fragment-forge
│   │   │   ├── Cargo.toml
│   │   │   └── src
│   │   │       └── lib.rs
│   │   └── guard-forge
│   │       ├── Cargo.toml
│   │       └── src
│   │           └── lib.rs
│   ├── marius
│   │   ├── Cargo.toml
│   │   └── src
│   │       └── lib.rs
│   └── shell
│       ├── render
│       │   ├── benches
│       │   │   ├── counting_alloc.rs
│       │   │   ├── hot_path_certify.rs
│       │   │   └── hot_path_render.rs
│       │   ├── Cargo.toml
│       │   └── src
│       │       ├── batch_renderer.rs
│       │       ├── bin
│       │       │   ├── dump.rs
│       │       │   └── verify.rs
│       │       ├── dispatcher.rs
│       │       ├── dumper.rs
│       │       ├── lib.rs
│       │       ├── packfile_builder.rs
│       │       ├── pack_html_format.rs
│       │       ├── pack_html_index.rs
│       │       ├── regenerate.rs
│       │       ├── registry.rs
│       │       └── sweep.rs
│       └── server
│           ├── Cargo.toml
│           └── src
│               ├── handlers.rs
│               └── main.rs
├── db
│   ├── 00_infra
│   │   ├── 01_bootstrap.sql
│   │   ├── 02_extensions.sql
│   │   └── 03_schemas.sql
│   ├── 01_meta
│   │   ├── 01_tables.sql
│   │   ├── 02_functions.sql
│   │   └── 03_views.sql
│   ├── 02_identity
│   │   ├── 01_components.sql
│   │   └── 02_systems.sql
│   ├── 03_geo
│   │   ├── 01_components.sql
│   │   └── 02_systems.sql
│   ├── 04_org
│   │   ├── 01_components.sql
│   │   └── 02_systems.sql
│   ├── 05_content
│   │   ├── 01_components.sql
│   │   └── 02_systems.sql
│   ├── 06_commerce
│   │   ├── 01_components.sql
│   │   └── 02_systems.sql
│   ├── 07_cross_fk
│   │   └── 01_constraints.sql
│   ├── 08_dcl
│   │   ├── 01_grants.sql
│   │   └── 02_secdef.sql
│   ├── 09_rls
│   │   └── 01_policies.sql
│   ├── 10_meta_seed
│   │   └── 01_manifest.sql
│   ├── 11_audit
│   │   ├── 01_v_performance_sentinel.sql
│   │   └── 02_v_master_health_audit.sql
│   ├── dml
│   │   ├── forge_test_dml.pgsql
│   │   ├── master_schema_dml.pgsql
│   │   └── triggers_notify_dml.sql
│   ├── master_init.sql
│   ├── migrations
│   │   └── 01_add_walsn.sql
│   └── tools
│       ├── extended-containment-security-matrix.md
│       ├── master-health-audit.md
│       └── meta_tooling_guide.md
└── scripts
    ├── certify_frugality.sh
    └── profile_frugality.sh
```

```
marius/docs$ tree -I "implementation-history"
.
├── guides
│   ├── guide-cycle-de-vie-runtime.md
│   ├── guide-fragment-forge.md
│   ├── meta-tooling-guide.md
│   ├── postgres-cmd.md
│   └── scenario-ajout-champ-varlena.md
├── manifestos
│   ├── article-0.md
│   ├── manifest-reactive-projection.md
│   ├── no_std-attitude-within-marius.md
│   ├── structural-theory.md
│   └── synergy-manifeste.md
├── memento.md
├── postgres
│   ├── adr
│   │   ├── adr-031-Nix-vs-Docker.md
│   │   ├── architecture-decision-records-for-postgresql.md
│   │   ├── Codegen.md
│   │   └── PostgreSQL(OLTP)-vs-OLAP.md
│   └── old
│       └── logical-data-model.pgsql
├── post-mortem
│   ├── graveyard-of-documentations
│   │   ├── manifest-reactive-projection-OLD.md
│   │   ├── static-usage-driven-selection-pipeline-v0_2_1.md
│   │   └── static-usage-driven-selection-pipeline-v3.md
│   ├── marius-s-technological-frontier.md
│   ├── monorepo.md
│   ├── Rust-versus-Zig.md
│   ├── static-view-driven-data-pipeline.md
│   └── when-architecture-ends-up-forgetting-its-own-past.md
├── prospective
│   ├── horizontal-scaling-strategies-for-marius.md
│   ├── pack_html_format-as-the-canonical-protocol-for-packfiles.md
│   ├── r-and-d-technical-summary-marius-engine-architecture-phase-2.md
│   ├── risks.md
│   ├── shared-memory-memorandum.md
│   ├── shared-memory-posix.md
│   ├── template-language-evolution.md
│   ├── towards-total-physical-symmetry.md
│   └── trade-off.md
├── reflexivity
│   ├── when-architecture-ceases-to-be-a-plan-and-becomes-an-instrument-of-discovery.md
│   └── why-marius-could-only-emerge-outside-the-tech-world.md
├── rust
│   ├── adr
│   │   ├── ADR-001-hashset-to-bit-vector.md
│   │   ├── ADR-002-reactive-projection-and-hybrid-state-management.md
│   │   ├── ADR-003-thread-alignment-projection-varlena-transport-and-thread-invariants.md
│   │   ├── ADR-004-normalization-of-configuration-indexes-and-memory-offsets.md
│   │   ├── ADR-005-HTMX-versus-Web-Components.md
│   │   ├── ADR-006-safeguarding-the-read-path-via-sendfile(2).md
│   │   ├── ADR-007-introspection-varlena-bounds.md
│   │   ├── ADR-008-topologie-artefact-lecture.md
│   │   └── ADR-009-pages-non-adressables-pk.md
│   ├── architecture
│   │   ├── architecture-pipeline-mode-page.md
│   │   ├── core-system-blueprint.md
│   │   ├── doc1-parser-mode-page.md
│   │   ├── doc2-linker-lowering.md
│   │   ├── doc3-orchestration-build-rs.md
│   │   ├── graphe-dependances-mode-page.md
│   │   └── roadmap-mode-page-implementation.md
│   ├── note-post-phase-5.3-generate-main.md
│   └── specifications
│       ├── marius-merge-rcu-spec-roadmap.md
│       ├── roadmap-db-forge.md
│       ├── roadmap-marius-compilateur-projections-html.md
│       ├── roadmap-marius-render-shell.md
│       ├── roadmap-orchestration-main.md
│       ├── specification-db-forge.md
│       ├── specification-marius-compilateur-projections-html.md
│       ├── specification-marius-render-shell.md
│       ├── specification-orchestration-main.md
│       └── specification-provisioning-projection.md
└── tree.md
```
