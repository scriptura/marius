# ADR-008 : Topologie de l'artéfact de lecture — composition de pages depuis des fragments

Statut : **Accepté** (révision — voir note ci-dessous).
Contexte projet : Marius, pipeline AOT (`marius-db-forge` / `marius-fragment-forge` / `marius-render`).
Documents liés : ADR-002 (Projection Réactive & État Hybride), ADR-003 (suppression
RenderPayload), ADR-007 (frontière Hot/Cold varlena), `core-system-blueprint.md`.

> **Note de révision.** Une première version de ce document (même session)
> proposait une composition résolue _à la requête_. Une relecture a établi
> que ce modèle contredit la doctrine fondatrice du projet
> (`core-system-blueprint.md` §3.A : un seul `sendfile()`, aucune résolution
> applicative à la lecture). Cette version corrige le modèle : la composition
> se résout _à l'écriture_ (dump/Dispatcher), jamais à la lecture. La
> délibération complète de ce renversement est archivée séparément — voir
> §3.3 et le post-mortem qu'il référence.
>
> **Amendement.** Quatre clarifications
> intégrées : taxonomie explicite des trois natures de contenu d'une page
> (§4.3, contenu de session/requête déclaré hors périmètre), taxonomie Forge
> explicite incluant l'exclusion des partials génériques paramétrés (§4.2),
> décision ferme sur le graphe d'invalidation — généré par la Forge, plus une
> hypothèse à vérifier (§5), et correction d'un exemple (`Header::render()`)
> qui contredisait silencieusement §4.6. Le cas des pages non adressables par
> clé primaire unique (page d'accueil, collections) est traité séparément,
> voir ADR-009.

---

## 1. Contexte et question déclenchante

À la clôture d'une session de travail sur le Render Shell, la question posée était :
_"comment rendre la page HTML à partir des fragments HTMX ?"_

Le pipeline construit jusqu'ici (`Projection`, `PackfileBuilder`/`PackfileReader`,
`resolve_and_measure`, `generate_aot_snippet`) produit un **fragment HTML par
enregistrement d'une table** — jamais une page composée de plusieurs fragments
(en-tête, navigation, contenu, pied de page). Aucune décision n'avait encore été
prise sur la façon dont une page assemble ces fragments, ni sur l'unité
fondamentale adressable par le serveur de lecture.

Une tentative de rédaction directe de `specification-marius-render-shell.md` a été
suspendue : ce document ne peut pas être spécifié avant que cette question de
topologie soit tranchée — il en dépend entièrement, il ne peut pas la précéder.

---

## 2. Diagnostic — pourquoi la Variante A (pages monolithiques) est un anti-pattern

Le PoC initial (Voie A, déjà purgée du code pour d'autres raisons — voir
historique de session) raisonnait : `Entity → Fragment HTML → Fichier HTML`,
un fichier complet par page. Appliqué à la composition de pages, ce modèle
imposerait qu'une page complète (en-tête + contenu + pied de page) soit
**stockée comme un artefact unique par entité**.

Défaut structurel : si `Header` (notifications, navigation) change, **toutes**
les pages qui l'incluent doivent être réécrites — alors que la donnée réelle
qui a changé ne concerne qu'un seul composant. Le coût d'invalidation devient
proportionnel au nombre de pages, pas au nombre de composants modifiés. Pour
10 000 produits, une modification du compteur de notifications dans `Header`
déclencherait la réécriture de 10 000 fichiers contenant chacun une copie
identique de ce `Header`. C'est exactement l'amplification d'écriture
qu'ADR-002 (Collector/Dispatcher) a été conçu pour éliminer côté mutation —
la rebâtir côté lecture serait incohérent avec le reste de l'architecture.

**Décision actée** : aucun artefact "page complète" n'est jamais stocké sur
disque comme un fragment monolithique pré-composé. Chaque composant
(`Header`, `ProductCore(id)`, `Footer`, etc.) reste stocké et invalidé
indépendamment, dans son propre `packfile`, exactement comme aujourd'hui pour
une table unique.

---

## 3. Hypothèses examinées et écartées

### 3.1 Variante B — composition pure côté client (HTMX multi-fetch)

La page initiale est une coquille statique ; le navigateur effectue N requêtes
(`GET /fragment/product/42`, `GET /fragment/header`, etc.) pour assembler la
page. Chaque requête est alors un `sendfile()` unique en O(1) — la plus simple
côté serveur.

**Écartée comme solution par défaut** : cohérente avec ADR-002 §1
("Vanilla JS... pas de réévaluation globale du script") sur le plan technique,
mais elle déplace l'orchestration de composition vers le navigateur — N RTT
réseau au lieu d'un seul, et une logique de coordination client (gestion de
fragments partiels, erreurs par fragment) qu'ADR-002 cherche justement à
éviter de complexifier côté client. Reste une option valide pour des
fragments véritablement indépendants et différés (lazy-load), mais pas le
modèle de composition de page par défaut.

### 3.2 `CompositionIndex` comme artefact dédié (symétrique de `SchemaIndex`)

Proposition initiale : un artefact généré distinct, décrivant layouts,
fragments, dépendances et routes — par analogie avec la paire ECS
`Entity/Component`, où une page serait à un fragment ce qu'une entité est à un
composant.

**Écartée.** Deux raisons, indépendantes l'une de l'autre :

1. **L'analogie ECS ne tient pas structurellement.** Un composant DOD au sens
   du projet est homogène, de stride fixe, contigu, batchable (`StorageRow`,
   `bytemuck::Pod`). Un ensemble `{Header, ProductCore, Footer}` n'a aucune de
   ces propriétés — ce sont des nœuds de composition hétérogènes, pas un
   tableau homogène. L'analogie peut rester utile pédagogiquement pour motiver
   l'intuition, mais elle ne doit jamais devenir un invariant architectural
   (ex : exiger un traitement par lot ou un stockage columnar des fragments,
   qui n'a aucune justification réelle ici).
2. **Aucun besoin concret non couvert par l'existant n'a été identifié.** Le
   graphe de composition d'une page existe déjà sous une forme exploitable :
   l'AST résolu (`Vec<FlatPageToken>`) du template de la page, produit par le
   pipeline `scan → parse_tokens → validate_ast → resolve_and_measure` déjà
   construit et validé. Introduire un second artefact pour décrire la même
   information dupliquerait une structure existante sans résoudre de problème
   qu'elle ne résout pas déjà. Principe directeur explicitement réaffirmé ici,
   le même qu'en ADR-007 (préférer l'extension du pipeline validé à un nouveau
   chemin parallèle) : **ne pas créer un artefact tant qu'une extension du
   pipeline existant suffit**, et ne réviser cette position que si un besoin
   concret, démontré, justifie le coût d'un second système.

### 3.3 Renversement requête/écriture — voir post-mortem dédié

Une étape intermédiaire de cette analyse a proposé, puis écarté, un modèle de
composition résolue _à la requête_, avant de converger vers le modèle retenu
en §4 (composition résolue _à l'écriture_). Cette volte-face — son
motivation, l'erreur qu'elle contenait, et ce qui l'a révélée — est
documentée intégralement dans
[`post-mortem/PM-001-composition-pages-resolution-temporelle.md`](../../post-mortem/PM-001-composition-pages-resolution-temporelle.md),
pour ne pas alourdir cette décision avec une délibération déjà close. Seule
la conclusion (§4) fait foi.

### 3.4 Vocabulaire syscall — précision technique actée

Une formulation initiale parlait de "Scatter-Gather I/O" pour décrire la
livraison de plusieurs fragments vers un socket. Imprécis : le scatter-gather
au sens strict (`readv`/`writev`) rassemble plusieurs _buffers mémoire d'un
même processus_ vers un descripteur, en un seul appel. `sendfile(2)` copie
d'**un seul** fd source vers un fd destination, entièrement noyau. Il n'existe
pas d'appel système rassemblant N fichiers distincts vers un socket en une
seule opération via `sendfile`. Livrer N fragments coûte **O(N) appels
système** (un `sendfile()` par fragment), pas O(1) — un coût réel, à nommer
explicitement plutôt qu'à masquer sous un terme valorisant. `io_uring`
(soumission batched de SQEs) est la seule voie connue vers un O(1) réel pour N
fragments, et reste un investissement distinct, non engagé par cet ADR.

---

## 4. Décision retenue

### 4.1 Deux unités stockées, pas une — le fragment seul ET la page composée

Le fragment individuel (`ProductCore(id)`, `ProductPrice(id)`...) reste
stocké et adressable indépendamment, sans changement à
`PackfileBuilder`/`PackfileReader` — nécessaire pour servir les swaps
partiels HTMX (`hx-get` ciblant un seul composant, sans recharger toute la
page).

**Une page composée est un second artefact stocké**, produit par les mêmes
fonctions `render()` que les fragments individuels — pas un nouveau
mécanisme de rendu, une réutilisation directe écrite vers un `packfile`
distinct (namespace "page", au même titre qu'un composant). Le render shell,
à la lecture, ne fait jamais la distinction entre lire un fragment seul ou
lire une page composée : dans les deux cas, un seul `sendfile()` vers un
fichier déjà entièrement écrit. La composition n'existe jamais comme
opération à la lecture — uniquement à l'écriture (dump initial ou Dispatcher
sur mutation).

### 4.2 Deux genres de templates `.marius`, pas un seul

Distinction nouvellement explicite, requise par la question posée en tête de
session ("la compilation doit être un assemblage de fichiers `.marius`") :

| Genre                                | Lié à une `Projection` ?                         | Emplacement                                           | Rôle                                                                                                                                                |
| ------------------------------------ | ------------------------------------------------ | ----------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Template feuille** (existant)      | Oui, 1:1 avec `schema.table`                     | `templates/{schema}/{table}.marius`                   | Pilote `StorageRow`/`VarlenOwned` → fragment HTML d'un enregistrement. Inchangé.                                                                    |
| **Template de page/route** (nouveau) | Non — compose des références vers des composants | `templates/pages/{route}.marius` (namespace distinct) | Décrit l'assemblage d'une page : chrome statique (`Static`/`StaticInclude`, inchangés) + références vers les fragments dynamiques qui la composent. |

`build.rs` doit distinguer les deux genres pour appliquer le bon chemin de
codegen : un template feuille produit toujours un `impl Projection` via
`write_projection_stub` (inchangé) ; un template de page produit une fonction
d'énumération de lookups (§4.3), jamais un `impl Projection`.

**Taxonomie Forge explicite — un fichier `.marius` ne produit pas
systématiquement un `packfile` (amendement) :**

| Cas                                                                                                                  | Exemple                                                                                                         | Packfile dédié ?                                                                                                                                                                                                                                                                                                                                                                                                                              |
| -------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Composant purement statique, sans variation                                                                          | `Button.marius` sans champ dynamique                                                                            | **Non** — `StaticInclude`, texte figé à la compilation, jamais de fichier adressable séparé                                                                                                                                                                                                                                                                                                                                                   |
| Composant dynamique lié à la _même_ entité que la page qui l'inclut                                                  | `data-product-id` du produit affiché sur sa propre page                                                         | **Non** — simples tokens `Field` écrits directement dans le template incluant, contre le même `SchemaIndex`. Pas de fichier `.marius` séparé nécessaire                                                                                                                                                                                                                                                                                       |
| Composant dynamique lié à une entité distincte, réutilisé par plusieurs pages                                        | `ProductCore(id)` référencé depuis plusieurs templates de page                                                  | **Oui** — template feuille ordinaire avec son propre `packfile`, référencé via `FragmentRef`                                                                                                                                                                                                                                                                                                                                                  |
| Partial générique, paramétré indépendamment de toute table fixe (fonction de template à arguments typés arbitraires) | Un composant "carte produit" réutilisable avec des paramètres ad hoc, sans `Projection` PostgreSQL sous-jacente | **Explicitement hors périmètre.** Construire ce mécanisme (système de templates à fonctions paramétrées générique) est un effort de Forge largement plus lourd que tout ce qui a été bâti à ce jour. Aucune tentative de le couvrir n'est faite par cet ADR — toute réutilisation de présentation doit, pour l'instant, passer par une `Projection` réelle (3ᵉ ligne) ou par `StaticInclude` (1ʳᵉ ligne), pas par un mécanisme intermédiaire. |

### 4.3 La composition se résout à l'écriture, jamais à la lecture

Le token d'AST ajouté pour référencer un composant depuis un template de
page :

```rust
FlatPageToken::FragmentRef {
    entity:    &'src str,   // nom symbolique dans le template
    component: &'src str,   // ex: "commerce.product_core"
    id_source: IdSource,    // d'où vient l'id au moment du dump/dispatch
}
```

`resolve_and_measure` le traite en consultant un `SchemaIndex` étendu pour
vérifier l'existence du composant référencé et accumuler sa capacité connue
(`{COMPONENT}_TOTAL_CAP`, déjà généré par le pipeline existant) dans
`total_dynamic_bytes` — même mécanique que pour un champ `Field` varlena,
aucune nouvelle catégorie de validation.

`generate_aot_snippet`, pour un template de page, émet un appel direct à
chaque `Component::render()` référencé par `FragmentRef`, concaténé dans le
buffer de la page — la mécanique initialement proposée, écartée puis
réhabilitée (voir post-mortem référencé en §3.3) une fois son motif de rejet
réfuté. Le chrome statique partagé (`Header`/`Footer` sans variation par
entité) passe par `StaticInclude`, pas par `FragmentRef` — voir la taxonomie
explicite ci-dessous, qui clarifie un exemple ambigu d'une version antérieure
de cette section :

```rust
fn render_product_page(ctx: &ProductPageContext, buf: &mut String) {
    buf.push_str(LAYOUT_HEADER);                              // StaticInclude — chrome partagé
    ProductCore::render(&ctx.core, &ctx.core_varlena, buf);   // FragmentRef — dynamique par entité
    ProductPrice::render(&ctx.price, &(), buf);               // FragmentRef — dynamique par entité
    buf.push_str(LAYOUT_FOOTER);                               // StaticInclude — chrome partagé
}
```

**Taxonomie explicite des trois natures de contenu possibles dans une page —
nécessaire suite à un audit croisé ayant révélé qu'une version antérieure de
cet ADR traitait `Header::render()` comme un `FragmentRef`, contredisant
silencieusement §4.6 :**

| Nature                                                                             | Mécanisme                     | Résolu quand                                                                                                                     |
| ---------------------------------------------------------------------------------- | ----------------------------- | -------------------------------------------------------------------------------------------------------------------------------- |
| Statique, partagé entre toutes les pages                                           | `StaticInclude` (inchangé)    | À la compilation — texte figé dans le binaire                                                                                    |
| Dynamique, dépendant d'une ligne PostgreSQL (l'entité de la page)                  | `FragmentRef` (§4.3)          | Au dump / à la mutation du composant référencé                                                                                   |
| Dynamique, dépendant de la requête ou de la session (utilisateur connecté, panier) | **Hors périmètre de cet ADR** | N/A — ne peut, par construction, jamais être pré-rendu au dump puisqu'il dépend de qui fait la requête, pas d'une ligne qui mute |

La troisième ligne n'est pas un oubli à combler ici : elle nomme une
frontière réelle du modèle AOT, pas un trou d'implémentation. Tout contenu
de cette nature (compteur de panier, nom de l'utilisateur) doit être injecté
par un mécanisme distinct, hors du pipeline `.marius`/`Projection` — typiquement
côté client (HTMX, `hx-get` ciblé) ou via un en-tête HTTP calculé par Axum à
la volée sur un fragment volontairement exclu du pré-rendu. Ce mécanisme
n'est pas spécifié par cet ADR ; sa nécessité est actée, sa conception est
différée.

**Qui appelle cette fonction, et quand — le point qui distingue ce modèle de
la version écartée en §3.3 :**

- `marius-dump` (cold start) : pour chaque id, construit le `ProductPageContext`
  (fetch des records nécessaires via `fetch_from_pg`, Voie d'Extraction —
  ADR distincte sur le cold start, déjà actée), appelle
  `render_product_page`, écrit le résultat dans le `packfile` "page" via
  `PackfileBuilder` — un dump complet, pas une opération par requête.
- **Le Dispatcher** (mutation, ADR-002) : sur la mutation d'un composant
  référencé par une page, régénère **les deux artefacts** depuis les mêmes
  données fraîchement fetchées — le fragment seul (pour HTMX) et la ou les
  pages composées qui le référencent (pour un chargement complet). Le Mode
  Batch (tick jusqu'à 2000ms, lots contigus) absorbe le coût d'amplification
  quand un composant largement référencé (`Header`) change — c'est
  exactement le rôle pour lequel ce mécanisme a été conçu en ADR-002, avant
  même cette discussion.
- **Le render shell, à la lecture, n'invoque jamais cette fonction.** Il ne
  fait jamais de résolution `(FragmentKind, id)` au moment de la requête —
  seulement un `sendfile()` vers un fichier déjà entièrement composé et
  écrit. La doctrine du chemin de lecture (§3.3) reste intacte sans
  exception.

**Invalidation hors-DB — pas un trigger neuf, une extension explicite du
contrat de déploiement existant.** Le Dispatcher réagit exclusivement à
`pg_notify` : une modification purement cosmétique d'un template (CSS, markup
statique de `Header.marius`, sans aucune mutation PostgreSQL) ne déclenche
aucun signal. Ce n'est pas un trou nouveau introduit par la composition de
pages — `meta_tooling_guide.md` §5 impose déjà la séquence
`cargo build → marius-dump → démarrage serveur` à chaque déploiement,
précisément parce qu'un changement de template (même pour un fragment seul)
nécessite une régénération complète du `store.bin`/`packfile` concerné. La
composition de pages rend une conséquence de ce contrat plus coûteuse, sans
en changer la nature : modifier un composant largement référencé (`Header`)
impose un `marius-dump` complet sur **toutes** les pages qui le référencent,
pas seulement sur sa propre table. Aucun mécanisme de détection de
changement de fichier `.marius` au runtime n'est introduit — le redéploiement
reste un événement explicite, opéré par l'humain ou la CI, jamais détecté
en tâche de fond.

**Tension "fonction composée vs plan d'exécution"** (formulée durant la
discussion) — résolue par la temporalité, pas par la forme : c'est bien une
fonction directe, monomorphisée, jamais un plan interprété contre un artefact
générique (`CompositionIndex` reste écarté, §3.2). Mais elle s'exécute à
l'écriture (dump/Dispatcher), jamais à la lecture — ce qui élimine à la fois
le besoin d'un plan runtime _et_ le coût de duplication qui semblait
initialement s'y opposer.

### 4.4 Indexation par template de page — extension du namespace existant, pas une nouvelle structure

Si un même enregistrement (`Produit 42`) est rendu par plusieurs templates de
page distincts (`/product/42` public, `/admin/product/42` administration),
l'identifiant seul (`id: i64`) ne suffit plus à discriminer une entrée dans
un unique espace d'adressage global — deux pages distinctes pour le même
produit auraient le même `id`.

**Ce n'est pas un défaut de `PackfileEntry { id, offset, len }`** — la
structure reste inchangée, comme `bytemuck::Pod`/`#[repr(C)]`/24B alignés.
C'est une confirmation qu'**un template de page est une table logique de
plus**, exactement comme `content.core` et `commerce.product_core` sont déjà
deux tables logiques distinctes, chacune avec son propre fichier
`{schema}_{table}_pack.bin`/`_store.bin` (convention déjà en vigueur, voir
`Projection::packfile_path()`/`store_path()`). Un template de page suit la
même convention, sous un namespace dédié :

```
artifacts/pages/product_public_pack.bin   ← /product/{id}
artifacts/pages/product_admin_pack.bin    ← /admin/product/{id}
```

`Produit 42` a une entrée dans chacun des deux fichiers, sans collision —
le fichier fait partie de la clé, exactement comme aujourd'hui un fragment de
`content.core` et un fragment de `commerce.product_core` partageant le même
`id` numérique ne collisionnent jamais, pour la même raison. Aucune
extension de `PackfileEntry` n'est nécessaire ; uniquement une convention de
nommage de fichier pour les templates de page, symétrique de celle des
templates feuilles.

### 4.5 Capacité d'une page composée — ordre de résolution et règle de sommation

`resolve_and_measure`, pour un `FragmentRef`, doit connaître la capacité déjà
calculée du composant référencé (`{COMPONENT}_TOTAL_CAP`) — pas seulement sa
partie dynamique : appeler `ProductCore::render()` pousse dans le buffer
**tout** ce que `ProductCore` écrit, son propre chrome statique et ses
propres champs dynamiques confondus. La contribution d'un `FragmentRef` à
`total_dynamic_bytes` de la page est donc la capacité **totale** du composant
référencé, traitée comme un pire cas opaque — la page n'a pas besoin de
connaître la décomposition interne statique/dynamique du composant qu'elle
inclut, seulement sa borne globale.

**Conséquence sur l'ordonnancement du build, à acter explicitement** :
`build.rs` doit résoudre tous les templates **feuilles** (un par table, comme
aujourd'hui) avant de résoudre le premier template de **page**, afin que
chaque `{COMPONENT}_TOTAL_CAP` référencé soit déjà connu au moment où le
template de page est traité. C'est une dépendance d'ordre simple (deux
passes séquentielles, feuilles puis pages), pas un graphe à résoudre, **à
condition d'interdire qu'un template de page référence un autre template de
page** — contrainte à acter ici explicitement : un `FragmentRef` ne peut
viser qu'un composant porteur d'une `Projection` (table), jamais une autre
page. Cette restriction évite tout risque de cycle et toute résolution
topologique non triviale ; elle ferme aussi la porte à une réintroduction
détournée de l'héritage de templates (`{% extends %}`/`{% block %}`)
délibérément exclu du périmètre Voie B.

### 4.6 Composants "statiques" (Header/Footer) : pas de mécanisme neuf

Couvert explicitement par les deux taxonomies introduites en §4.2 (côté
Forge : packfile dédié ou non) et §4.3 (côté contenu : statique partagé,
dynamique par entité, ou hors périmètre). `Header`/`Footer` sans variation
par entité relèvent de la première ligne de chaque table : `StaticInclude`,
zéro mécanisme neuf, zéro `packfile` dédié.

---

## 5. Conséquences et travaux différés

- **Spécification Render Shell : redevient triviale, pas "débloquée vers une
  nouvelle complexité".** Sous le modèle retenu, lire une page composée
  coûte exactement la même chose que lire un fragment seul — un unique
  `sendfile()` vers un fichier déjà entièrement écrit. La discussion sur le
  coût O(N) syscalls et `io_uring` (§3.4) reste valide comme **correction de
  vocabulaire** (`sendfile` ≠ scatter-gather), mais son **applicabilité**
  disparaît avec le modèle de composition à la requête qu'elle visait à
  chiffrer — elle ne s'applique à aucun chemin retenu par cet ADR. La spec
  Render Shell peut donc rester alignée sur le modèle originel
  d'`core-system-blueprint.md` §3.A, sans complexité de séquencement
  syscall à spécifier.

- **Énumération des (page, id) à composer au dump — point réellement neuf,
  pas une simple extension.** Sous le modèle à la requête (écarté), l'id
  venait trivialement de l'URL au moment du `GET`. Sous le modèle au dump
  (retenu), il faut désormais **énumérer par avance** quelles pages existent
  et quels ids elles couvrent — symétrique de ce que `marius-dump` fait déjà
  pour un fragment seul (`SELECT id FROM table ORDER BY id`), mais à
  généraliser à des routes paramétrées. C'est un vrai morceau de conception
  restant, pas un détail de grammaire : la spec du compilateur de templates
  de page devra le couvrir explicitement.

- **Grammaire exacte d'`IdSource`** : non spécifiée ici — relève de la spec
  du compilateur de templates de page, en lien direct avec le point
  précédent (d'où vient l'id au moment de l'énumération du dump, pas au
  moment d'une requête qui n'existe plus dans ce rôle).

- **Invalidation d'une page lors de la mutation d'un composant qu'elle
  référence — décision ferme (amendement),
  remplaçant l'hypothèse précédemment laissée ouverte.** Le modèle retenu
  (§4.3) exige que le Dispatcher sache, à la mutation d'un composant, quelles
  pages composées le référencent, pour régénérer les deux artefacts
  (fragment + page) depuis les mêmes données fraîchement fetchées.

  **La Forge (`fragment-forge`) génère cette relation inverse à la
  compilation**, pas le Dispatcher au runtime — cohérent avec la doctrine
  déjà appliquée à `SchemaIndex`/`TemplateMetrics` : tout ce qui peut être su
  à la compilation l'est, jamais recalculé au runtime. Puisque la Forge parse
  déjà l'AST de chaque template de page et connaît chaque `FragmentRef` qu'il
  contient, elle peut émettre, pour chaque composant, la liste statique des
  pages qui le référencent :

  ```rust
  pub static PRODUCT_CORE_DEPENDENT_PAGES: &[PageKind] = &[
      PageKind::ProductPagePublic,
      PageKind::ProductPageAdmin,
  ];
  ```

  Le Dispatcher, à la mutation, effectue un **lookup tableau** (zéro graphe
  résolu au runtime, zéro calcul) pour savoir quelles fonctions
  `render_*_page` invoquer en plus du fragment seul. Le Render Shell
  (`batch_renderer.rs`) reste totalement ignorant de cette distinction —
  il exécute la fonction de rendu et le chemin de fichier qu'on lui passe,
  sans connaître la notion de page ou de dépendance. Le Mode Batch d'ADR-002
  (tick jusqu'à 2000ms, lots contigus) absorbe le coût d'amplification quand
  un composant largement référencé change — c'est exactement le rôle pour
  lequel ce mécanisme a été conçu, avant même cette discussion.

- **`Content-Length`** : propriété inchangée et toujours acquise — la page
  composée a son propre `PackfileEntry.len`, connu à l'écriture, exactement
  comme un fragment seul. Ne dépend plus d'une sommation de N entrées au
  moment de la lecture (cette nécessité disparaît avec le modèle à la
  requête, §3.3).

---

## 6. Principe directeur reconduit

Cet ADR applique le même réflexe qu'ADR-007 (CHECK PostgreSQL réutilisé plutôt
que `pg_description` ou `VARCHAR(N)` par défaut) et que la purge de la Voie A
(suppression du chemin hardcodé plutôt que coexistence avec la Voie B) :
**étendre le pipeline déjà validé jusqu'à preuve qu'il ne suffit pas, plutôt
que d'introduire un artefact ou un mécanisme parallèle par anticipation.**
`CompositionIndex` n'est pas rejeté comme faux — il est rejeté comme
généralisation prématurée. Si l'AST résolu d'un template de page devient, en
pratique, un objet qu'on consulte et fait évoluer comme une structure de
données à part entière, il sera toujours temps de lui donner un nom et un
type dédié à ce moment-là. L'inverse — défaire un artefact conceptuel après
avoir découvert qu'il dupliquait une information déjà présente — coûte
nettement plus.

---

_Rédigé à la suite d'une session de conception collaborative sur la composition de pages HTML depuis des fragments AOT. Conserve la
discussion complète des trois variantes pour référence future — ne pas
supprimer même si la décision §4 est révisée._

---

_22 juin 2026_
