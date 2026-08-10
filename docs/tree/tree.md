marius$ tree -I "doc|target|build|artifacts|assets|logs"
.
├── Cargo.lock
├── Cargo.toml
├── crates
│   ├── core
│   │   ├── collector
│   │   │   ├── Cargo.toml
│   │   │   ├── README.md
│   │   │   └── src
│   │   │       ├── collector.rs
│   │   │       ├── lib.rs
│   │   │       └── projection.rs
│   │   ├── projection
│   │   │   ├── Cargo.toml
│   │   │   ├── README.md
│   │   │   └── src
│   │   │       ├── lib.rs
│   │   │       └── store_registry.rs
│   │   └── schema
│   │       ├── build.rs
│   │       ├── Cargo.toml
│   │       ├── README.md
│   │       ├── src
│   │       │   └── lib.rs
│   │       └── templates
│   │           ├── base.marius
│   │           ├── commerce
│   │           │   └── product_core.marius
│   │           ├── content
│   │           │   └── core.marius
│   │           └── offline
│   │               └── offline.marius
│   ├── forge
│   │   ├── bridge-forge
│   │   │   ├── Cargo.toml
│   │   │   ├── README.md
│   │   │   └── src
│   │   │       └── lib.rs
│   │   ├── db-forge
│   │   │   ├── Cargo.toml
│   │   │   ├── README.md
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
│   │   │   ├── README.md
│   │   │   └── src
│   │   │       └── lib.rs
│   │   └── guard-forge
│   │       ├── Cargo.toml
│   │       ├── README.md
│   │       └── src
│   │           └── lib.rs
│   ├── marius
│   │   ├── Cargo.toml
│   │   ├── README.md
│   │   └── src
│   │       └── lib.rs
│   └── shell
│       ├── render
│       │   ├── benches
│       │   │   ├── counting_alloc.rs
│       │   │   ├── hot_path_certify.rs
│       │   │   └── hot_path_render.rs
│       │   ├── Cargo.toml
│       │   ├── README.md
│       │   └── src
│       │       ├── batch_renderer.rs
│       │       ├── bin
│       │       │   ├── dump.rs
│       │       │   └── verify.rs
│       │       ├── dispatcher.rs
│       │       ├── dumper.rs
│       │       ├── ingest_and_swap.rs
│       │       ├── lib.rs
│       │       ├── merge_store.rs
│       │       ├── packfile_builder.rs
│       │       ├── pack_html_format.rs
│       │       ├── pack_html_index.rs
│       │       ├── regenerate.rs
│       │       ├── registry.rs
│       │       ├── store_provisioning.rs
│       │       └── sweep.rs
│       └── server
│           ├── build.rs
│           ├── Cargo.toml
│           ├── src
│           │   ├── handlers.rs
│           │   └── main.rs
│           └── tests
│               └── server_supervision_and_provisioning.rs
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
│   ├── 12_events
│   │   └── 01_notify.sql
│   ├── dml
│   │   ├── forge_test_dml.pgsql
│   │   └── master_schema_dml.pgsql
│   ├── master_init.sql
│   ├── migrations
│   ├── README.md
│   ├── tests
│   │   ├── 01_schema_and_security.sql
│   │   ├── 02_identity_logic.sql
│   │   ├── 03_content_logic.sql
│   │   ├── 04_commerce_logic.sql
│   │   ├── 05_tag_hierarchy.sql
│   │   ├── 06_rls_policies.sql
│   │   ├── 06_security_audit.sql
│   │   ├── 07_hot_audit.sql
│   │   ├── 08_rgpd_audit.sql
│   │   ├── 09_dod_hot_collision.sql
│   │   └── 10_mutation_interface.sql
│   └── tools
│       ├── extended-containment-security-matrix.md
│       ├── master-health-audit.md
│       └── meta_tooling_guide.md
├── docs
│   ├── architecture
│   │   ├── monorepo.md
│   │   ├── pipeline-mode-page-architecture.md
│   │   └── runtime-data-flow
│   │       ├── CONTRAT-implementation-phase1.md
│   │       ├── DESIGN-runtime-segment-pipeline (post-ADR-011).md
│   │       ├── DESIGN-store-registry.md
│   │       ├── DFS-phase1-reactivite-cow.md
│   │       ├── PHASE1-CLOSURE.md
│   │       └── runtime-data-flow-invariants.md
│   ├── benchs
│   │   ├── benchs-2026.08.03.md
│   │   └── benchs-2026.08.06.md
│   ├── contacts
│   │   ├── CONTRAT-implementation-multi-slot-varlena.md
│   │   ├── CONTRAT-implementation-projection-segmentee.md
│   │   └── CONTRAT-implementation-varlena-raw.md
│   ├── graveyard-of-documentations
│   │   ├── manifest-reactive-projection-OLD.md
│   │   ├── static-usage-driven-selection-pipeline-v0_2_1.md
│   │   └── static-usage-driven-selection-pipeline-v3.md
│   ├── guides
│   │   ├── fragment-forge-guide.md
│   │   ├── meta-tooling-guide.md
│   │   ├── postgres-cmd.md
│   │   ├── runtime-lifecycle-guide.md
│   │   └── scenario-ajout-champ-varlena.md
│   ├── handoffs
│   │   ├── handoff-cartographie-hyper(ADR-011).md
│   │   └── HANDOFF-js-deps-capacites-frontend.md
│   ├── manifestos
│   │   ├── article-0.md
│   │   ├── DESIGN-projection-composition.md
│   │   ├── manifest-reactive-projection.md
│   │   ├── no_std-attitude-within-marius.md
│   │   ├── structural-theory.md
│   │   └── synergy-manifeste.md
│   ├── memento.md
│   ├── postgres
│   │   ├── adr
│   │   │   ├── adr-031-Nix-vs-Docker.md
│   │   │   ├── architecture-decision-records-for-postgresql.md
│   │   │   ├── Codegen.md
│   │   │   └── PostgreSQL(OLTP)-vs-OLAP.md
│   │   └── old
│   │       └── logical-data-model.pgsql
│   ├── post-mortem
│   │   ├── marius-s-technological-frontier.md
│   │   ├── static-view-driven-data-pipeline.md
│   │   ├── when-architecture-ends-up-forgetting-its-own-past.md
│   │   └── when-a-system-becomes-harder-to-read-than-to-design.md
│   ├── prospective
│   │   ├── horizontal-scaling-strategies-for-marius.md
│   │   ├── pack_html_format-as-the-canonical-protocol-for-packfiles.md
│   │   ├── r-and-d-technical-summary-marius-engine-architecture-phase-2.md
│   │   ├── risks.md
│   │   ├── shared-memory-memorandum.md
│   │   ├── shared-memory-posix.md
│   │   ├── template-language-evolution.md
│   │   ├── towards-total-physical-symmetry.md
│   │   └── trade-off.md
│   ├── reflexivity
│   │   ├── when-architecture-ceases-to-be-a-plan-and-becomes-an-instrument-of-discovery.md
│   │   └── why-marius-could-only-emerge-outside-the-tech-world.md
│   ├── rust
│   │   ├── adr
│   │   │   ├── ADR-001-hashset-to-bit-vector.md
│   │   │   ├── ADR-002-reactive-projection-and-hybrid-state-management.md
│   │   │   ├── ADR-003-thread-alignment-projection-varlena-transport-and-thread-invariants.md
│   │   │   ├── ADR-004-normalization-of-configuration-indexes-and-memory-offsets.md
│   │   │   ├── ADR-005-HTMX-versus-Web-Components.md
│   │   │   ├── ADR-006-safeguarding-the-read-path-via-sendfile(2).md
│   │   │   ├── ADR-007-introspection-varlena-bounds.md
│   │   │   ├── ADR-008-topologie-artefact-lecture.md
│   │   │   ├── ADR-009-pages-non-adressables-pk.md
│   │   │   ├── ADR-010-chunking-of-large-varlena-objects.md
│   │   │   ├── ADR-011-projections-ordonnancees.md
│   │   │   └── ADR-rust-versus-zig.md
│   │   ├── note-post-phase-5.3-generate-main.md
│   │   └── specifications
│   │       ├── db-forge-roadmap.md
│   │       ├── db-forge-specification.md
│   │       ├── dependances-mode-page
│   │       │   ├── core-system-blueprint.md
│   │       │   ├── doc1-parser-mode-page.md
│   │       │   ├── doc2-linker-lowering.md
│   │       │   ├── doc3-orchestration-build-rs.md
│   │       │   ├── graphe-dependances-mode-page.md
│   │       │   └── mode-page-implementation-roadmap.md
│   │       ├── marius-assets-roadmap.md
│   │       ├── marius-assets-specification.md
│   │       ├── marius-compilateur-projections-html-roadmap.md
│   │       ├── marius-compilateur-projections-html-specification.md
│   │       ├── marius-merge-rcu-specification-roadmap.md
│   │       ├── marius-render-shell-roadmap.md
│   │       ├── marius-render-shell-specification.md
│   │       ├── orchestration-main-roadmap.md
│   │       ├── orchestration-main-specification.md
│   │       └── provisioning-projection-specification.md
│   └── tree
│       └── tree.md
├── README.md
└── scripts
    ├── certify_frugality.sh
    ├── profile_frugality.sh
    └── tree.sh
