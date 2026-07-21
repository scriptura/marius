# Runtime Data Flow — Invariants

Invariant 1
store.bin est la seule source de vérité de fetch_batch.

Invariant 2
fetch_batch ne contacte jamais PostgreSQL.

Invariant 3
pack.bin est toujours dérivé de store.bin, jamais directement de PostgreSQL.

Invariant 4
HTTP ne lit jamais store.bin.

Invariant 5
regenerate_and_swap ne parle jamais à PostgreSQL.

Invariant 6
fetch_from_pg n'est appelé que par dump_table et ingest_and_swap.

Invariant 7
Toute écriture de store.bin passe par merge_store — jamais de patch in-place, jamais d'append.

Invariant 8
Un appel à fetch_batch résout tous ses ids contre une unique version de store.bin.

Invariant 9
ingest_and_swap s'exécute toujours avant regenerate_and_swap dans un même tick, jamais l'inverse, jamais en parallèle.

Invariant 10
Un registre (StoreRegistry, LiveRegistry) ne publie jamais un fichier qui n'a pas été revalidé après écriture.

Invariant 11
Aucun composant du chemin de rendu (merge_store, merge_sweep, BatchRenderer, regenerate_and_swap) ne dépend d'un pool SQL.
