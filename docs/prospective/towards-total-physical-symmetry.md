# Document de Prospective : Vers la Symétrie Physique Totale (Niveau 2+)

## 1. Rectification de l'Invariant : Du Zéro-Copy au Zéro-Indirection

L'audit technique a révélé une limite physique : le protocole **pgwire** (réseau) de PostgreSQL impose une couche de transport. Les données sont sérialisées avec des préfixes de longueur, ce qui interdit le "casting" direct des buffers réseau vers les structures Rust `#[repr(C)]`.

### L'état actuel (Niveau 1) : Zéro-Indirection

- **Mécanique :** `SQL Buffer -> SQLx Parser -> Rust Struct`.
- **Gain :** Suppression du mapping sémantique (ORM), de la sérialisation intermédiaire (JSON/Bincode) et des allocations d'objets sur le tas.
- **Coût résiduel :** Copie mémoire champ par champ depuis le buffer `pgwire`.

## 2. Perspective A : La Voie de la Mémoire Partagée (Shared Memory)

Cible : Déploiement en mode "Appliance" (PostgreSQL et Marius co-localisés sur le même OS).

### Architecture

L'objectif est d'accéder aux données là où elles résident (les _Shared Buffers_ de Postgres) sans passer par la pile réseau de l'OS.

1. **Extension `pgrx` :** Développement d'un plugin natif chargé dans PostgreSQL.
2. **Segment SHM (Shared Memory) :** L'extension écrit les mises à jour directement dans un segment de mémoire partagée (`POSIX shm`), formaté exactement selon le layout `#[repr(C)]` produit par la **DB-Forge**.
3. **Accès Marius :** Le Core Marius effectue un `mmap` sur ce segment.
4. **Consommation :** L'accès à une entité devient une simple déférence de pointeur.

- _Complexité :_ $O(1)$.
- _Latence :_ ~10-50 nanosecondes (latence RAM).

## 3. Perspective B : La Voie du Flux Binaire (Logical Decoding)

Cible : Performance extrême sur infrastructure distribuée.

### Architecture

Remplacer le protocole de messagerie `pgwire` par un flux de réplication binaire "Raw-to-Raw".

1. **Output Plugin Custom :** Un plugin de décodage logique (en C ou Rust) s'abonne au WAL (Write-Ahead Log) de PostgreSQL.
2. **Transpilation Binaire :** Au lieu de générer du SQL ou du JSON, le plugin émet un flux d'octets qui est l'image exacte du layout mémoire Rust.
3. **Réception Core :** Marius écoute le flux de réplication. Le buffer TCP entrant est traité comme une slice de mémoire brute (`std::slice::from_raw_parts`) et "casté" instantanément.

## 4. Roadmap d'Évolution de la Forge

Grâce à l'**Article Zéro**, le passage à ces technologies ne nécessite pas de refonte du Core, mais une évolution de l'outillage :

| Étape        | Transport        | Rôle de la Forge                                           | Statut          |
| ------------ | ---------------- | ---------------------------------------------------------- | --------------- |
| **Niveau 1** | SQLx / TCP       | Génération de `query_as!` et structs `#[repr(C)]`.         | **Actuel**      |
| **Niveau 2** | Logical Decoding | Génération du plugin de décodage PostgreSQL + Reader Rust. | **Recherche**   |
| **Niveau 3** | Shared Memory    | Génération de l'extension `pgrx` + Mapping `mmap`.         | **Prospective** |

## 5. Synthèse pour l'Avenir

Le maintien de la **DB-Forge** comme pivot central permet de rester agnostique vis-à-vis du transport. Que l'on utilise SQLx aujourd'hui ou la mémoire partagée demain, le Core Marius reste identique dans sa logique de projection. L'effort d'ingénierie futur doit se concentrer sur la suppression des interruptions CPU liées au parsing réseau pour atteindre la **Symétrie Physique Totale**.
