# Manifeste de la Projection Réactive

> **Créé le 25 mars 2026. Révisé le 28 juin 2026. Corrigé et vérifié le 7 juillet 2026** — voir notes de révision en fin de document, deux erreurs de périmètre distinctes, ne pas les confondre.

## Architecture Data-First & Rendu AOT

### 1. Vision Stratégique

Le serveur web n'est plus un médiateur interactif, mais un **Système de Projection**. Il transforme de manière déterministe un flux de mutations de données (PostgreSQL) en artéfacts statiques ou semi-statiques (HTML brut via allocation minimale), éliminant le besoin de caches intermédiaires (Redis, Memcached). L'artéfact généré _est_ l'état optimal de lecture.

### 2. Résolution des Problèmes Classiques

- **Invalidation du Cache :** Élimination de la logique temporelle (TTL). L'artéfact est réécrit uniquement lorsque la source de vérité le commande.
- **Indirection de Transformation :** Suppression du mapping objet-relationnel (ORM) et de la sérialisation (JSON). ~~Suppression du driver SQL lui-même sur le chemin chaud. Le pipeline transfère les octets directement d'un instantané mémoire-mappé (`store.bin`) aux buffers d'écriture HTML~~ **[Inexact, corrigé le 7 juillet 2026 — voir note de révision]** : le driver SQL reste actif sur le chemin de régénération (`fetch_batch`, asynchrone, hors requête HTTP) ; c'est le **chemin chaud HTTP** — distinct du chemin de régénération — qui reste exempt de SQL et d'allocation, via lecture directe du pack déjà rendu (`pread`). L'accès SQL réel (`marius-dump`, extraction périodique) reste, lui, correctement décrit : confiné, jamais sur le chemin chaud.
- **Gaspillage CPU :** Le rendu est calculé une seule fois à l'écriture (AOT), libérant le CPU pour le transport réseau (I/O) lors de la lecture.

### 3. Invariants Structurels

L'architecture repose sur trois piliers inaltérables :

1. **Source de Vérité (PostgreSQL) :** Centralise la logique métier et l'état. Seule la base de données qualifie une mutation. `store.bin` (§2) n'est qu'une projection dérivée, jamais une source concurrente — sa fraîcheur dépend du processus d'extraction qui l'alimente, pas d'une autorité propre.
2. **Canal de Transport (LISTEN/NOTIFY) :** Protocole asynchrone natif poussant les signaux de mutation vers le système applicatif.
3. **Transformateur Pur (Rust AOT) :** Un pipeline de génération de chaînes brutes sans état interne, traduisant le modèle de données (`struct`) en mémoire contiguë (HTML/Octets).

### 4. Limite Physiologique : L'Amplification d'Écriture

**Le Risque :** Dans un système réactif pur, une mise à jour massive en base de données (ex: 10 000 lignes modifiées via une procédure stockée) déclenche une avalanche de notifications. Traiter chaque signal individuellement sature le pipeline de rendu et les I/O disque/réseau, provoquant un goulot d'étranglement CPU.

### 5. La Solution DOD : Le Modèle Collector / Dispatch

Pour protéger le transformateur, on interpose un système de regroupement (Batching) qui réduit l'entropie du flux d'événements.

- **Le Collector (Dédoublonnement en O(1)) :**
  Les signaux entrants sont stockés dans une structure contiguë avec contrainte d'unicité (une _table de présence_). Si un même ID est modifié 50 fois dans un court intervalle, il n'est conservé qu'une fois dans le layout mémoire.
- **Le Dispatcher (Tick & Seuil) :**
  Le vidage du Collector (`flush`) est régi par deux invariants stricts pour lisser la charge (Smoothing) :
- _Volumétrique :_ Déclenchement si la capacité maximale est atteinte (ex: 100 entités).
- _Temporel :_ Déclenchement périodique forcé (variable ajustée en temps réel selon la télémétrie du système, entre 100ms et 2s).

- **Concurrence Inter-Shard, Séquentialité Intra-Lot (Sympathie Mécanique) :**
  Chaque artéfact projeté possède son propre `Dispatcher`, exécuté comme tâche asynchrone indépendante — la concurrence entre artéfacts distincts provient de l'ordonnanceur Tokio (répartition naturelle sur les cœurs disponibles), pas d'un fan-out manuel par lot. À l'intérieur d'un même lot, en revanche, le rendu est délibérément **séquentiel** : un buffer unique, alloué une fois, réutilisé et vidé entre chaque enregistrement. Ce choix sacrifie le parallélisme intra-lot pour saturer le cache L1 et éliminer toute allocation sur le chemin chaud. La stabilité de la latence ne provient donc pas d'une distribution de calcul, mais du couple Seuil/Tick (lissage de l'entropie en amont, ci-dessus) conjugué à une régulation explicite de la concurrence d'écriture disque inter-shard (sémaphore borné, pour contenir la pression sur le cache de pages du noyau).

### 6. Pipeline Mécanique Global

Le cycle de vie complet d'une donnée suit ce flux directionnel strict :

1. **Mutation DB :** `UPDATE content.document` $\rightarrow$ Trigger SQL.
2. **Signal :** `pg_notify('updates', 'ID')`.
3. **Capture :** Écouteur asynchrone Rust $\rightarrow$ Enregistrement dans la _table de présence_.
4. **Dispatch :** Seuil ou Tick atteint $\rightarrow$ Extraction des IDs uniques.
5. **Extraction Data :** ~~Lecture mémoire-mappée zéro-copie d'un instantané local (`store.bin`) — aucune requête SQL sur le chemin réactif.~~ **[Inexact, corrigé le 7 juillet 2026]** Requête PostgreSQL live (`P::fetch_batch(pool, ids)`) sur le delta du tick — `store.bin` n'est jamais lu à cette étape. Voir note de révision en fin de document.
6. **Projection AOT :** Génération de texte brut (`push_str`) sur un buffer unique réutilisé, séquentiellement par lot — zéro allocation, localité de cache maximale. La concurrence reste inter-shard (Tokio), jamais intra-lot.
7. **Persistance :** Remplacement atomique de l'artéfact (Fichier / RAM).

---

Document rédigé le 25 mars 2026.
Révisé le 22 juin 2026.
Révisé le 28 juin 2026 — §2, §3, §5, §6 : l'implémentation réelle (Phase 4) a éliminé le driver SQL du chemin chaud (lecture `store.bin` mmap plutôt que `SELECT` par lot) et opté pour un rendu séquentiel zéro-allocation plutôt qu'une distribution Rayon intra-lot. La réalité physique du système a dépassé l'intention initiale plutôt que l'avoir trahie — corrections apportées pour refléter cette discipline DOD plus stricte que prévu, pas pour constater un écart à corriger dans le code.

Révisé le 7 juillet 2026 — §2, §6.5 : la révision du 28 juin était elle-même inexacte sur un point précis, repéré par audit croisé contre le code compilé (`regenerate.rs`, `dispatcher.rs`, Phase 4.2). Confusion de périmètre entre deux chemins distincts, jamais nommés séparément avant cette révision :

- **Chemin chaud (service HTTP)** : `pread` sur le pack déjà rendu. Zéro SQL, zéro allocation — invariant intact, jamais remis en cause.
- **Chemin de régénération** (NOTIFY → Collector → Dispatcher → `regenerate_and_swap`) : appelle `P::fetch_batch(pool, ids)`, une requête PostgreSQL live sur le delta du tick. `store.bin` n'y est jamais lu — c'est un instantané figé au dernier `marius-dump`, hors-bande, sans garantie de fraîcheur avec la mutation venant de déclencher le `NOTIFY`. Le lire à cette étape aurait servi une donnée périmée au moment précis où la réactivité doit garantir sa fraîcheur — contradiction interne du modèle initial, pas une simple dérive d'implémentation.

Le pilier 2 (§3, Canal de Transport) et le modèle Collector/Dispatch (§5) restent exacts tels quels — seule la nature de l'« Extraction Data » (§6, étape 5) était mal caractérisée.
