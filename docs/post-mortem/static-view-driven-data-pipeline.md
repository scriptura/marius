# Post-Mortem Architectural : Inversion de Projection Statique (Spécification v0.1)

## 1. Description du Paradigme Déprécié (Le "Gouvernement des Vues")

La spécification v0.1 introduisait le concept d'**Inversion de Projection Statique**. Dans ce modèle, le fichier de présentation (le template `.marius`) n'était pas le récepteur final de la donnée, mais le **contrat maître de compilation**. C'est la vue qui dictait la structure physique des données depuis le disque PostgreSQL jusqu'à l'espace utilisateur Rust.

L'intention initiale était de saturer le bus mémoire utile en éliminant à la compilation (via un mécanisme de _Tree-Shaking_) tout octet non explicitement déclaré dans l'interface utilisateur.

---

## 2. Invariants Techniques Tentés (L'Idéal Matériel)

L'architecture v0.1 tentait de figer trois invariants structurels stricts :

- **Le Layout C-Compatible Déduit :** Le script de build analysait le template pour générer une structure Rust `#[repr(C)]` ou `#[repr(C, packed)]` (Artefact A), forçant l'usage exclusif de types primitifs ou de segments contigus fixes (`[u8; N]`).

- **La Requête I/O Synchrone Binaire :** PostgreSQL devait exporter ses données via le format binaire natif (`COPY ... WITH (FORMAT BINARY)`) pour s'aligner exactement, au bit près, sur la structure mémoire générée en Rust (Artefact B).

- **La Normalisation des Index :** Pour préserver la lisibilité humaine, les fichiers YAML de configuration externe utilisaient une indexation base-1. Le compilateur AOT normalisait immédiatement ces valeurs en indexation base-0 pour la génération des pointeurs, tableaux et offsets internes.

- **La Séparation Taxonomie / Algorithme :** L'organisation physique des répertoires de templates documentait uniquement la taxonomie métier. Elle n'intervenait pas dans l'exécution ; la résolution des routes au runtime incombait exclusivement au dispatching linéaire des `Opcodes` (Artefact C).

- **Le Rendu par Copie Aveugle :** Le processus de lecture (`render_batch_pure`) éliminait toute allocation sur le tas et toute logique conditionnelle, se contentant d'itérer des instructions bas niveau `ptr::copy_nonoverlapping` entre un fichier `mmap` (mémoire partagée POSIX) et le buffer réseau.

---

## 3. Causes de l'Échec Absolu (Impasse Systémique)

Bien que l'objectif d'allocation nulle au runtime ait été atteint, ce modèle a été déprécié en raison d'un **couplage bilatéral destructeur** et d'un risque majeur de dérive fonctionnelle.

### A. Brisure de la Souveraineté du Schéma

Faire dépendre la structure binaire de la base de données de l'état d'un fichier de template HTML inversait la hiérarchie logique d'un système d'information. Une modification mineure dans l'interface utilisateur (suppression d'un champ textuel dans un bloc) reconfigurait le layout binaire de l'Artefact A, modifiait la signature de la requête SQL binaire (Artefact B), et exigeait une réplication de données asynchrone modifiée dans le segment `mmap`.

### B. Rigidité face au Changement (Fragilité AOT)

L'obligation d'aligner parfaitement les types PostgreSQL avec des structures `#[repr(C, packed)]` via des paires d'offsets interdisait toute flexibilité. Le pipeline était incapable de tolérer des variations de données sans recalculer l'intégralité du graphe d'instructions des `Opcodes`.

---

## 4. Le Pivot Architectural (La Solution v1)

La spécification v0.1 a été abandonnée au profit d'une **optimisation par l'usage**.

Le "gouvernement des vues" (où la vue impose sa structure) est remplacé par la **souveraineté du schéma**. C'est désormais le dictionnaire de données PostgreSQL qui régit les structures. L'optimisation matérielle recherchée (le _Tree-Shaking_ des octets morts sur le bus mémoire) n'est plus obtenue en modifiant le layout binaire de la base, mais en limitant sélectivement les projections générées de manière préemptive.

### Ce qui est définitivement éliminé :

1. **Le pipeline de transport binaire personnalisé (`COPY BINARY` + `mmap` POSIX) :** Remplacé par l'écriture préemptive d'artéfacts HTML complets sur le disque, laissant le noyau Linux optimiser la mémoire via son propre cache page.
2. **L'exécution par Opcodes au runtime :** Remplacée par la génération de code Rust natif via `build.rs` lors de la compilation de l'application, transformant le DSL `.marius` en instructions machines figées sans couche logicielle intermédiaire.

### Ce qui survit dans l'ADN de Marius :

- L'invariant de l'**AOT absolu** : la vue reste résolue au build-time.
- La **normalisation stricte de l'indexation** (l'humain écrit en base-1, la machine s'exécute en base-0) pour éviter les erreurs d'offsets.
- La **séparation taxonomique** : l'arborescence des dossiers sert de documentation métier, jamais de moteur de routage dynamique au runtime.
