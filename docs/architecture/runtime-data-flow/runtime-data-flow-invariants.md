# Runtime Data Flow — Invariants

## Projection store

**Invariant 1**  
`store.bin` est une projection DOD persistée de PostgreSQL ; il n'est pas
une source de données pour la régénération HTML.

**Invariant 2**  
`fetch_from_pg` est le chemin de construction / mise à jour du `store.bin`.
Toute écriture persistante du store respecte le pipeline `merge_store` puis
publication du `StoreRegistry`.

**Invariant 3**  
`fetch_batch` utilisé par la régénération HTML ne lit jamais `store.bin`.

## Projection HTML

**Invariant 4**  
`regenerate_and_swap` récupère directement auprès de PostgreSQL les lignes
correspondant au delta d'IDs qui lui est fourni.

**Invariant 5**  
`regenerate_and_swap` ne reconstruit jamais le pack HTML à partir du
`store.bin`.

**Invariant 6**  
La régénération HTML est incrémentale : `ids` représente le delta du tick ;
les entités absentes du delta sont conservées par `merge_sweep` depuis le
packfile actuellement servi.

**Invariant 7**  
Une suppression est représentée dans le `DeltaBatch` par l'absence de l'ID
dans le résultat de `P::fetch_batch`, matérialisée par une
`DeltaEntry { offset: 0, length: 0 }`.

## Atomicité / publication

**Invariant 8**  
`final_path` n'est jamais ouvert en écriture par `apply_merge_io_sync`.
Toute nouvelle génération est écrite dans un `.tmp`, rendue durable, puis
publiée par `rename` atomique.

**Invariant 9**  
`LiveRegistry` ne publie la nouvelle génération qu'après succès complet de
la fusion, de la durabilité, du `rename` et de la réouverture du packfile par
`PackHtmlIndex::open`.

**Invariant 10**  
Un échec avant le `rename` laisse le packfile actuellement servi intact ;
l'ancien `Arc<PackHtmlIndex>` reste publié.

## Séparation des responsabilités

**Invariant 11**  
HTTP ne lit jamais `store.bin` : le chemin de lecture HTTP passe exclusivement
par le `LiveRegistry` et le pack HTML.

**Invariant 12**  
Le chemin synchrone `apply_merge_io_sync` et le noyau `merge_sweep` ne
dépendent d'aucun pool SQL ni runtime async. La récupération PostgreSQL est
isolée dans `fetch_delta_batch`.

**Invariant 13**  
Le `store.bin` et le `pack.bin` sont deux artefacts distincts, produits par
deux pipelines de projection distincts. Aucun des deux ne constitue une
dépendance de lecture de l'autre.