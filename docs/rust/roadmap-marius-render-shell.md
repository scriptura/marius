# Roadmap : Render Shell — implémentation

Statut : prête pour exécution par sessions successives (~200k tokens/session).
Document de référence : `specification-marius-render-shell.md`.
ADR amont, non rediscutées : ADR-002, ADR-006, ADR-007, ADR-008, ADR-009.

Découpage hérité d'une proposition de Gemini, auditée et corrigée sur quatre
points (périmètre Phase 1, dépendance non déclarée Phase 3, placement de
fichier Phase 4, angle mort sur le compilateur de pages) — voir le compte
rendu d'audit qui précède ce document dans son contexte de production.

**Règle de transmission inter-session** : chaque phase commence en donnant à
la session la spec (§ pertinentes seulement, pas le document entier si la
session précédente l'a déjà ingéré) et le code produit par la phase
précédente — jamais en lui faisant relire tout l'historique de conception.
Le coût de "reconstruire la compréhension depuis zéro" est le premier poste
de gaspillage de budget sur ce genre de découpage, pas la complexité du code
lui-même.

---

## Phase 1 — Primitives physiques (format binaire, mmap borné)

**Fichiers, nommage explicite (corrige l'ambiguïté de la proposition
initiale) :**

- `crates/shell/render/src/pack_html_format.rs` — **nouveau**. Contient
  uniquement les types du format on-disk : `PackfileEntry`, `PackfileFooter`,
  constantes de taille, `const assert!`. Source de vérité unique, importée
  par l'écriture et la lecture — même discipline que `PackfileStoreHeader`
  dans `marius_projection` (un seul endroit qui définit le format, deux qui
  le consomment). Ne PAS dupliquer ces définitions dans `batch_renderer.rs`
  ou le nouveau fichier de lecture.
- `crates/shell/render/src/batch_renderer.rs` — déjà patché cette session
  (`write_packfile_footer`). Importe désormais `pack_html_format` au lieu de
  définir `PackfileEntry`/`PackfileFooter` localement — léger refactor de
  déplacement, pas de changement de logique.
- `crates/shell/render/src/pack_html_index.rs` — **nouveau**. `PackHtmlIndex`
  (lecture), corrigé suite à l'audit de concurrence/mmap (spec §5) :
  ouverture via `read_at` du footer, `mmap` borné strictement à la région
  d'index.

**`packfile_builder.rs` n'est pas touché — hors périmètre de cette phase et
de toute la roadmap.** C'est le format `store.bin` (données), pas le format
packfile HTML.

**Objectif** : le format on-disk complet, dans les deux sens, sans aucune
dépendance à Axum/Tokio/SQLx — testable en isolation totale.

**Jalon 1 — critères d'acceptation explicites (pas seulement narratifs) :**

- Écrire un blob synthétique + footer (réutilise `write_packfile_footer`,
  déjà livré), ouvrir via `PackHtmlIndex::open()`.
- `mmap.len() == footer.index_len` — assertion stricte, pas approximative.
  C'est le test qui rend l'invariant "le blob n'est jamais mappé" vérifiable
  plutôt que déclaratif.
- `binary_search` retourne le bon `(offset, len)` pour chaque id présent,
  `None` pour un id absent.
- **Cas limites obligatoires, pas optionnels** (la proposition initiale les
  reléguait à une note de vigilance informelle — ils doivent être des tests,
  pas des avertissements) :
  - `entry_count == 0` (table vide) — `open()` doit réussir, `lookup()` doit
    toujours retourner `None`, jamais paniquer.
  - `entry_count == 1` (cas limite bas).
  - Un blob synthétique volontairement massif (quelques centaines de Mo,
    généré par répétition, pas par vraie donnée) avec un index minuscule —
    preuve concrète que `mmap.len()` reste petit indépendamment de la taille
    du blob.
  - Footer corrompu (magic invalide, version inconnue) → `open()` retourne
    une erreur, ne panique jamais.

**Budget de contexte** : le plus léger des quatre. Zéro dépendance externe
nouvelle au-delà de `bytemuck`/`memmap2`, déjà présentes dans le crate.

---

## Phase 2 — Registre lock-free et concurrence I/O

**Fichiers :**

- `crates/shell/render/src/registry.rs` — **nouveau**. `LiveRegistry`,
  `arc_swap::ArcSwap<PackHtmlIndex>` par clé de packfile.

**Objectif** : prouver que la lecture concurrente et le remplacement
atomique d'un index coexistent sans jamais produire une lecture incohérente
— **sans Tokio**, par discipline de budget (reprend tel quel le bon réflexe
de la proposition initiale).

**Jalon 2 — critères d'acceptation, rendus concrets :**

- N threads lecteurs natifs (`std::thread`), chacun en boucle serrée :
  `registry.load()` → `lookup(id)` → `read_at(offset, len)` → vérifier que
  les octets lus correspondent exactement au fragment attendu pour cet `id`
  précis (comparaison stricte, pas juste "ça n'a pas paniqué" — une lecture
  *décalée* mais qui ne crashe pas serait un faux négatif si le test ne
  vérifie pas le contenu).
- 1 thread écrivain qui appelle `registry.store(Arc::new(nouvel_index))` en
  boucle, à une fréquence largement supérieure à la durée d'une requête de
  lecture — pour maximiser la probabilité de capturer une fenêtre de
  remplacement en plein milieu d'une lecture.
- Assertion finale : zéro lecture incohérente sur l'ensemble du test, et le
  nombre de `PackHtmlIndex` instanciés au cours du test correspond exactement
  au nombre de `store()` appelés + 1 (le premier) — preuve indirecte qu'aucun
  ancien index n'a fui (en lien avec le 3ᵉ point de vigilance de Gemini,
  vérifié ici plutôt qu'en Phase 4 où il serait plus coûteux à isoler).

**Budget de contexte** : modéré. Pas de nouvelle dépendance hormis
`arc_swap`, déjà nommée dans la spec.

---

## Phase 3 — Frontière réseau (Axum/Tokio)

**Fichiers :**

- `crates/shell/server/src/main.rs` — bootstrap, `LiveRegistry::cold_start()`,
  enregistrement des routes.
- `crates/shell/server/src/handlers.rs` — **nouveau**. `serve_route`,
  `deliver` (spec §6.1/§6.3 — version **corrigée** : `read_at` +
  `spawn_blocking` + `Vec<u8>` en corps de réponse, **pas** un streaming
  `tokio::fs::File`/`Body::from_stream` — cette dernière forme apparaissait
  dans un brouillon antérieur de la spec et a été remplacée ; s'assurer que
  la session de cette phase reçoit la version finale du document, pas un
  extrait daté).

**Dépendance déclarée explicitement — correction du défaut de la proposition
initiale** : `ROUTE_TABLE` n'est, à ce stade du projet, générée par aucun
outil. Le compilateur de templates de page (`FragmentRef`, ADR-008 §4.2-§4.5)
n'a jamais été implémenté — seulement spécifié. **Cette phase utilise une
`ROUTE_TABLE` écrite à la main**, littéralement, comme `const` Rust dans
`main.rs` ou un fichier de configuration dédié — pas générée. C'est un choix
délibéré, pas un raccourci honteux : il découple la validation de la
frontière réseau de l'existence du compilateur de pages, qui est un effort
séparé, plus lourd, hors périmètre de cette roadmap (voir section finale).

**Objectif** : prouver que la frontière réseau respecte les invariants —
zéro calcul, lookup O(log N), livraison correcte, capacité d'erreur propre
(404, 400).

**Jalon 3 :**

- Serveur démarré sur un port de test, `LiveRegistry::cold_start()` réussi
  sur 2-3 packfiles synthétiques (réutiliser les fixtures de Phase 1).
- Requêtes concurrentes (`reqwest` ou équivalent, dans le test) :
  - `GET` sur un id existant → 200, `Content-Length` exact, corps identique
    à l'attendu.
  - `GET` sur un id absent → 404.
  - `GET` avec paramètre non numérique → 400.
  - Charge concurrente modérée (quelques centaines de requêtes simultanées)
    pour confirmer l'absence de blocage du pool Tokio — lien direct avec le
    2ᵉ point de vigilance de Gemini, à vérifier ici concrètement, pas
    seulement en théorie.
- **Point de vigilance supplémentaire, absent de la liste initiale** :
  réexécuter, *dans ce contexte Tokio*, une variante du test de swap de la
  Phase 2 (déclencher un `registry.store()` pendant que le serveur sert des
  requêtes réelles). La Phase 2 prouve la correction d'`ArcSwap` en
  isolation ; elle ne prouve pas que l'interaction avec le pool de threads
  bloquants de Tokio (`spawn_blocking`) reste correcte sous charge réelle —
  un risque d'intégration distinct, pas couvert par les 3 points de Gemini.

**Budget de contexte** : le plus lourd des quatre — surface de documentation
Axum/Tower plus large. Recommandation explicite : fournir à cette session le
code des Phases 1 et 2 déjà fonctionnel comme contexte de départ, pas
seulement la spec — reconstruire `PackHtmlIndex`/`LiveRegistry` depuis la
prose coûterait inutilement cher en tokens.

---

## Phase 4 — Boucle d'écriture (régénération + bascule)

**Fichiers :**

- `crates/shell/render/src/regenerate.rs` — **nouveau** (corrige le
  placement ambigu "`dispatcher.rs` ou `dumper.rs`" de la proposition
  initiale). `regenerate_and_swap<P>` est générique aux deux usages (dump
  initial, mutation Dispatcher) — il n'appartient à aucun des deux fichiers
  existants. `dispatcher.rs` et `dumper.rs` l'appellent, ne le contiennent
  pas.

**Objectif, scindé en deux jalons distincts plutôt qu'un seul jalon
end-to-end** — la proposition initiale fusionnait les deux, risquant de
consommer tout le budget de la session en câblage d'infrastructure (DB
réelle + serveur réel + Dispatcher réel) avant la première assertion utile :

**Jalon 4a — sans PostgreSQL, sans serveur réel (le cœur du test, doit
passer en premier) :**

- `Projection` stub (réutiliser le pattern `StubProjection` de
  `batch_renderer.rs`), pas de vrai `fetch_from_pg`.
- Appeler `regenerate_and_swap` deux fois de suite avec des données stub
  différentes, sur un `LiveRegistry` de test.
- Assertions : le fichier temporaire est bien renommé, l'ancien fichier
  n'existe plus sous son nom temporaire, le nouvel index lu après le swap
  reflète bien les nouvelles données, et — reprise du 3ᵉ point de vigilance
  de Gemini — un lecteur ayant chargé l'`Arc` *avant* le swap continue de
  fonctionner sans erreur jusqu'à la fin de sa requête en cours (pas de
  coupure brutale).

**Jalon 4b — intégration réelle, marquée `#[ignore]`, `DATABASE_URL` requis**
(cohérent avec la convention déjà établie dans tout le reste du projet pour
ce type de test — Phase 4 db-forge, etc.) :

- Mutation réelle sur PostgreSQL → Dispatcher (ou appel manuel équivalent) →
  `regenerate_and_swap` avec un vrai pool → requête HTTP réelle sur le
  serveur de la Phase 3 → la réponse reflète la donnée mutée.
- Ce jalon peut être laissé non implémenté à l'issue de cette session si le
  budget est consommé par 4a — 4a seul valide déjà la quasi-totalité de la
  mécanique. 4b est une preuve de bout en bout, pas la source principale de
  confiance.

**Budget de contexte** : modéré pour 4a, potentiellement lourd pour 4b selon
l'état d'avancement du Dispatcher réel au moment de l'exécution de cette
phase — à traiter comme optionnel/séparable explicitement, pas comme un
bloc indivisible.

---

## Points de vigilance — les trois de Gemini, confirmés, plus un quatrième

1. **Arithmétique du mmap (Phase 1).** Confirmé, critique. Couvert
   maintenant par des cas limites explicites dans le Jalon 1, pas seulement
   par une note.
2. **Étouffement du pool Tokio (Phase 3).** Confirmé. Couvert par un test de
   charge concurrente explicite dans le Jalon 3.
3. **Libération des fd/inodes (Phase 4, et déjà vérifiable en Phase 2).**
   Confirmé — déplacé en partie vers la Phase 2 (comptage d'instances
   `PackHtmlIndex`), où il est moins coûteux à isoler qu'en bout de chaîne.
4. **Interaction `ArcSwap` × pool de threads bloquants sous charge réelle
   (Phase 3) — absent de la liste initiale.** La correction d'`ArcSwap` seul
   (Phase 2) ne garantit pas son comportement correct une fois composé avec
   `spawn_blocking` et un vrai exécuteur Tokio sous charge. Risque
   d'intégration distinct, à tester spécifiquement, pas supposé hérité
   automatiquement de la preuve unitaire.

---

## Hors périmètre — explicitement, pas par omission

Cette roadmap ne couvre que le Render Shell tel que spécifié. **Elle ne
couvre pas, et ne doit pas être confondue avec** :

- Le compilateur de templates de page (`FragmentRef`, parsing, résolution de
  capacité composée — ADR-008 §4.2 à §4.5). Tant qu'il n'existe pas, la
  `ROUTE_TABLE` reste écrite à la main (Phase 3). Une roadmap distincte sera
  nécessaire pour ce compilateur — ne pas tenter de l'improviser comme
  sous-tâche d'une phase de cette roadmap-ci.
- La génération automatique du graphe d'invalidation composant→pages
  (ADR-008 §5) — nécessaire pour que le Dispatcher sache *quoi* régénérer en
  cascade, en amont de l'appel à `regenerate_and_swap` (Phase 4). Cette
  roadmap suppose que l'appelant sait déjà quelle cible régénérer ; elle ne
  spécifie pas comment il le sait.
- `libc::sendfile` réel (Option B de la spec, §6.3) — différé tant que
  l'Option A (retenue, Phase 3/4) n'est pas mesurée insuffisante.
