# Roadmap & Décisions ouvertes — `marius-assets`

Ce document complète `marius-assets-specification.md`. Il liste ce qui reste à trancher avant ou pendant l'implémentation, et ce qui est volontairement différé. Rien ici ne contredit la spécification ; ce sont des cases non cochées, pas des désaccords.

---

## 1. Décisions à trancher avant ou pendant l'implémentation (v1)

Chaque point ci-dessous doit être tranché explicitement — soit par l'auteur du projet, soit proposé puis validé en session d'implémentation. Aucun ne doit être décidé silencieusement par défaut.

### 1.1 Ordre explicite des fichiers JS composants

La convention actuelle (préfixe `_`, tri alphabétique ASCII) ne généralise pas si plusieurs fichiers d'une même cible ont simultanément besoin d'une contrainte d'ordre précoce. Deux options :

- Conserver le tri par préfixe, avec une règle stricte "un seul fichier `_`-préfixé par cible" imposée et vérifiée.
- Introduire un ordre explicite (ex. `_01_base.js`, `_02_foo.js`, ou un petit fichier déclaratif listant l'ordre).

### 1.2 Validation des références d'URL littérales dans le JS composants

Proposée par analogie avec la validation Fonts↔CSS (§10.1/§10.3 de la spec), jamais formellement confirmée par l'auteur. Décision à prendre : inclure en v1, ou différer.

### 1.3 Détection de collision de liaison top-level JS

Le mécanisme (lexer à suivi de profondeur d'accolades, §10.4.1 de la spec) est désormais spécifié en détail suite à l'audit. Reste seulement à trancher : son inclusion en v1 ou son report — recommandé en v1 vu son coût d'implémentation modéré (lexer à un seul passage, pas un parseur complet) et sa cohérence avec la rigueur déjà actée pour SVG/Fonts.

### 1.4 Format et emplacement exact du fichier manifeste de build

Nom de fichier, chemin exact relatif à la racine de build, encodage (TOML pressenti par cohérence avec l'exemple initial `[[asset]]` et avec le choix désormais acté pour la configuration de thème, §4 de la spec) — à fixer en conception technique.

### 1.5 Contrat exact de déclaration des bibliothèques vendored

Comment le développeur déclare-t-il, dans la configuration de thème, la liste des bibliothèques à préparer (nom, version, sous-liste d'extensions — ex. les extensions HTMX comme `hx-sse`) ? Question d'API reportée volontairement pendant la session de scope, à traiter en conception technique.

### 1.6 Convention de nommage SVG en cas de collision de dossiers homonymes

La règle "id déduit du nom de fichier" est actée, mais le cas de deux fichiers de même nom dans des sous-dossiers différents d'un même thème n'a pas été explicitement examiné. À vérifier : la règle de collision d'ID (§10.2 de la spec) couvre-t-elle déjà ce cas par construction, ou faut-il une règle de chemin relatif aplati ?

### 1.7 Politique de suivi HTMX 4 (bêta → stable)

HTMX 4 est retenu malgré son statut non stabilisé au moment de la conception. Aucune procédure n'a été définie pour la montée en version vers une future release stable (impact potentiel sur les hash de contenu des artefacts vendored, donc sur les URL publiques déjà en cache navigateur).

### 1.8 Mécanisme de résolution d'URL interne au CSS — au-delà des fonts

La résolution d'URL versionnée pour `@font-face` (§10.1/§10.3 de la spec, fermée lors de la clôture du hash par artefact) ouvre une question sœur non encore traitée : le CSS peut aussi référencer des images de fond (`background-image: url(...)`) ou d'autres ressources via `url()`. Le même mécanisme de résolution doit-il s'étendre à tout `url()` rencontré dans le CSS source, ou seulement à `@font-face` pour la v1 ? Portée à trancher explicitement — l'extension parait naturelle mais élargit la surface de ce que le compilateur doit reconnaître dans la grammaire fermée CSS (§10.3).

---

## 2. Évolutions différées (post-v1, compatibles sans rupture de contrat)

Ces pistes ont été explicitement mises de côté pour éviter la complexité prématurée. Elles ne doivent être reprises que si elles s'intègrent naturellement, sans dépendance structurelle nouvelle.

### 2.1 Réduction pilotée par l'usage réel (tree-shaking)

Exploiter les métadonnées produites par `fragment-forge` (composants/fragments réellement utilisés) pour générer :

- un sprite SVG limité aux icônes effectivement utilisées ;
- un bundle JS limité aux modules effectivement nécessaires ;
- une feuille de style réduite aux règles effectivement utilisées.

**Condition de reprise** : `marius-assets` ne doit jamais dépendre structurellement de `fragment-forge` en tant que crate Rust. Si cette piste se concrétise, l'interface correcte est la même que celle déjà retenue pour le manifeste : `fragment-forge` émet une liste d'identifiants utilisés (fichier de données), `marius-assets` la consomme en entrée optionnelle — jamais un couplage de types Rust entre les deux crates.

**Point de vigilance identifié en audit (à ne pas casser lors de l'implémentation)** : si le tree-shaking est un jour implémenté, il faut impérativement **séparer l'extraction d'usage de la substitution finale** pour éviter un faux cycle Forge ↔ Assets :

```
Templates .marius (texte brut)
        │  extraction pure — ne lit que les templates, jamais le Manifeste
        ▼
IR d'usage (identifiants de fragments/assets référencés)
        │
        ▼
marius-assets (tree-shaking guidé par l'IR) → Manifeste (avec hash finaux)
        │
        ▼
Forge — passe de substitution (lit le Manifeste, abaisse {% asset id %})
```

La confusion à éviter : croire que « la Forge » qui produit l'IR d'usage et « la Forge » qui substitue les tokens `{% asset id %}` doivent être la même passe, exécutée au même moment, sur les mêmes données. Ce sont deux passes distinctes — la première ne dépend jamais du Manifeste (elle ne lit que le texte des templates), la seconde en dépend entièrement. Tant que cette séparation est respectée, il n'y a pas de cycle et la topologie producteur-unique du §8 de la spec reste valide sans modification.

**À proscrire explicitement** : une résolution d'`{% asset id %}` par table de correspondance embarquée dans le binaire du **Shell** au runtime (id logique → hash), même si elle est techniquement séduisante pour contourner l'ordonnancement — elle réintroduit une indirection au runtime que la conception a délibérément éliminée (§9 de la spec : `push_str` figé en dur, zéro lookup).

### 2.2 Migration du stockage physique des artefacts

V1 : fichiers indépendants sur disque, adressage direct par le Shell via le manifeste. Évolutions envisageables sans jamais modifier le contrat de manifeste : packfile unique, ou mémoire partagée (SHM). Le manifeste masque déjà cette couche de stockage par construction — la migration ne devrait toucher que l'implémentation interne du crate et la lecture côté Shell, jamais l'API du manifeste lui-même.

### 2.3 Regroupement visuel de `crates/forge/` et `crates/assets/`

Éventuel préfixe commun de haut niveau (type `build-tools/`) pour signaler visuellement "outils exécutés avant le runtime", tout en conservant la distinction Forge-cible-Core / Assets-cible-filesystem au niveau du nom de crate plutôt que du dossier. Non nécessaire pour la lisibilité actuelle de l'arborescence.

---

## 3. Refactoring différé, hors périmètre de cette session

- **Déplacement de `forge/` sous `crates/forge/`** : décision actée en principe (`crates/` doit redevenir une catégorie univoque : tout ce qui est membre du workspace Cargo, et seulement ça), mais son exécution — y compris la mise à jour du `Cargo.toml` racine (glob ou liste de membres) — est explicitement reportée à une session ultérieure.

---

## 4. Tests de validation appliqués pendant la définition du périmètre

Ces méthodes ont servi à trancher les questions de placement et de couplage tout au long de la conception ; elles restent utiles pour toute décision future similaire.

- **Test de substitution** : retirer un voisin architectural du système et vérifier si le composant étudié reste inchangé. Utilisé pour confirmer que `marius-assets` ne dépend pas du Shell (substituable par nginx), et que la connaissance d'une bibliothèque vendored (HTMX) ne doit jamais inclure sa raison d'être architecturale (substituable par une autre bibliothèque sans changement de responsabilité).
- **Précédent de nommage dossier/package** : avant d'inventer une nouvelle convention, vérifier si le workspace en a déjà une. A permis de confirmer `crates/assets/` / `marius-assets` par analogie directe avec `core/collector`, `core/projection`, `core/schema`.
