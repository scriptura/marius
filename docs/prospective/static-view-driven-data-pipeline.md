# Spécification Architecturale : Inversion de Projection Statique (Static View-Driven Data Pipeline)

**Version :** 0.1
**Cible :** Moteur Marius (Crates `marius-schema`, `marius-projection`, `marius-render`)

## 1. Objectif Système

Définir un pipeline déterministe où la définition de la vue (Template `.marius` en syntaxe d'indentation) agit comme l'unique contrat de layout binaire pour l'ensemble de la chaîne, depuis l'extraction PostgreSQL jusqu'à l'écriture du buffer réseau. L'objectif est de garantir une allocation nulle au runtime et de saturer le bus mémoire utile en éliminant toute donnée non déclarée par la vue.

## 2. Invariants et Contrats Fondamentaux

1. **AOT Absolu :** La vue est un contrat de compilation, pas un script d'exécution. Si un composant n'est pas explicitement appelé par la vue, son empreinte mémoire au runtime est mathématiquement nulle.
2. **Normalisation de l'Indexation :** Les éventuelles configurations externes (fichiers YAML exposés à l'humain pour paramétrer les vues) emploient une indexation base-1. Le compilateur AOT normalise impérativement toutes ces entrées en indexation base-0 pour la génération des offsets mémoires, tableaux et pointeurs internes.
3. **Séparation Taxonomie / Algorithme :** La structure de répertoires hébergeant les templates `.marius` et les projections générées documente strictement la taxonomie métier. Cette arborescence ne pilote et ne déclenche aucun algorithme de calcul ou de résolution de route au runtime ; la résolution repose intégralement sur le dispatch des `Opcodes` générés.

## 3. Topologie du Pipeline de Compilation (AOT)

Le script de build (ou `proc_macro`) analyse les fichiers `.marius` et génère trois artefacts synchronisés.

### Artefact A : Le Contrat de Projection (`marius-projection`)

Le compilateur déduit la taille exacte en octets de la structure requise et génère le layout C-compatible correspondant.

- **Format :** Structs `#[repr(C)]` ou `#[repr(C, packed)]`.
- **Règle :** Utilisation exclusive de types primitifs ou de tableaux de taille fixe. Les chaînes de caractères dynamiques sont remplacées par des tranches (`[u8; N]`) ou des couples d'offsets pointant vers un segment contigu.

### Artefact B : L'Extracteur I/O (`marius-schema`)

Génération de la requête SQL ciblant exclusivement les composants identifiés.

- **Format :** Requête SQL statique.
- **Mécanique :** La requête exploite les fonctions de formatage binaire de PostgreSQL (ou un protocole `COPY ... WITH (FORMAT BINARY)`) pour correspondre octet pour octet au layout défini dans l'Artefact A.

### Artefact C : Le Pipeline de Rendu (`marius-render`)

Transformation de l'arbre syntaxique du template en un flux d'instructions linéaires.

- **Format :** Tableau statique d'`Opcodes`.
- **Mécanique :** Séparation des segments HTML statiques (stockés dans un `&'static [u8]` unique) et des instructions d'injection de composants (offsets mémoires).

## 4. Architecture de Résolution au Runtime

Le runtime de Marius perd toute capacité d'analyse textuelle et se transforme en un simple exécuteur de pipeline mémoire.

1. **Phase de Synchronisation (Write) :**

- PostgreSQL exécute l'Artefact B.
- Le flux binaire est écrit directement dans la mémoire partagée (Shared Memory POSIX / `mmap`), s'alignant sur les structures de l'Artefact A.

2. **Phase de Rendu (Read/Hot Path) :**

- Le thread de rendu (`render_batch_pure`) itère sur le tableau d'`Opcodes` (Artefact C).
- L'exécution se résume à des appels `ptr::copy_nonoverlapping`, copiant alternativement le HTML statique et les octets ciblés du `mmap` vers le buffer de sortie.
- **Contrat de performance :** 0 allocation sur le tas (Heap), 0 désérialisation, 0 évaluation logique.

## 5. Prochaines Étapes d'Audit (Vers v0.2)

- Définir le protocole binaire exact d'export depuis PostgreSQL vers la structure `mmap` (mapping natif vs sérialisation personnalisée dans le WAL).
- Établir la structure de la macro de parsing `.marius` pour générer le `static_segments` HTML.

---

Le 4 juin 2026.
