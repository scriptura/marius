# Contrat d'Implémentation — Projection Segmentée

**Fonde sur** : `ADR-010-chunking-of-large-varlena-objects.md` (§4.5, §7) —
alternative retenue, confirmée non bloquée au niveau stockage
(`pack_html_format.rs`, `packfile_builder.rs`, `handlers.rs`) le 22/07/2026.
`CONTRAT-implementation-varlena-raw.md` (déjà livré) fournit `EscapePolicy`,
prérequis de ce Contrat.

**Principe directeur (généralisation, §7 de l'ADR)** : `Segment` est un concept
général — un champ marqué comme segment aujourd'hui est `content.body.content`,
mais le mécanisme ne code en dur aucune notion de « gros varlena » ni de HTML.
Tout composant sans champ segmenté continue d'utiliser le chemin par défaut,
zéro changement de comportement.

**Deux invariants non négociables, hérités de la relecture croisée
(session du 22/07/2026)**, valables pour toutes les étapes :
1. `Projection::render()` (signature actuelle) reste inchangée et continue de
   n'exister que pour `&mut String` — jamais de `Write`/socket/fichier.
2. Aucun composant existant ne doit changer de comportement ou de performance
   mesurable s'il ne porte aucun champ segmenté.

**Choix de conception assumés dans ce Contrat (arbitrables)** :
- Un seul tag SQL, `marius:large_content`, plutôt que deux tags cumulés. Il implique
  automatiquement `EscapePolicy::Raw` — un champ segmenté est par nature
  emprunté zéro-copie (`&str` brut écrit tel quel), incompatible avec un
  passage par `marius_html_escape` (qui exige de recopier caractère par
  caractère). `marius:raw` reste utilisable seul pour un champ raw **non**
  segmenté (HTML pré-rendu, mais assez petit pour rester dans `buf`).
- `render_segments()` est une nouvelle méthode de trait à **implémentation par
  défaut** (délègue à `render()`, un seul segment `Buffered` couvrant tout
  `buf`) — pas une méthode obligatoire à réimplémenter partout. Seuls les
  composants générés avec un champ `marius:large_content` reçoivent une
  implémentation réelle multi-segments.
- Pré-allocation façon `INV-5`/`INV-6` déjà en place ailleurs (`BatchRenderer`,
  `PackfileBuilder`) : le nombre de segments par enregistrement est connu
  statiquement par template — nouvelle constante associée
  `Projection::MAX_SEGMENTS: usize` (défaut `1`), pour pré-réserver
  `Vec<Segment>` une seule fois, jamais de resize en boucle de rendu.

---

### Étape 1 — Tag SQL `marius:large_content` + `introspect.rs`

**Crate** : `crates/forge/db-forge`, `introspect.rs`.
**Contenu** :
- Détection : `"marius:large_content" => { escape_policy: EscapePolicy::Raw, is_segment: true }`, à côté des cas existants (`marius:pre_escaped`, `marius:raw`).
- Nouveau champ `pub is_segment: bool` sur `VarlenField` (fragment-forge, Étape 2).
- Garde-fou explicite (défense en profondeur, même si la construction ne
  devrait jamais produire l'inverse) : `panic!` si `is_segment && escape_policy
  != EscapePolicy::Raw` — incohérence interne à signaler fort, pas à corriger
  silencieusement.
- **Un champ segmenté ne contribue plus du tout à `DYNAMIC_CAP`** — ni facteur
  1 ni facteur 6, contribution nulle. C'est tout l'objet de ce Contrat : un
  varlena ne doit plus dicter la capacité du buffer partagé (ADR-010 §3). Le
  seuil absolu de 64 Ko (ligne ~289) est également contourné pour un champ
  segmenté — il ne passe jamais par `buf`, la limite ne s'applique pas.
- `max_len` reste introspecté normalement (H1-H10, ADR-007 inchangé) — un champ
  segmenté sans borne connue reste soumis à la table de vérité Hot/Cold/Erreur
  existante ; seule sa contribution à `DYNAMIC_CAP` change.
**Dépend de** : `EscapePolicy` (déjà livré).
**Critère de complétion** : test — colonne taguée `marius:large_content` → `is_segment
== true`, `escape_policy == Raw`, contribution à `DYNAMIC_CAP` nulle
indépendamment de `max_len`.

### Étape 2 — `Segment<'a>` (marius_projection) + `VarlenField::is_segment` (fragment-forge) — CLOSE, CRATE CORRIGÉ

**Correction de session (23/07/2026)** : la première rédaction plaçait
`Segment<'a>` dans `crates/forge/fragment-forge`. Erreur repérée en
confrontant `crates/core/projection/src/lib.rs` (définition réelle du trait
`Projection`) et `batch_renderer.rs` : ce dernier dépend de `marius_projection`,
**jamais** de `marius_fragment_forge` (outil de build-time, jamais une
dépendance runtime). `Segment` doit vivre là où le trait `Projection`
le consomme en signature — `crates/core/projection/src/lib.rs` — pas dans le
crate de génération. `VarlenField::is_segment` (booléen build-time pur, jamais
une valeur réelle à l'exécution) reste, lui, dans `fragment-forge`.

**Crate** : `crates/core/projection`, `lib.rs` (`Segment<'a>`) ; `crates/forge/fragment-forge`, `lib.rs` (`VarlenField::is_segment`).
**Contenu** :
```rust
pub enum Segment<'a> {
    /// Plage déjà écrite dans le buffer partagé (`buf[start..end]`).
    Buffered { start: usize, end: usize },
    /// Référence empruntée, zéro copie — jamais recopiée dans `buf`.
    Borrowed(&'a str),
}
```
**Pourquoi `{start, end}` et non `Buffered(&'a str)`** (point soulevé et
tranché en session, 23/07/2026) : `render_segments` continue d'écrire dans
`buf` *après* avoir logiquement « produit » un premier segment (ex. en-tête
déjà écrit, pied écrit plus tard dans le même appel). Un `&'a str` emprunté
sur `buf` et stocké dans `segments: Vec<Segment<'a>>` maintiendrait un prêt
immuable vivant pendant que la fonction continue de faire `buf.push_str(...)`
pour la suite — prêt immuable et mutation simultanés sur le même `buf`, rejeté
par le borrow checker, et à raison : `String` peut réallouer, ce qui
invaliderait toute `&str` prise avant la dernière écriture. Les indices
diffèrent la « vue en `&str` » jusqu'à ce que `buf` soit stable — après le
retour de `render_segments`, quand `BatchRenderer` (qui possède déjà `buf`)
peut re-trancher `&buf[start..end]` sans risque. Ce n'est pas une fuite de
représentation interne : `BatchRenderer` possède déjà `buf` dans son
intégralité, `start`/`end` ne lui apprennent rien qu'il ne pourrait déduire.

Ajout de `pub is_segment: bool` sur `VarlenField`, propagé dans les 4 sites de
construction de test déjà recensés (Étape 2 du Contrat multi-slot + Étape 1 de
ce Contrat) — valeur `false` par défaut pour tous les cas existants.
**Dépend de** : Étape 1 (source de la valeur).
**Critère de complétion** : compilation, tests existants inchangés — **fait**,
`cargo build`/`cargo test`/`cargo clippy` confirmés verts par vous en session.

### Étape 3 — Trait `Projection` : `render_segments()` par défaut — CLOSE

**Crate** : `crates/core/projection`, `lib.rs`. Fichier reçu et confronté en
session (23/07/2026) — signature réelle du trait conforme à ce qui était
inféré (`type Record`, `type VarlenOwned`, `fn render(record, varlena, buf:
&mut String)`, etc.), aucune surprise structurelle (pas d'`#[async_trait]`,
`fetch_from_pg`/`fetch_batch` via `impl Future` natif RPITIT).
**Contenu implémenté** :
```rust
/// Nombre maximal de segments produits par un enregistrement de ce composant.
/// Connu statiquement (généré par db-forge selon le template). Permet à
/// BatchRenderer de pré-allouer son Vec<Segment> une seule fois (INV-5/INV-6).
const MAX_SEGMENTS: usize = 1;

/// Par défaut, délègue à render() — un seul segment Buffered couvrant tout
/// buf. Composants sans champ marius:large_content : comportement inchangé, coût
/// additionnel négligeable (un push() dans un Vec pré-alloué).
fn render_segments<'a>(
    record: &Self::Record,
    varlena: &'a Self::VarlenOwned,
    buf: &mut String,
    segments: &mut Vec<Segment<'a>>,
) {
    buf.clear();
    Self::render(record, varlena, buf);
    segments.push(Segment::Buffered { start: 0, end: buf.len() });
}
```
**Sur la nature de `MAX_SEGMENTS`** (point soulevé en session, 23/07/2026) :
conceptuellement, c'est une propriété du **template compilé**, pas du composant
en tant que tel — mais c'est déjà cohérent avec la mécanique existante : `impl
Projection for {Name}Projection` est lui-même entièrement généré par
`db-forge`/`fragment-forge` à partir du template. La constante par défaut
(`1`) vit sur la définition du trait ; sa valeur réelle par composant est
fixée par le code généré, qui la surcharge selon le nombre de champs
`marius:large_content` réellement présents dans le template. Le trait n'impose
donc rien qui ne soit pas déjà déterminé par la compilation du template
lui-même — pas une propriété arbitrairement rattachée à `Projection`.

**Dépend de** : Étape 2 (`Segment`) — close.
**Critère de complétion** : `cargo build` sur tout composant existant sans
changement de code généré (méthode par défaut jamais surchargée pour eux) —
**à confirmer par vous** (implémenté, non testé en conditions réelles à ce
stade de la session).

### Étape 4 — `BatchRenderer` : boucle de segments — CLOSE

**Crate** : `crates/shell/render`, `batch_renderer.rs`.
**Corrections apportées en cours d'implémentation (23/07/2026), par rapport à
ce qui était prévu ici initialement** :
- **`segments` n'est PAS un champ de `BatchRenderer`**, contrairement à ce que
  cette section prévoyait. `Segment<'a>` emprunte sur `varlena`, dont la durée
  de vie est celle du slice `records` reçu par *cet* appel à `render_batch` —
  différente à chaque appel. En faire un champ aurait figé `BatchRenderer` sur
  une seule durée de vie pour toute son existence, empêchant sa réutilisation
  entre lots. Déclaré en local dans `render_batch`, pré-alloué une fois par
  appel (pas par enregistrement), vidé à chaque itération.
- **`Projection::render_segments()` (Étape 3) ne doit PAS faire `buf.clear()`
  en interne** — corrigé après relecture, avant même d'écrire cette étape :
  une implémentation multi-segments réelle doit pouvoir écrire l'en-tête,
  laisser `buf` intact pendant qu'un segment emprunté est produit, puis
  continuer à écrire le pied à la suite dans le même `buf`. Le nettoyage reste
  la responsabilité exclusive de l'appelant (`render_batch`), exactement comme
  pour `render()` seul aujourd'hui — un seul `self.buf.clear()` par
  enregistrement, avant tout appel à `render_segments`.

**Contenu implémenté** :
- `render_batch` appelle désormais `P::render_segments(record, varlena, &mut
  self.buf, &mut segments)` au lieu de `P::render(...)` directement.
- Boucle d'écriture : pour chaque `Segment` dans l'ordre, `write_all(&buf[start..end])`
  ou `write_all(borrowed.as_bytes())` ; `len` accumulé = somme des tailles de
  segments.
- L'assertion `debug_assert_eq!(self.buf.capacity(), self.total_cap, ...)`
  reste valable telle quelle : `buf` ne contient jamais un champ segmenté, sa
  capacité reste petite et stable pour les composants qui en portent un.
**Dépend de** : Étape 3 — close.
**Critère de complétion** : les 6 tests existants passent sans modification
(composants stub, `MAX_SEGMENTS` défaut = 1) ; 2 nouveaux tests ajoutés avec
un `StubSegmentedProjection` (3 segments réels : en-tête `Buffered`, corps
`Borrowed` de 100 000 caractères — largement au-delà de l'ancien seuil de
64 Ko —, pied `Buffered`) : contenu/index reconstruits correctement, et
`buf.capacity()` inchangée malgré le corps volumineux — **implémenté, non
exécuté en conditions réelles à ce stade de la session, à confirmer par vous**.

### Étape 5 — Codegen : scission du token stream (db-forge/fragment-forge) — CLOSE

**Crate** : `crates/forge/fragment-forge` (`lib.rs`, nouvelle fonction
`generate_segmented_snippet`), `crates/forge/db-forge` (`build.rs` — choix du
générateur ; `codegen/projection.rs::write_projection_stub` — émission de
`render()`/`render_segments()`/`MAX_SEGMENTS`), `codegen/varlen.rs` (mention
`is_segment` dans le commentaire généré, polish).

**Fichier confronté avant d'écrire, dans son intégralité** :
`generate_aot_snippet` (`lib.rs`, fragment-forge) — tous les variants de
`FlatPageToken` couverts (`Static`, `Field`, `IfBool`, `EndIf`, `ScriptStart`/
`ScriptEnd`, `StaticInclude`, `AssetRef`), pas seulement la branche varlena
déjà modifiée à l'Étape 4 du Contrat `varlena-raw`.

**Algorithme implémenté** (`generate_segmented_snippet`) : identique à
`generate_aot_snippet` pour tout token qui n'est pas un champ `is_segment`. Un
champ `is_segment` clôt le run `Buffered` courant (variable générée
`seg_start`, `let mut` déclarée une seule fois en tête de fonction, jamais
re-`let` ensuite — seulement réassignée), pousse sa valeur comme
`Segment::Borrowed` autonome, puis rouvre un nouveau run. **Vérifié à la main
avant d'écrire le code** : un champ segmenté à l'intérieur d'un `{% if %}`
généré produit un résultat correct dans les deux branches d'exécution
(condition vraie/fausse) — la réassignation de `seg_start` à l'intérieur du
bloc conditionnel est sans danger, la variable retient sa valeur d'avant le
bloc si la condition est fausse à l'exécution, prolongeant le run englobant
sans discontinuité.

**`build.rs`** : les deux sites de résolution de template
(`resolve_page_template` — celui qui a produit l'erreur « Mode Page » du
début de session — et `resolve_template`) calculent désormais `has_segment =
varlena.iter().any(|v| v.is_segment)` et appellent
`generate_segmented_snippet` au lieu de `generate_aot_snippet` si vrai. Aucune
valeur à faire transiter par le tuple de retour `(body, metrics)` —
`write_projection_stub` recalcule le même booléen à partir du même `varlena`.

**`codegen/projection.rs`** : si `has_segment`, `render()` devient un stub
`unreachable!()` (jamais appelée — `BatchRenderer` appelle systématiquement
`render_segments()`, Étape 4), et `render_segments()` reçoit le corps réel
(déjà produit par `generate_segmented_snippet` côté `build.rs`) plus une
surcharge `const MAX_SEGMENTS: usize = 2N+1` (`N` = nombre de champs
`is_segment` dans le join — hypothèse documentée : un champ par template,
vraie pour tous les cas réels à ce jour ; sur-approvisionnement sûr sinon,
jamais un bug de correction).

**Point non couvert par ce Contrat, signalé mais non traité** : plusieurs
champs segmentés dans un même template — l'algorithme les gère dans l'ordre
du token stream sans limite de nombre a priori (vérifié par construction),
mais aucun test réel ne l'exerce à ce jour (`content.core` n'en a qu'un).
**Dépend de** : Étapes 1-4 — closes.
**Critère de complétion** : 3 tests unitaires ajoutés pour
`generate_segmented_snippet` (scission simple, scission à l'intérieur d'un
`{% if %}`, cas dégénéré sans champ segmenté) — **implémenté, non exécuté en
conditions réelles à ce stade de la session, à confirmer par vous**. La
validation bout-en-bout complète (code généré réel pour `content.core`,
3 segments) reste à l'Étape 8, après la migration de l'Étape 7.

### Étape 6 — Non-régression

**Contenu** : tout composant sans champ segmenté (100 % des composants réels
à ce jour) continue de produire le même code généré qu'avant ce Contrat —
`render_segments()` n'est jamais surchargée pour eux, l'implémentation par
défaut de l'Étape 3 s'applique telle quelle.
**Dépend de** : Étapes 1 à 5.
**Critère de complétion** : diff nul sur le code généré pour tout composant
sans segment.

### Étape 7 — Migration DDL : `marius:raw` → `marius:large_content` — ÉCRITE, NON EXÉCUTÉE

**Révision de portée (23/07/2026)** : cette étape prévoyait initialement de
laisser la borne `VARCHAR(32000)` inchangée (« hors périmètre »). Revu en
session : cette borne n'avait de raison d'être que pour rester sous l'ancien
seuil de 64 Ko — devenu sans objet pour un champ `is_segment`. La laisser à
32 000 aurait été une contrainte artificielle, sans justification produit,
alors même que c'est exactement ce qui bloquait un `UPDATE` réel avec un
contenu plus long. Les deux changements (retag + relâchement de borne) sont
donc regroupés dans la même migration — ce n'est plus « deux changements »
mais un seul geste cohérent une fois le mécanisme segmenté en place.
**Contenu** : `db/migrations/05_content_body_large_content_tag_and_bound.sql`
— `COMMENT ON COLUMN content.body.content IS 'marius:large_content'` (remplace
`marius:raw`) + `VARCHAR(32000)` → `VARCHAR(2000000)`, scan de validation
contre la nouvelle borne, `content.v_article` recréée (même contournement
DROP/CREATE + regrant qu'aux migrations 02/04).
**Dépend de** : Étapes 1 à 6 — confirmées vertes par vous
(`cargo build`/`test`/`clippy`).
**Critère de complétion** : migration exécutée en conditions réelles,
`cargo build` passe toujours après (le tag ne change rien côté code déjà
livré — seule la donnée introspectée change). **Écrite cette session, pas
encore exécutée.**

### Étape 8 — Validation bout-en-bout

**Contenu** : `UPDATE content.body SET content = <contenu > 64 Ko>` → `NOTIFY`
→ régénération → `pack.bin` régénéré → requête HTTP réelle retourne le HTML
complet, non tronqué, non échappé. Vérifier en particulier que
`{CONTENT_CORE}_TOTAL_CAP` (constante générée) est resté petit (dimensionné
sur l'en-tête/pied statiques seuls), indépendamment de la taille réelle du
contenu de l'article.
**Dépend de** : Étapes 1 à 7, toutes closes.
**Critère de complétion** : identique en rigueur à la validation bout-en-bout
de Phase 1 — exécution réelle, pas relecture de code.

---

## Dépendances entre étapes — résumé

```
1 (tag SQL) ──▶ 2 (Segment enum) ──▶ 3 (trait, FICHIER MANQUANT) ──▶ 4 (BatchRenderer) ──▶ 6 ──▶ 7 ──▶ 8
                                                                      5 (codegen) ─────────────▶ 6
```

## Fichier requis avant de commencer l'Étape 3 — REÇU, ÉTAPE 3 CLOSE

`crates/core/projection/src/lib.rs` reçu et confronté en session (23/07/2026).
Correction notable qui en a découlé : `Segment<'a>` déplacé de
`fragment-forge` vers `marius_projection` (cf. Étape 2 révisée) — c'est ce
fichier même qui a révélé l'erreur de placement initiale.

---

## Addendum (25/07/2026) — régression découverte après clôture du Contrat

**Contexte** : Contrat clos, Étapes 1-8 confirmées vertes en conditions
réelles (`document_id=7`, HTML complet non échappé, `TOTAL_CAP` stable). En
préparant les benchmarks Divan avant une interruption prolongée de
disponibilité, confrontation à trois fichiers jamais couverts par aucune
étape de ce Contrat : `crates/shell/render/src/dispatcher.rs`,
`crates/shell/render/benches/hot_path_render.rs`,
`crates/shell/render/benches/hot_path_certify.rs`.

**Régression trouvée** : `render_batch_pure()` (`dispatcher.rs`) et un test
existant (`test_hot_path_pipeline_stress`) appelaient `P::render(...)`
directement — cassé pour tout composant segmenté depuis l'Étape 5 (`render()`
y est un stub `unreachable!()`). Les deux bancs Divan appelaient de même
`ContentCoreProjection::render(...)` directement dans leurs benchmarks
`render/single/*` et dans `bench_certify_zero_alloc`. Aucun de ces quatre
points de rupture n'a été détecté avant cette vérification — ces fichiers
n'ont jamais fait partie du critère de complétion d'aucune étape (1-8), et
rien n'indique que `cargo test -p marius-render` ait tourné après l'Étape 5.

**Corrigé** (non exécuté en conditions réelles — interruption de
disponibilité) :
- `dispatcher.rs` : `render_batch_pure()` et le test appellent désormais
  `render_segments()`, avec un `Vec<Segment>` local (même raison qu'à
  l'Étape 4 : `Segment<'a>` emprunte sur `varlena`, durée de vie différente à
  chaque appel — pas un champ de struct).
- `hot_path_render.rs`/`hot_path_certify.rs` : les 4 benchmarks `render/single/*`
  et `certify/zero_alloc_in_render` corrigés de même. Une nouvelle section
  (« IV. Benchmarks chemin segmenté ») ajoutée aux deux fichiers : fixtures
  `is_readable=1` + corps HTML de 200 Ko (les fixtures existantes,
  `is_readable=0`, n'exerçaient jamais la branche segmentée) — un benchmark
  de débit (`render/segmented/single_large`, `render/segmented/sequential_large`)
  et une certification zéro-allocation dédiée
  (`certify/zero_alloc_in_render_segments_large_body`), qui échouerait
  spécifiquement si `Segment::Borrowed` venait à copier son contenu au lieu
  de rester une référence zéro-copie — un scénario de régression que la
  certification originale (fixture toujours `content: None`) ne peut pas
  détecter.

**Enseignement, dans l'esprit de `PHASE1-CLOSURE.md` §7** : un Contrat clos et
vert ne garantit que les fichiers qu'il a effectivement touchés ou dont les
tests ont réellement tourné — pas l'absence de rupture dans des fichiers
adjacents jamais réexécutés. La clôture d'un Contrat n'est pas une preuve
d'absence de régression ailleurs dans le crate.

**À confirmer à votre retour** : `cargo build && cargo test -p marius-render
&& cargo bench -p marius-render --bench hot_path_certify && cargo bench -p
marius-render --bench hot_path_render`. Rien de ce qui précède n'a été
exécuté cette session.
