# Document 2 — Linker & Lowering

**Contrat d'architecture.** Entrée : `Vec<ParsedPageTemplate<'src>>` produits par le Document 1 (un par fichier impliqué dans une chaîne d'héritage). Sortie : `Vec<FlatPageToken<'src>>` — l'IR canonique, déjà consommable tel quel par le Resolver et le Codegen gelés (Document 3, §3).

Ce document couvre quatre sous-contrats séquentiels : **Arène**, **Collecte de blocs (schéma-libre)**, **Linker**, **Lowering (Normalizer)**. Ils restent quatre fonctions distinctes malgré leur regroupement dans un seul document — la fusion en une seule fonction romprait la règle du §0 (une fonction, une catégorie de concept éliminée).

---

## 1. Principe directeur : lowering irréversible

Contrat d'architecture, pas commentaire de code : **à la sortie de ce pipeline, il est _impossible de construire_ une valeur représentant l'héritage.** Ce n'est pas une propriété vérifiée à l'exécution — c'est une propriété du système de types. `FlatPageToken<'src>` ne possède aucune variante `Block`, `Extends`, ou `TemplateId`. Un consommateur du type ne peut pas, par construction, écrire un `match` qui traiterait un cas d'héritage résiduel — il n'y a pas de bras à écrire, l'exhaustivité du `match` ne l'exige pas.

Chaque sous-contrat ci-dessous élimine une catégorie précise, jamais deux à la fois :

| Sous-contrat      | Élimine                                                                       | Ne touche pas                                         |
| ----------------- | ----------------------------------------------------------------------------- | ----------------------------------------------------- |
| Arène             | « fichier isolé, sans identité stable »                                       | Contenu des tokens                                    |
| Collecte de blocs | « position de bloc inconnue dans le fichier »                                 | Existence du parent, correspondance parent/enfant     |
| Linker            | « correspondance parent/enfant incertaine », « fichier `static` non vérifié » | Contenu effectif substitué                            |
| Lowering          | **héritage lui-même** — `Block`, `TemplateId`, distinction parent/enfant      | Rien : c'est la dernière étape du domaine composition |

---

## 2. Arène

### Responsabilité

Donner une identité stable (`TemplateId`) à chaque fichier admis, pour que les plages (`NamedBlockRange`) puissent référencer leur fichier d'origine sans emprunt auto-référentiel — décision déjà actée par le type `TemplateId` (Phase 3.0, gelé) : « responsabilité du Linker ».

### Structure

```rust
pub struct PageArena<'src> {
    templates: Vec<ParsedPageTemplate<'src>>, // indexé par TemplateId.0
}

impl<'src> PageArena<'src> {
    pub fn admit(&mut self, parsed: ParsedPageTemplate<'src>) -> TemplateId;
    pub fn get(&self, id: TemplateId) -> &ParsedPageTemplate<'src>;
}
```

### Invariants mémoire

`Vec` de croissance linéaire, une entrée par fichier de la chaîne d'héritage (2 dans le cas courant : enfant + parent ; pas de limite structurelle à plus de 2, mais l'héritage multi-niveaux n'est pas couvert par ce contrat — voir §6, point ouvert). `admit` ne fait aucune E/S : le fichier est déjà lu et parsé (Document 1) au moment de l'admission. Aucune donnée n'est copiée — `PageArena` prend possession du `Vec<ParsedPageTemplate<'src>>`, qui contient déjà des emprunts sur les sources.

### Garantie produite

Après admission, toute référence croisée (`NamedBlockRange::template`) est vérifiable par égalité de valeur (`TemplateId` est `Copy`, `Eq`) — une plage extraite du mauvais fichier produit un identifiant qui ne correspond pas à l'arène consultée, donc une valeur détectable par assertion, jamais un contenu haluciné.

---

## 3. Collecte de blocs — schéma-libre

### Responsabilité

Parcourir `&[PageSourceToken<'src>]` d'**un** fichier déjà admis et produire, en une seule passe :

1. `Vec<NamedBlockRange<'src>>` — appariement `BlockOpen`/`BlockEnd` par pile, tagué du `TemplateId` du fichier ;
2. la validation de forme qui ne nécessite aucune connaissance d'un second fichier ni d'un `SchemaIndex` : imbrication de blocs (`NestedBlock`), présence de `{% for %}` (`ForLoopDetected`), mot-clé relationnel (`RelationalKeyword`).

Fusion délibérée des deux responsabilités dans une seule passe — même justification DOD que `resolve_and_measure` (Mode Fragment, gelé) : construire les plages et vérifier leur bien-formation consomment le même flux dans le même ordre, deux parcours gaspilleraient la localité de cache pour un gain de modularité illusoire.

**Ce que cette fonction ne fait pas** : elle ne vérifie ni l'existence du parent, ni la correspondance nom-à-nom enfant/parent (Linker), ni le typage `bool` d'un champ `{% if %}` (nécessite `SchemaIndex`, disponible seulement à l'orchestration — Document 3).

### Signature

```rust
pub fn collect_blocks<'src>(
    template: TemplateId,
    tokens:   &[PageSourceToken<'src>],
) -> Result<Vec<NamedBlockRange<'src>>, Vec<PageValidationError<'src>>>;
```

### Invariants mémoire

Pile de profondeur bornée par l'imbrication réelle du fichier (généralement ≤ 2, jamais allouée avant le premier `BlockOpen`). `Vec<PageValidationError>` fail-slow, alloué au premier `push` uniquement. Aucun emprunt supplémentaire créé : les `NamedBlockRange` produites portent des `usize` (indices), pas de nouvelles références texte.

### Précondition / Postcondition

Précondition : `template` est un `TemplateId` déjà admis en arène pour ce `tokens`. Postcondition (succès) : toute paire `BlockOpen`/`BlockEnd` du fichier est représentée exactement une fois dans le résultat, à profondeur d'imbrication 1 maximum (une imbrication détectée produit une erreur, pas une plage — cohérent avec `NestedIfNotSupported` en Mode Fragment : l'erreur est signalée, la pile reste sur le bloc externe, le parcours continue).

---

## 4. Linker

### Responsabilité

Répondre à des questions de correspondance **par référence**, sans muter aucune structure : le bloc nommé `X` déclaré par l'enfant a-t-il un `BlockOpen` de même nom dans le parent ? Le fichier référencé par `{% static %}` existe-t-il ? Fonction pure modulo E/S injectée (même style que `get_file_size` dans `resolve_and_measure`, gelé et déjà testable sans FS réel).

**Ce que le Linker ne fait pas** : il ne lit ni ne parse le fichier parent lui-même — cette E/S (suivre `extends "path"`, lire, parser via le Document 1, admettre en arène) est une responsabilité de l'orchestrateur (Document 3), pas du Linker, conformément au principe déjà acté : toute l'E/S disque vit dans `build.rs`, la Forge reste pure. Le Linker reçoit un parent déjà admis.

### Signature

```rust
pub struct BlockSubstitution<'src> {
    pub name:   &'src str,
    pub source: NamedBlockRange<'src>, // plage enfant si override, plage parent sinon
}

pub struct LinkPlan<'src> {
    pub substitutions: Vec<BlockSubstitution<'src>>,
}

pub fn link<'src>(
    parent_blocks: &[NamedBlockRange<'src>],
    child_blocks:  &[NamedBlockRange<'src>],
    static_refs:   &[StaticPartialRef<'src>],
    file_exists:   impl Fn(&str) -> bool,
) -> Result<LinkPlan<'src>, Vec<PageLinkError<'src>>>;
```

Règle de construction du plan (contrat, pas algorithme) : pour chaque plage du parent, la substitution retenue est celle de l'enfant si un nom identique existe côté enfant, sinon celle du parent lui-même (comportement par défaut — un bloc non redéfini conserve son contenu d'origine). Toute plage de l'enfant sans correspondance côté parent est un `PageLinkError::OrphanBlock`, jamais silencieusement ignorée.

`file_exists` : vérification d'existence, distincte de la lecture de taille faite plus tard par le Resolver (Document 3, §3). Duplication d'E/S assumée et déjà justifiée dans le scaffolding gelé : trois erreurs de fichier manquant (`ExtendsNotFound`, `StaticFileNotFound`, `ResolverError::IoError`), trois phases, pas de mutualisation prématurée avant l'écriture des trois points d'appel réels.

### Invariants mémoire

`LinkPlan` ne copie aucun texte — `BlockSubstitution::source` porte un `NamedBlockRange` (`Copy`, indices + `TemplateId`), pas de contenu. Fail-slow : toutes les erreurs (blocs orphelins, fichiers manquants) sont accumulées avant retour, à l'image de `resolve_and_measure`.

### Garantie produite

Après cette phase, toute référence de composition — nom de bloc, chemin `static` — est soit résolue à une source concrète, soit rejetée avec une erreur nommée. Aucune référence pendante ne peut franchir cette phase.

---

## 5. Lowering (Normalizer)

### Responsabilité

Dernière phase du domaine composition. Parcourt linéairement `tokens` du **parent**, substitue chaque plage de bloc selon `LinkPlan`, et **projette** chaque `PageSourceToken` restant vers `FlatPageToken`. C'est ici, et uniquement ici, que `Block`, `TemplateId`, `Static(StaticPartialRef)` cessent d'exister.

### Règles de projection (contrat, pas pseudo-code)

- `PageSourceToken::Runtime(t)` → `t` inchangé (`Static`, `Field`, `IfBool`, `EndIf` traversent tels quels).
- `PageSourceToken::Block(BlockOpen{name})` / `BlockEnd` → jamais émis directement ; consommés comme délimiteurs de la plage substituée par `LinkPlan`. Le contenu de la plage retenue (`Vec<PageSourceToken>` du fichier source de cette plage — enfant ou parent selon `BlockSubstitution::source.template`) est projeté récursivement par les mêmes règles.
- `PageSourceToken::Static(StaticPartialRef{original_path})` → `FlatPageToken::StaticInclude { original_path, rel_from_manifest, len: 0 }` — `len` provisoire, résolu par le Resolver gelé exactement comme `{% include %}` (Mode Fragment). **Décision de portée (voir §6)** : chaque occurrence est traitée indépendamment, comme `{% include %}` ; aucune déduplication de bytes au niveau d'une page.
- `PageSourceToken::Unsupported { .. }` → n'atteint jamais cette phase : rejeté en amont par la Collecte de blocs (§3). Si cette invariante était violée, ce serait un bug de la phase amont, pas un cas à gérer ici — le Lowering suppose une entrée déjà validée, comme `generate_aot_snippet` le suppose déjà pour `FlatPageToken`.

### Signature

```rust
pub fn lower<'src>(
    parent_tokens: &[PageSourceToken<'src>],
    plan:          &LinkPlan<'src>,
    arena:         &PageArena<'src>,
) -> Vec<FlatPageToken<'src>>;
```

Pas de `Result` : par construction, toute référence est déjà résolue (Linker passé) — le Lowering est une fonction totale sur une entrée garantie cohérente, à l'image du Normalizer décrit en §4 du document précédent.

### Invariants mémoire

Reconstruction d'un nouveau `Vec<FlatPageToken<'src>>` — allocation build-time, une seule fois. Les fragments texte (`Static`, `Field`, `StaticInclude`) restent des emprunts sur leur source d'origine, quelle que soit l'arène dont ils proviennent (`'src` unique côté type — les fichiers admis dans une même arène partagent nécessairement le même `'src` par construction du build-time, puisqu'ils appartiennent au même passage de compilation).

### Garantie produite (postcondition finale)

`Vec<FlatPageToken<'src>>` structurellement indiscernable d'une sortie de `parse_tokens` (Mode Fragment). À partir d'ici : `validate_ast`, `resolve_and_measure`, `generate_aot_snippet` s'appliquent **sans modification, sans branchement de mode** (Document 3, §3).

---

## 6. Points ouverts — non tranchés par ce document

Consistant avec la méthode déjà appliquée dans le document précédent : ces points sont signalés, pas résolus, pour éviter d'inventer un contrat qui masquerait une décision réelle.

1. **Héritage multi-niveaux.** Ce contrat couvre un seul niveau (`enfant extends parent`). Rien dans `parse_page_tokens` n'empêche syntaxiquement un parent de déclarer lui-même `{% extends %}` (`ParsedPageTemplate::extends` serait alors `Some` pour ce fichier aussi). Ce cas n'est pas gardé par le typage aujourd'hui — à trancher explicitement (interdiction stricte avec erreur nommée, ou support récursif) avant l'implémentation.

2. **Déduplication cross-page de `{% static %}`.** Le Lowering (§5) traite chaque occurrence de `{% static %}` comme un `{% include %}` ordinaire — correct au niveau d'une page (une page compte ses propres octets une fois par occurrence réelle dans son propre flux). Le scaffolding gelé de `StaticPartialRef` documente une intention plus large : partager un **unique** `static_partials::{IDENT}` entre plusieurs pages générées qui référencent le même partiel via un `extends` commun (ex. `nav.html` inclus par `base.marius`, hérité par N tables). Réaliser cette intention exigerait soit une modification de `generate_aot_snippet` (aujourd'hui gelé, hors périmètre de ce document), soit une passe de post-traitement au niveau de l'orchestrateur sur l'ensemble de `generated_schema.rs`. Ce document retient la position dégradée (§5) comme contrat v1, valide et suffisante pour la correction du calcul de capacité par page, et reporte l'optimisation de partage binaire à une session ultérieure.

3. **Extension de `PageComposeParseError`** — déjà signalée au Document 1, §3.

---

_2 juillet 2026_
