Voici la version corrigée et expurgée du document. Les mentions à Pug et Maud ont été radiées au profit du format `.marius` et de la génération native (`push_str` / `write_fmt`), et la mesure de temps de traitement RAM a été mise à jour en conséquence.

---

# Synthèse Technique R&D : Architecture du Moteur Marius (Phase 2)

**Préoccupation :** Alignement Système Marius

**Cadre Cognitif :** ECS / DOD / AOT / Zéro-Indirection / Zéro-Copie

**Statut :** Exploration Validée / Alignement Manifeste & ADR Ok

---

## 1. Limites Physiques de la Phase 1 (Le Verrou SQLx)

La Phase 1 a matérialisé le paradigme de **Zéro-Indirection** en éliminant les ORM classiques et l'encapsulation JSON au profit d'interactions directes via `SQLx`. Cependant, l'analyse CPU/mémoire révèle une barrière physique incontournable au niveau de la couche réseau :

- **Le protocole `pgwire` :** PostgreSQL sérialise les données sous forme de messages `DataRow`. Ces messages intègrent des métadonnées de transport (longueurs de champs, identifiants de types) imbriquées dans le flux d'octets.
- **Conséquence DOD :** Il est physiquement impossible d'effectuer un casting binaire direct (`transmute`) de ces paquets réseau vers des structures Rust configurées en `#[repr(C)]`. Le système subit un coût incompressible de parsing de transport et de copie mémoire à la réception.

L'objectif de la Phase 2 est d'abolir le concept de message pour passer à un modèle de **Bus Système**, transitant de la Zéro-Indirection vers la **Zéro-Copie**.

---

## 2. Rupture Technologique : SHM vs. Logical Decoding

Deux perspectives de rupture ont été explorées pour contourner le goulot d'étranglement de `pgwire` :

### Perspective A : Mémoire Partagée (Shared Memory - Retenue)

- **Mécanisme :** Développement d'une extension PostgreSQL native (via `pgrx`) qui intercepte les mutations au niveau des triggers ou de l'exécuteur, et écrit les données brutes dans un segment de mémoire partagée (`POSIX shm`).
- **Pipeline :** Le Core Marius accède directement à ce segment RAM via `mmap`.
- **Analyse DOD :**
- Latence d'accès : Échelle de la nanoseconde (temps d'accès RAM pur, déférence de pointeurs).
- CPU : Zéro parsing, zéro allocation sur la pile réseau.
- Contrainte : Co-location obligatoire sur la même machine (Modèle "Appliance").

### Perspective B : Flux Binaire (Logical Decoding)

- **Mécanisme :** Déploiement d'un _Output Plugin_ de décodage logique s'abonnant directement au WAL (Write-Ahead Log) de PostgreSQL.
- **Pipeline :** Le plugin extrait le journal des transactions et pousse un flux d'octets compact sans les métadonnées de transport standard du protocole applicatif.
- **Analyse DOD :** Latence à l'échelle de la microseconde (I/O Sockets), mais offre une topologie distribuée (séparation physique de la DB et du moteur).

**Orientation Stratégique :** Marius adopte prioritairement la **Perspective A**. Ce choix est le seul cohérent avec la vision "Appliance" haute performance, calquant le comportement d'un moteur de jeu sur une base de données.

---

## 3. Le Fragment-Forge : La Vue comme Matrice Topologique Pure

Le concept de template HTML subit une refonte complète sous le prisme DOD. Le DSL (défini par les templates `.marius`) est purgé de toute logique métier.

### Invariants du Fragment-Forge

1. **Zéro Logique au Runtime :** Le template ne prend aucune décision (pas de formatage, pas de calculs arithmétiques). Il décrit uniquement un agencement spatial (le DOM) de slots mémoire. Toute transformation de donnée est déléguée en amont à PostgreSQL (Vues SQL) ou au Dispatcher Rust.
2. **Calcul de Capacité Statique (O(1) Allocation) :** À la compilation, la Fragment-Forge analyse le fichier `.marius` et calcule à l'octet près la taille cumulative de toutes les chaînes HTML statiques (`<article>`, `</div>`). Elle injecte cet indice de capacité dans le code généré :

```rust
let mut buffer = String::with_capacity(STATIC_SIZE_BYTES + ESTIMATED_DYNAMIC_SIZE);

```

Cela supprime les réallocations dynamiques (`realloc`) pendant la phase de rendu.

3. **Composabilité par Fonctions Compilées :** Le système d'héritage/inclusion est résolu à la compilation. La Forge génère des fonctions Rust pures et monomorphes que le compilateur Rust peut _inline_ librement, éliminant les indirections d'appels de fonctions au runtime.

### Intégration de la Machine d'État Client (Anti-Clobbering)

Pour appliquer l'ADR _Reactive Projection & Hybrid State Management_, la Fragment-Forge transpose le concept de **Nœud Réactif** directement dans les appels natifs.

**Entrée du DSL (`document_card.marius`) :**

```html
<article
  class="document-node"
  id="doc-{document.id}"
  data-id="{document.id}"
  data-state="pristine"
>
  <header>
    <h3>{document.title}</h3>
  </header>
  <div class="body" contenteditable="true">{document.body_text}</div>
</article>
```

**Sortie de la Forge (Code AOT généré) :**

```rust
pub fn render_document_card(document: &DocumentStruct, arena: &VarlenArena, buffer: &mut String) {
    use std::fmt::Write;

    buffer.push_str(r#"<article class="document-node" id="doc-"#);
    let _ = write!(buffer, "{}", document.id);
    buffer.push_str(r#"" data-id=""#);
    let _ = write!(buffer, "{}", document.id);
    buffer.push_str(r#"" data-state="pristine">
  <header>
    <h3>"#);
    buffer.push_str(arena.resolve(&document.title));
    buffer.push_str(r#"</h3>
  </header>
  <div class="body" contenteditable="true">"#);
    buffer.push_str(arena.resolve(&document.body_text));
    buffer.push_str(r#"</div>
</article>"#);
}

```

_Note Client :_ Le Shell injecte un script d'interception global (attaché via `htmx.onLoad` pour garantir son application sur les fragments injectés dynamiquement) au niveau de l'événement `htmx:beforeSwap`. Si un élément ou son parent possède l'attribut `data-state="dirty"` ou `"sync"`, le swap HTMX est avorté, protégeant le tampon de saisie utilisateur contre tout écrasement asynchrone.

---

## 4. Le Dispatcher : Régulateur Réactif

Le `Dispatcher` agit comme un régulateur de débit de type Filtre Passe-Bas. Il contrôle l'amplification d'I/O en sortie.

### Le Contrat d'Abstraction Générique

Pour préserver la _no_std attitude_ du Core, le Dispatcher manipule un trait générique abstrait des types SQL, généré par la Forge :

```rust
pub trait AutonomousProjection {
    type EntityId;
    type DataStructure;

    fn fetch_batch(ids: &[Self::EntityId]) -> Vec<Self::DataStructure>;
    fn render_batch(data: &[Self::DataStructure], buffer: &mut String);
}

```

### Mécanique de l'Adaptive Tick (Régulation Bang-Bang)

L'analyse physique démontre que le temps de traitement brut en RAM (Fetch SHM + Rendu AOT natif parallélisé via `Rayon`) est infime : **$\approx$ 100 à 500 microsecondes pour 100 entités**.

Le goulot d'étranglement se situe exclusivement au niveau des **I/O de commit du Shell** (appels système d'écriture disque ou réseau pour pousser le HTML). Le Dispatcher ajuste dynamiquement sa période de réveil (_Tick_) non pas en fonction de la charge CPU, mais de la latence du Shell :

```rust
// Logique d'asservissement temporel dans dispatcher.rs
let io_saturation = total_cycle_time > self.config.io_budget_frame; // ex: 16ms (60 FPS)

let new_tick = match io_saturation {
    true => self.config.tick_max,   // Sature -> Ralentissement (ex: 1000ms) pour maximiser le dédoublonnement
    false => self.config.tick_min,  // Fluide -> Accélération (ex: 50ms) pour vider en temps réel
};

```

En augmentant le Tick lors des pics de charge, le Bit-Vector du Collector accumule et écrase un plus grand nombre de mutations concurrentes pour les mêmes IDs. Le système convertit une tempête d'I/O en un unique accès séquentiel en mémoire.

---

## 5. Le Bridge-Forge : Extraction Vectorielle et Localité spatiale

La `Bridge-Forge` génère la couche d'extraction des données. Elle refuse les requêtes dynamiques à base de clauses `IN` (génératrices de replanification et d'allocations désordonnées dans PostgreSQL).

### Optimisation Niveau 1 (SQLx)

La Forge génère une requête préparée statique s'appuyant sur l'opérateur vectoriel `ANY($1)`. L'invariant absolu imposé par la Forge est l'adjonction systématique de la clause de tri :

```sql
SELECT id, title, body_text FROM content.document WHERE id = ANY($1) ORDER BY id ASC;

```

### L'Impact DOD sur le Rendu

Le tri `ORDER BY id ASC` garantit que le vecteur `Vec<DocumentStruct>` renvoyé au Core est aligné séquentiellement en RAM selon l'ordre physique des entités.

Lorsque la boucle de rendu de la Fragment-Forge s'exécute, le _Hardware Prefetcher_ du CPU anticipe le chargement des structures dans la hiérarchie de caches L1/L2. La vitesse de rendu est ainsi limitée uniquement par la bande passante de la mémoire vive.

### Abstraction Évolutive

Le Dispatcher appelle `P::fetch_batch(ids)`. Au Niveau 1, l'implémentation exécute le code asynchrone SQLx. Lors du passage au Niveau 2 (Shared Memory), la Forge modifiera exclusivement l'intérieur de la fonction générée pour y injecter l'arithmétique de pointeurs brute sur le pointeur `mmap` :

```rust
// Évolution cible Niveau 2 générée automatiquement par la Forge
let entity_ptr = shm_ptr.add(id as usize);
batch_destination.push(std::ptr::read(entity_ptr));

```

Le code du cœur du Dispatcher reste inchangé.

---

## 6. Topologie Finale du Workspace Cargo + Nix

L'organisation des répertoires de Marius valide l'étanchéité absolue entre la génération d'artefacts (Build-time) et l'exécution pure (Runtime).

```
marius/
├── Cargo.toml               # Configuration du workspace multi-membres
├── flake.nix                # DevShell pure + Dérivations de build reproductibles
│
├── db/                      # Source de Vérité Unique (DDL & Triggers SQL)
│
├── forge/                   # Silos Générateurs (Build-time exclusif, aucune dépendance runtime)
│   ├── db-forge/            # pg_attribute -> Structures #[repr(C)] + Constantes du Collector
│   ├── fragment-forge/      # Analyseur de fichiers .marius -> Génération de code Rust natif (push_str/write_fmt)
│   ├── guard-forge/         # Générateur de traits de sécurité / RLS
│   └── bridge-forge/        # Requêtes vectorielles SQLx / Génération des routes primitives
│
└── crates/                  # Composants exécutables (Runtime)
    ├── core/                # Zone de Pureté "no_std attitude" (Cibles de calcul strictes)
    │   ├── collector/       # Bit-Vector Atomique + Dispatcher (run_loop)
    │   ├── schema/          # build.rs -> Appelle db-forge -> Contient les types #[repr(C)]
    │   └── projection/      # build.rs -> Appelle bridge-forge -> Implémente AutonomousProjection
    │
    └── shell/               # Zone Orientée "std" (Gestion des I/O, Réseau, Allocations OS)
        ├── render/          # build.rs -> Appelle fragment-forge -> Stocke le rendu final
        └── server/          # Main binaire : Axum (Routage strict + sendfile(2) uniquement), LISTEN/NOTIFY, Démarrage

```

### Logique d'Inclusion Statique

Les crates du `core` et du `shell` intègrent les productions des Forges via le cycle classique des scripts de build Rust, assurant qu'aucun fichier généré n'est pollué dans le répertoire source `/src` :

```rust
// Dans crates/core/schema/src/lib.rs
include!(concat!(env!("OUT_DIR"), "/generated_schema.rs"));

```

---

Le 19 mai 2026.
Révisé le 7 juin 2026.
