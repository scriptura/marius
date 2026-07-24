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

### Étape 2 — `Segment<'a>` + `VarlenField::is_segment` (fragment-forge)

**Crate** : `crates/forge/fragment-forge`, `lib.rs`.
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

Ajout de `pub is_segment: bool` sur `VarlenField`, propagé dans les 3 sites de
construction de test déjà recensés (Étape 2 du Contrat multi-slot) — valeur
`false` par défaut pour tous les cas existants.
**Dépend de** : Étape 1 (source de la valeur).
**Critère de complétion** : compilation, tests existants inchangés.

### Étape 3 — Trait `Projection` : `render_segments()` par défaut

**Crate** : `crates/core/projection` — **fichier jamais vu cette session**
(définition du trait `Projection` lui-même). Signature actuelle inférée par
usage (`StubProjection` dans `batch_renderer.rs`) : `type Record`, `type
VarlenOwned`, `fn fetch_batch`, `fn render(record, varlena, buf: &mut
String)`, `fn record_id`, `fn packfile_path`, `fn store_path`, `fn
store_registry`. **Ne pas commencer cette étape sans ce fichier** — la forme
exacte du trait (object-safety, generics existants, éventuel
`#[async_trait]`) doit être confrontée avant d'y ajouter quoi que ce soit.
**Contenu prévu** :
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

**Dépend de** : Étape 2 (`Segment`).
**Critère de complétion** : `cargo build` sur tout composant existant sans
changement de code généré (méthode par défaut jamais surchargée pour eux).

### Étape 4 — `BatchRenderer` : boucle de segments

**Crate** : `crates/shell/render`, `batch_renderer.rs`.
**Contenu** :
- Nouveau champ `segments: Vec<Segment<'a>>` (durée de vie liée au batch
  courant), pré-alloué à `P::MAX_SEGMENTS` dans `new()` — même discipline que
  `buf`/`index`.
- `render_batch` appelle désormais `P::render_segments(record, varlena, &mut
  self.buf, &mut self.segments)` au lieu de `P::render(...)` directement.
- Boucle d'écriture : pour chaque `Segment` dans l'ordre, `write_all(&buf[start..end])`
  ou `write_all(borrowed.as_bytes())` ; `len` accumulé = somme des tailles de
  segments (inchangé dans son principe, cf. `PackfileEntry.len`).
- `self.segments.clear()` après écriture, avant le prochain enregistrement —
  même pattern que `buf.clear()`.
- L'assertion `debug_assert_eq!(self.buf.capacity(), self.total_cap, ...)`
  reste valable telle quelle : `buf` ne contient jamais un champ segmenté, sa
  capacité reste petite et stable pour les composants qui en portent un.
**Dépend de** : Étape 3.
**Critère de complétion** : les 6 tests existants de `batch_renderer.rs`
passent sans modification (composants stub, `MAX_SEGMENTS` défaut = 1) ; un
nouveau test avec un `StubProjection` segmenté (2-3 segments synthétiques,
`Borrowed` incluant une chaîne dépassant volontairement l'ancien seuil de
64 Ko) vérifie l'index physique final (`offset`/`len`) et le contenu
reconstruit depuis le fichier, comme `packfile_content_matches_index`.

### Étape 5 — Codegen : scission du token stream (db-forge/fragment-forge)

**Crate** : `crates/forge/fragment-forge` (génération), `crates/forge/db-forge`
(orchestration `codegen/projection.rs`/`build.rs`).
**Contenu** : pour un template référençant un champ `is_segment == true`,
scinder le `FlatPageToken` stream en runs statiques (avant/après/entre les
tokens segmentés), générer `render_segments()` au lieu de `render()` :
chaque run statique écrit dans `buf` (comme aujourd'hui), clôturé par un
`Segment::Buffered{start,end}` ; chaque champ segmenté devient un
`Segment::Borrowed({field}_ref)` intercalé dans l'ordre du template.
**Point non couvert par ce Contrat, à signaler explicitement s'il survient** :
plusieurs champs segmentés dans un même template — l'ordre relatif doit
suivre le token stream, sans limite de nombre a priori, mais aucun cas réel ne
l'exerce à ce jour (`content.core` n'en a qu'un). Ne pas complexifier pour un
cas hypothétique — traiter au moment où il se présente réellement.
**Dépend de** : Étapes 1-4, toutes closes.
**Fichier à confronter avant d'écrire** : le corps actuel de
`generate_aot_snippet` (déjà lu en partie, Étape 4 du Contrat varlena-raw) —
relire dans son intégralité avant de le restructurer, pas seulement la
branche varlena déjà modifiée.
**Critère de complétion** : code généré pour `content.core` (une fois retagué
`marius:large_content` à l'Étape 7) produit `render_segments()` avec 3 segments
(en-tête statique, corps emprunté, pied statique) — vérifié par lecture du
code généré, puis par test d'intégration réel (Étape 8).

### Étape 6 — Non-régression

**Contenu** : tout composant sans champ segmenté (100 % des composants réels
à ce jour) continue de produire le même code généré qu'avant ce Contrat —
`render_segments()` n'est jamais surchargée pour eux, l'implémentation par
défaut de l'Étape 3 s'applique telle quelle.
**Dépend de** : Étapes 1 à 5.
**Critère de complétion** : diff nul sur le code généré pour tout composant
sans segment.

### Étape 7 — Migration DDL : `marius:raw` → `marius:large_content`

**Contenu** : `COMMENT ON COLUMN content.body.content IS 'marius:large_content';`
(remplace le tag `marius:raw` posé en Étape 7 du Contrat varlena-raw — un seul
tag à la fois, `marius:large_content` implique `Raw` désormais). Occasion naturelle
de reconsidérer la borne `VARCHAR(32000)` (choix PoC lié à l'ancien seuil de
64 Ko, qui ne s'applique plus à un champ segmenté) — **hors périmètre de ce
Contrat**, à traiter séparément si souhaité, pour ne pas mélanger deux
changements dans une même migration.
**Dépend de** : Étapes 1 à 6, buildées et testées réellement.
**Critère de complétion** : migration exécutée, `cargo build` passe.

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

## Fichier requis avant de commencer l'Étape 3

`crates/core/projection/src/lib.rs` (définition du trait `Projection`) —
jamais vu cette session, indispensable avant d'y ajouter `render_segments()`
et `MAX_SEGMENTS`.
