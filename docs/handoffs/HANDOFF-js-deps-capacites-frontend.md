# HANDOFF-js-deps-capacites-frontend.md
## Remplacer `main.js`/`more.js` par un système de capacités frontend piloté par `js_deps`

**Statut : complète et affine `HANDOFF-js-runtime-per-page.md` §3-5, qui reste
valide pour tout le reste (§0-2, §5 checklist générale). Document rédigé au
terme d'une session de conception pure — aucune ligne de code, SQL ou Rust
n'a été écrite pendant cette session. Rédigé à partir d'une collaboration
Claude + GPT + propriétaire du système ; toute affirmation ci-dessous est
étiquetée par sa source (voir légende).**

**Révision post-lecture de `crates/assets/src/scripts.rs` (988 lignes,
lu intégralement après la première rédaction) : la tension §5 sur
`[scripts.components]` est résolue (🟢). Deux points sont apparus en
§4.2bis — l'un de conception (le contrat d'activation, désormais clos),
l'autre de **migration** (quels modules candidats sont aujourd'hui
atteignables dans le graphe `main`/`map`) : ce second point ne remet
jamais en cause le contrat lui-même, qui doit rester valide même si aucun
module candidat n'était encore compilé aujourd'hui. Voir checklist §11,
point 1(a) — reformulé en ce sens après audit.**

**Légende :**
- 🟢 **Vérifié sur pièce** — lu directement dans le code/schéma réel cette
  session, référence `fichier:ligne` fournie.
- 🔵 **Décision actée** — arbitrée explicitement par le propriétaire du
  système au fil de la session, non encore implémentée.
- 🟡 **Hypothèse de reconstruction** — cohérente avec les preuves
  disponibles, mais jamais vérifiée directement sur le fichier source
  concerné. À confirmer avant de s'appuyer dessus.
- 🔴 **Ouvert** — question non tranchée, aucune décision prise.

Ne pas convertir un 🟡 en fait acquis sans vérification. Ne pas relancer une
discussion sur un 🔵 sans raison nouvelle — ce sont des arbitrages, pas des
suggestions.

---

## 0. Pourquoi ce document existe

La session a dérivé de « remplacer `main.js`/`more.js` par des imports ESM
inline par page » (objectif initial de `HANDOFF-js-runtime-per-page.md` §3)
vers une question plus large : comment une dépendance JavaScript
conditionnelle au contenu d'une page peut-elle rester une donnée
déclarative, compilée AOT, sans jamais faire fuiter le vocabulaire frontend
dans le Rust ou le SQL métier. Toutes les décisions ci-dessous sont
dispersées dans plusieurs conversations (Claude, GPT, le propriétaire du
système) — ce document les rassemble avant qu'elles ne se perdent.

**Rien n'est implémenté.** Ce document décrit un contrat de conception
arbitré, pas un état du code.

---

## 1. Le problème initial, reconstruit précisément

🟢 `main.js` est aujourd'hui le seul fichier JS chargé sur toutes les pages
du site, déclaré dans `templates/base.marius:22-23` :
```
{% script %}<script src="{% asset main.js %}" type="module"></script>{% endscript %}
```
Son contenu compilé (cité dans `HANDOFF-js-runtime-per-page.md` §1) importe
deux modules hachés séparément :
```js
import{initDisclosureSystem as e}from"/scripts/disclosure.cbb62.js";
import{initNavigation as t}from"/scripts/navigation.a0e14.js";
e(),t();
```

🟢 `base.marius:19-25` déclare aussi, du même patron
(`<script src="{% asset X %}" type="module">`), `leaflet.js` et
`serviceWorker.js` — les trois chargés sur toutes les pages aujourd'hui,
sans aucune conditionnalité.

🟢 `base.marius:31-76` contient le HTML de navigation (`.nav`, `.cmd-nav`,
`.sub-nav`) mais **aucun** bloc `{% script %}` n'y référence `navigation.js`
explicitement — sa présence ne s'explique que par son inclusion dans
`main.js` (voir ci-dessus).

**Reconstruction du problème réel** (🟢 depuis cette mise à jour — confirmé
directement dans `scripts.rs`, plus seulement déduit) : `main.js` importe
aujourd'hui, via ESM natif, un module réellement global (`navigation.js`)
et un module qui ne devrait être chargé que sur les pages utilisant
`.tabs`/`.accordion` (`disclosure.js`). `scripts.rs:621-639` documente
explicitement un bug corrigé en session, en prenant **`navigation.js`
importé par `main`/`index.js`** comme exemple direct — ce n'est plus une
inférence à partir du contenu compilé cité dans le HANDOFF précédent, c'est
nommé tel quel dans le code source du pipeline lui-même. `leaflet.js`,
séparément, est chargé partout via son propre `{% script %}` alors qu'il ne
sert qu'aux pages `.map` (mécanisme précisé en §4, non identique à
`navigation.js` — voir plus bas). C'est la preuve concrète du problème que
ce document cherche à résoudre.

---

## 2. Trois niveaux, jamais à mélanger — architecture actée

```
                    PIPELINE ESM (crates/assets/scripts.rs)
                    "zéro bundling par construction" (§2 du
                    HANDOFF précédent, 🟡 non revérifié cette
                    session — voir §7.7)
                              │
                              ▼
                  modules JS individuels, hachés
                              │
              ┌───────────────┴───────────────┐
              │                                │
       {% script %}                    système de dépendances
       (déclaration opaque,             de capacités frontend
        .marius, AOT)                   (js_deps, par ligne)
              │                                │
              ▼                                ▼
     hoist_and_dedupe_scripts          détection de marqueurs
     + splice_hoisted_scripts          → capacité → bit → modules
              │                                │
              ▼                                ▼
      <!-- MARIUS_SCRIPTS -->          <!-- MARIUS_MODULES -->
      (un <script> par entrée,         (un seul <script type="module">,
       verbatim, inchangé)              imports inline sélectionnés)
```

🔵 **Deux marqueurs distincts, deux sorties distinctes.** La tentative de
fusionner les deux systèmes en un seul collecteur a été explicitement
examinée et écartée (§3). `{% script %}` garde son comportement actuel sans
aucune modification.

🔵 **Le pipeline de compilation ESM (`crates/assets/src/scripts.rs`) n'est
pas remis en cause.** Le sujet de cette session porte uniquement sur *quel
sous-ensemble* de modules est référencé par page, jamais sur la façon dont
un module individuel est compilé/haché.

---

## 3. Pourquoi `{% script %}` et `js_deps` ne peuvent pas fusionner

🟢 **Fait décisif**, `fragment-forge/src/lib.rs:544-561` (commentaire de
doc de `FlatPageToken::ScriptStart`) :

> « Le contenu entre `ScriptStart` et `ScriptEnd` (typiquement un tag
> `<script>` complet écrit par le développeur, avec les attributs de SON
> choix — `defer`, `id`, `integrity`... — **ce Parser n'a et n'aura jamais
> de connaissance de la grammaire HTML `<script>` elle-même**) est capturé
> verbatim. »

`{% script %}` est un mécanisme délibérément opaque : capture verbatim,
hoisting, dédoublonnage structurel, réinsertion telle quelle. Il ne
« sait » jamais ce qu'il transporte — il pourrait porter un
`<script type="application/ld+json">`, un script `defer` sans rapport avec
l'ESM, n'importe quoi. Le convertir automatiquement en ligne `import`
obligerait le compilateur à interpréter un contenu qu'il s'interdit
explicitement de comprendre. **Ce n'est pas une prudence, c'est une
violation d'invariant si on le fait.**

🔵 Conséquence actée : `{% script %}` reste un mécanisme pour des
dépendances/scripts arbitraires, non réductibles à une capacité détectée.
Le développeur peut retirer manuellement une entrée `{% script %}` devenue
redondante une fois qu'une capacité `js_deps` la couvre (cas `leaflet.js`,
voir §4) — mais c'est un geste éditorial au cas par cas, jamais une
transformation automatique de la Forge.

---

## 4. Contrat sémantique de `js_deps` — modèle capacité, pas module

🔵 **Un bit représente une capacité fonctionnelle, jamais un fichier JS.**
Confirmé empiriquement : `.tabs` et `.accordion` sont tous deux servis par
`disclosure.js` (un seul module pour deux marqueurs) — le modèle
« bit = fichier » aurait dupliqué inutilement.

```
capacité
    │
    ├── détecteur(s) : marqueur(s) HTML/CSS
    └── module(s) ESM : point(s) d'entrée
```

🔵 Un bit ne référence que le **point d'entrée fonctionnel** d'une
capacité. Les dépendances ESM transitives d'un module (s'il en importe
d'autres en interne) ne deviennent jamais des bits séparés — le graphe ESM
existant (§2 du pipeline) les résout déjà côté navigateur. Exemple
attendu : la capacité `map` référencerait `map.js` comme point d'entrée ;
si `map.js` importe Leaflet en interne, Leaflet n'a pas de bit propre.

🟢 **Confirmé sur pièce (`map.js`, lu intégralement) : Leaflet n'est pas
une dépendance ESM interne de `map.js`.** Aucun `import` dans tout le
fichier — Leaflet est consommé exclusivement via la globale `L`
(`L.map`, `L.tileLayer`, `L.divIcon`, ...), avec un garde-fou explicite
en tête de `initMaps()` : `if (typeof L === "undefined") { console.warn(...);
return; }`. Ce n'est donc jamais une question de « combien de fichiers
sous le bit `map` » réglable par simple lecture de code — c'est confirmé :
`map.js` a besoin que `leaflet.js` se soit déjà exécuté (effet de bord
global, pas d'activation nommée) **avant** que `initMaps()` soit appelée.

🔵 **Tranché — option C, ni A ni B : faire de `leaflet.js` une dépendance
ESM interne de `map.js`, pas une exception au contrat.** Plutôt que
d'accommoder Marius à l'état actuel du frontend (A : descripteur à
plusieurs entrées ordonnées) ou de laisser une redondance non résolue
(B : `leaflet.js` reste hors `js_deps`), le frontend ajoute lui-même
`import "leaflet.js";` en tête de `map.js`. `map.js` n'utilise jamais `L`
comme liaison importée (toujours la globale `window.L`) — un import pour
effet de bord seul suffit, **aucune modification de `leaflet.js`
lui-même n'est requise**. Les modules ES garantissent qu'un module importé
est intégralement évalué avant que le code du module importeur ne
s'exécute — suffisant pour garantir `window.L` défini avant tout appel à
`initMaps()`, quel que soit le moment de cet appel. Le descripteur reste
strictement `entry` + `activation`, sans champ `dependencies` — confirmé,
pas une nouveauté par rapport à §4.2bis (dépendances ESM transitives
jamais des bits séparés). Généralise proprement à toute future capacité
dépendant d'une ressource `[static.verbatim]`.

🔵 **Condition technique précise pour que ça marche du premier coup —
distincte de la question ESM/non-ESM, jamais soulevée avant ce tour.** La
forme du spécificateur d'import compte. `scripts.rs` distingue deux
chemins de résolution (`scripts.rs:868-871,946-948`, déjà vu §4.2quater) :
un import **relatif** (`./leaflet.js`) serait traité comme un nœud du
graphe ESM propre à `scripts.rs` — tentative de lexer/hacher/réécrire un
fichier déjà traité séparément par `verbatim.rs`, redondance/risque de
conflit. Un import **non relatif** (`import "leaflet.js";` ou une forme
absolue, exactement le patron du test déjà lu) est traité comme externe,
résolu via `resolve_asset_reference`/`AssetUrlRegistry` — réutilise l'URL
déjà hachée par `verbatim.rs`, sans double traitement. **La forme à
utiliser est la seconde, systématiquement, pour toute capacité importateur
d'une ressource vendored.**

🟡 La question abstraite « un descripteur peut-il porter plusieurs
`(entry, activation)` » reste théoriquement ouverte mais n'a plus aucune
instance connue l'exigeant — reversée au même statut que les autres
extensions non actées faute de besoin démontré (multi-export, dépendances
entre capacités, etc., ledger §4.2quater).

🟢 **Contrat validé par un second échantillon indépendant** (`disclosure.js`,
lu intégralement, à jour) : `export const initDisclosureSystem = () => {...}`
— même forme exacte que `map.js` (un export nommé, zéro argument,
interrogation du DOM en interne, `.tabs, .accordion`). `map.js` interroge
`.map` de la même façon. Deux modules d'âges/styles différents,
même contrat externe — bon indicateur que la frontière retenue capture la
bonne chose. Seule différence interne notable, sans impact sur le contrat :
`disclosure.js` est synchrone et immédiat, `map.js` est différé
(`IntersectionObserver`, activation réelle par élément dans une fonction
interne non exportée, `initMap` — à ne pas confondre avec `initMaps`,
seule export publique). Confirme que « activation » doit rester strictement
fire-and-forget, sans attente de complétion — cohérent avec la décision déjà
actée de ne gérer aucun cycle de vie mount/unmount.

🟡 Nommage non uniforme entre les deux modules (`initDisclosureSystem` vs
`initMaps`) — confirme, plutôt qu'infirme, la nécessité d'un champ
`activation` déclaré explicitement par capacité (§4.2bis), jamais dérivé
d'une convention de nommage.

### 4.1 Cartographie des marqueurs — état au terme de cette session

| Marqueur(s) confirmé(s) | Module JS (source : nom de fichier, 🟡 sauf mention) | Capacité | Statut |
|---|---|---|---|
| `.tabs`, `.accordion` | `disclosure.js` | `disclosure` | 🟢 confirmé, code lu intégralement |
| `.figure-image-focus` (préfixe seul — les suffixes `-thumbnail-alignleft`/`-alignright` sont purement CSS, hors périmètre) | `imageFocus.js` | `image-focus` | 🟡 tentatif |
| `.media` | `mediaPlayer.js` | `media-player` | 🟡 tentatif |
| `.video-youtube` | `youtube.js` | `youtube-embed` | 🟡 tentatif |
| `.range` | `range.js` | `range-input` | 🟡 tentatif |
| `.map` | `map.js` (import ESM interne vers `leaflet.js`, à ajouter — §4.2bis, option C retenue) | `map` | 🟢 `map.js` confirmé, code lu intégralement ; migration de l'import Leaflet non encore faite |
| `add-line-marks` | `lineMark.js` | `line-mark` | 🔵 confirmé par le propriétaire |
| `.progress` | `_base.js` (à confirmer — candidat déclaré incertain) | inconnue | 🔴 ouvert, le propriétaire se réserve le droit de corriger |
| `.mosaic` | — | — | 🔵 écarté explicitement, CSS pur, aucun JS |
| `.nav`, `.cmd-nav`, `.sub-nav` | `navigation.js` | **hors `js_deps`** | 🟡 tentatif — présumé global (chargé partout), donc catégorie « toujours actif », pas une capacité conditionnelle. Mécanisme de déclaration actuel non retracé (aujourd'hui : bundlé dans `main.js`, cf. §1) |
| GDPR / consentement (`#gdpr-true-consent`, `#gdpr-false-consent`, présents dans `base.marius`) | inconnu | inconnue | 🔴 ouvert — aucun des 25 fichiers JS listés ne l'évoque clairement ; peut ne pas exister encore comme comportement JS |

**Liste complète des fichiers JS frontend disponibles** (fournie par le
propriétaire du système, aucun de leur contenu n'a été lu) :
```
_base.js, activateOnScroll.js, barChart.js, clientTest.js, codeBlock.js,
disclosure.js, dragList.js, flipCard.js, formAssistance.js,
formMultipleTerms.js, formValidation.js, imageFocus.js, lineMark.js,
map.js, masonry.js, mediaPlayer.js, navigation.js, pieChart.js,
previewImage.js, range.js, readablePassword.js, serviceWorker.js,
svgAnimation.js, svgSpriteToInline.js, youtube.js
```
Six fichiers n'apparaissent dans aucune hypothèse ci-dessus :
`activateOnScroll.js`, `barChart.js`, `clientTest.js`, `codeBlock.js`,
`dragList.js`, `flipCard.js`, `formAssistance.js`, `formMultipleTerms.js`,
`formValidation.js`, `masonry.js`, `pieChart.js`, `previewImage.js`,
`readablePassword.js`, `svgAnimation.js`, `svgSpriteToInline.js` — soit ils
correspondent à des marqueurs pas encore identifiés dans la liste initiale
de 10, soit ils sont eux-mêmes bundlés/consommés par d'autres modules (cas
`svgSpriteToInline.js`, plausible utilitaire interne). **Aucune conclusion
prise — liste fournie ici pour référence de la prochaine session.**

### 4.2bis Réutilisation du manifeste existant — bonne nouvelle et risque précis

🟢 **Bonne nouvelle, vérifiée sur pièce.** Tout module atteint uniquement
par import ESM transitif (jamais déclaré comme cible logique dans
`[scripts.components]`) est quand même individuellement haché **et**
inscrit au manifeste, sous une clé dérivée de son propre nom de fichier
(`scripts.rs:633`, *« Clé : stem du fichier source (ex. "navigation.js")
»*) — correctif explicitement documenté dans le code
(`scripts.rs:621-639`), confirmé par test (`scripts.rs:910-918`,
`manifest.contains_key("navigation.js")`, URL de la forme
`/scripts/navigation.<hash>.js`). **Conséquence directe pour `js_deps`** :
si un module candidat de capacité (`disclosure.js`, `imageFocus.js`, ...)
est déjà importé quelque part dans le graphe atteignable depuis `main` ou
`map`, il possède déjà aujourd'hui sa propre entrée manifeste,
indépendamment de `main.js`/`map.js` — `{% asset disclosure.js %}` serait
donc **déjà résolvable tel quel**, sans aucun nouveau mécanisme de
compilation à écrire.

🔴 **Risque précis, non couvert par la bonne nouvelle ci-dessus.** Cette
garantie ne vaut que pour les modules **atteignables depuis un point
d'entrée déjà déclaré** dans `[scripts.components]` (aujourd'hui : `main`,
`map` — cf. extrait `theme.toml` fourni). Un module candidat qui n'est
importé par **aucun** point d'entrée existant n'apparaît nulle part dans
`build_module_arena` — jamais lu, jamais haché, jamais manifesté, invisible
à `{% asset %}`. **Chacun des candidats du tableau §4.1 doit être vérifié
individuellement** : est-il aujourd'hui importé (directement ou
transitivement) par `main` ou `map` ? Si non, il faudra soit l'ajouter
comme point d'entrée à part entière dans `[scripts.components]` (ou
l'équivalent `[scripts.dependencies.*]` à concevoir), soit l'importer
depuis un module déjà atteint — dans les deux cas, une modification de
`theme.toml`, jamais du code Rust : `run_scripts_pipeline`
(`scripts.rs:576-592`) prend `entry_paths` de façon totalement générique,
rien n'y est spécifique à `main`/`map` — un nouveau point d'entrée n'exige
aucun changement de `scripts.rs` lui-même, seulement une entrée de
manifeste supplémentaire. Ce n'est pas vérifié pour l'instant : personne
n'a confirmé si `imageFocus.js`, `mediaPlayer.js`, `youtube.js`, `range.js`,
`lineMark.js`, `_base.js`, `map.js` sont aujourd'hui importés par `main`/
`map` ou totalement hors graphe.

🟢 **Clos, contrat d'activation.** Un doute a persisté sur plusieurs tours
de cette session (y compris dans ce document, versions précédentes de ce
paragraphe) : le modèle `js_deps` a été décrit à plusieurs reprises comme
des « lignes `import` nues » (effet de bord seul, sans appel) — une
simplification jamais explicitement décidée, qui s'est simplement propagée
sans être corrigée. **Ce n'est pas le contrat retenu.** Le contrat exact
est celui déjà prouvé par `main.js` (`scripts.rs:860-865`, et le `main.js`
compilé cité en §1) : un module expose un export nommé (une fonction),
appelé explicitement, sans argument. Le changement architectural ne porte
jamais sur ce contrat d'activation — seulement sur *qui* l'orchestre :
`main.js` (statique, global, tout le site) devient un programme AOT généré
par page (`<!-- MARIUS_MODULES -->`), qui fait exactement le même
`import{X}from"URL";X();` que `main.js` aujourd'hui, mais sélectivement.

**Contrat minimal retenu** (🟢 pour la partie confirmée par les deux
échantillons disponibles, 🔵 pour la généralisation actée) :
- un module de capacité expose un export nommé unique (fonction) ;
- appelé **sans argument** — cohérence à noter explicitement : c'est ce qui
  permet à `js_deps` de rester un simple bitset de présence (§6). Si un
  module futur exigeait des arguments à l'activation, l'INT8 deviendrait
  insuffisant — ce serait alors un signal remettant en cause §6, pas
  seulement ce paragraphe ;
- un seul appel par page et par capacité active, indépendamment du nombre
  d'instances DOM du marqueur (le module itère en interne — non vérifié
  directement sur un module réel, mais cohérent avec l'existant).

**Descripteur d'activation minimal** (🔵 acté, forme conceptuelle
seulement, pas de syntaxe finale) :
```
capacité
    marqueurs   : [".selectorA", ".selectorB"]
    entry       : chemin du module source
    activation  : nom de la fonction exportée à appeler
```
La Forge connaît ces quatre informations, jamais la logique interne du
module ni ses propres imports (déjà résolus par `scripts.rs` indépendamment
de la Forge). Délibérément, aucun support d'arguments à l'appel n'est
prévu — le décider maintenant sur la base de deux échantillons serait de la
sur-spécification.

**Conséquence sur `FlatPageToken`/`TOTAL_CAP` (§7-8), précision, pas
nouveauté** : chaque bit actif contribue **deux** fragments à l'émission,
pas un — une ligne `import{X as _n}from"URL";` et une ligne d'appel
`_n();` — forme déjà prouvée par le `main.js` compilé (imports groupés,
puis appels groupés, ordre déterministe §4.2). **Les deux fragments
alimentent deux listes séparées (SoA), jamais entrelacées bit par bit** :
tous les imports actifs d'abord, tous les appels ensuite (audité et
confirmé — Gemini — comme la forme correcte pour le moteur JS du
navigateur ; c'était déjà la conséquence mécanique de « deux listes »,
rendu explicite ici pour qu'un futur lecteur ne lise pas « deux fragments
par bit » comme une autorisation d'entrelacer `import A; A(); import B;
B();`). Le majorant `TOTAL_CAP` doit sommer les deux listes, pas une
seule. Les deux générateurs AOT doivent porter cette émission à deux
segments — à vérifier individuellement avant implémentation.

### 4.2quater Dataflow complet de `marius-assets` — vérifié sur pièce

🟢 `crates/assets/src/main.rs` orchestre, séquentiellement, un seul
`HashMap<String, AssetEntry>` accumulé par tous les pipelines :
```
theme.toml → toml::from_str → ThemeConfig (config.rs)
    │
    ▼
verbatim (construit asset_url_registry) → webmanifest (optionnel)
    → sprites → styles → scripts ([scripts.components], Phase 7)
    → [boucle : entry.version = theme.version, sur TOUTES les entrées]
    → service_worker (optionnel, dernier, dépend du manifeste COMPLET)
    │
    ▼
un seul manifest: HashMap<String, AssetEntry>
    │
    ▼
toml::to_string_pretty → écriture UNIQUE de build/{theme}/manifest.toml,
tout à la fin de main()
```

🟢 `run_scripts_pipeline` (appelée `main.rs`, Phase 7) reçoit
`&theme.scripts.components` — un `&HashMap<String, String>` brut, rien
d'autre. Rien dans cette signature n'est spécifique à `main`/`map` :
fonction générique sur son ensemble de points d'entrée.

🔵 **Réponse à la question centrale posée par GPT — réutilisable sans
dupliquer la résolution.** Rien n'empêche d'appeler `run_scripts_pipeline`
une seconde fois (ou avec un `HashMap` fusionné) avec les points d'entrée
d'une future `[scripts.dependencies.*]`, écrivant dans le **même**
`manifest` avant l'écriture finale unique. Même arène de modules, même
hachage, même enregistrement par stem pour les dépendances transitives
(§4.2bis). Zéro nouvelle logique de résolution. 🟡 Corps interne de
`run_scripts_pipeline` non retracé spécifiquement pour le cas d'un second
appel sur le même manifeste — plausible au vu de la signature, non
vérifié formellement.

🔵 **Où loger `markers`/`activation`, la métadonnée que `scripts.rs` ne
connaît pas.** Pas dans `AssetEntry` (`manifest.rs`) — son propre
commentaire de doc l'interdit : *« Invariant de rupture : les noms de
champs doivent rester strictement identiques à la struct lue par
`build.rs`... toute divergence cassera la désérialisation silencieusement
»* (deux structs dupliquées à la main, une par crate, sans partage de
type — découplage déjà noté §4.2quater précédent). La bonne place : un
champ frère de `assets` dans `AssetManifest`, suivant exactement le
patron déjà établi dans `config.rs` pour `WebManifestConfig`/
`ServiceWorkerConfig` — *« configurations PWA autonomes, transmises en
tant que blocs isolés »*, à l'écart de la déstructuration générique
`HashMap<String,String>`. Une nouvelle table `[scripts.dependencies.*]`
suivrait ce même patron côté `ScriptsConfig` (`config.rs`).

🔵 **Correctif retenu — chemin relatif canonique, jamais `AssetID`/Arena.**
Diagnostic confirmé sur les deux versants, symétriquement : `verbatim.rs`
(`logical_key = rel.file_name()...`) réduit au nom de fichier seul côté
écriture, exactement comme `resolve_asset_reference` côté lecture (§4.2bis
précédent). Deux dictionnaires distincts en dépendent
(`AssetUrlRegistry` **et**, indépendamment, l'enregistrement par stem des
modules JS transitifs dans `scripts.rs`) — les deux à corriger, pas un
seul (🟡 le second non revérifié précisément après ce correctif).

**Rejeté explicitement, avec preuve, pas seulement par prudence** :
`AssetID u64`/Arena/SoA (proposé en session pour protéger un « chemin
chaud de la sérialisation »). `crates/assets` (`marius-assets`) n'a
**aucun chemin chaud** — `main.rs` le dit explicitement dans son propre
en-tête : *« Outil de build hôte exclusivement... les allocations
dynamiques sont acceptées sans restriction (contrairement au chemin chaud
du Shell/Core) »*. La discipline zéro-allocation qui gouverne `TOTAL_CAP`
(§7) s'applique à `crates/shell/render`, jamais à `crates/assets`. Toute
proposition de structure de données pour ce crate justifiée par un souci
d'allocation part d'une prémisse fausse.

**Correctif réel, plus modeste qu'estimé au tour précédent** : `verbatim.rs`
dispose déjà, dans le même fichier, de l'utilitaire nécessaire
(`path_to_slash`, déjà utilisé pour `output_rel` quelques lignes plus loin)
— remplacer `rel.file_name()` par `path_to_slash(rel)` comme `logical_key`
est un changement local et trivial. Ce qui reste un vrai travail : côté
lecture (`resolve_asset_reference` et ses quatre appelants), convertir une
référence relative-au-fichier-référençant en chemin canonique relatif à
`theme_dir` avant le lookup — sans souci d'allocation (voir ci-dessus),
donc sans justification à complexifier au-delà d'une jointure/normalisation
de chemin ordinaire.

**Rejeté également, prémisse et pas seulement mise en œuvre : dépendances
entre capacités / tri topologique de capacités.** `map.js` import-ant
`leaflet.js` en interne est un problème entièrement résolu par ESM,
invisible à Marius (§4.2bis, §4.2quater) — jamais une dépendance entre deux
*capacités*. Un graphe `capacité → capacité` (`depends_on = [...]`) serait
une seconde abstraction non demandée par le problème actuel. Aucune
syntaxe proposée pour cette raison — proposer une syntaxe reviendrait à
acter l'abstraction en répondant à la question.

**Invariant d'identité, formulé explicitement (pas seulement illustré par
le correctif ci-dessus)** : toute ressource JS possède une identité
déterminée par son chemin canonique relatif au thème ; le nom de fichier
seul n'est jamais une identité suffisante. La façon dont chaque registre
matérialise cette identité (clé de `HashMap`, autre) est une décision
séparée, postérieure à cet invariant — non tranchée ici.

**Ledger de clôture pour cette sous-discussion (arbitrage explicite,
au-delà du correctif d'indexation) :**
- Acté : dépendances internes JS entièrement du ressort d'ESM ; Marius ne
  connaît que le point d'entrée d'une capacité ; contrat minimal inchangé
  (marqueurs/entry/activation) ; `import != activation` ; activation
  générée inline ; micro-modules (`utils.js`) jamais déclarés à Marius ;
  le mécanisme de détection (classe CSS, `data-*`, autre) est orthogonal
  au contrat d'activation ; aucune gestion de cycle de vie
  HTMX/mount/unmount maintenant ; identité canonique de chemin nécessaire,
  normalisée à la Forge (build-time), jamais au runtime.
- Explicitement non acté, faute de besoin démontré : `AssetID u64`, Arena,
  SoA pour les descripteurs, multi-export par capacité, dépendances entre
  capacités, tri topologique de capacités, scanner DOM générique, support
  d'arguments aux fonctions d'activation.

🔴 Toujours ouvert : la syntaxe exacte de `[scripts.dependencies.*]`
elle-même (forme du nouveau type `Deserialize` dans `config.rs`), et si
l'implémentation choisit un second appel à `run_scripts_pipeline` ou une
fusion du `HashMap` en amont dans `main.rs`.

### 4.2 Ordre des imports

🔵 Le masque (`js_deps`) représente un **ensemble non ordonné**. L'ordre
d'injection entre deux capacités co-occurrentes est une propriété du
**programme AOT** (dérivable une fois pour toutes par la Forge), jamais de
la donnée elle-même. Cohérent avec le fait que `content.core.js_deps` sera
un simple bitset — aucune structure ordonnée à transporter.

---

## 5. `theme.toml` comme source de vérité du vocabulaire

🟢 `assets/default/theme.toml` existe déjà et contient (extrait fourni par
le propriétaire du système) :
```toml
[scripts.components]
main = "scripts/development/main.js"
map = "scripts/development/map.js"

[service_worker]
entry = "scripts/development/serviceWorker.js"
```

🔵 **Décision actée** : `theme.toml` (ou une section associée) devient la
source de vérité déclarative du vocabulaire fonctionnel de `js_deps` — pas
un fichier Rust, pas une table SQL écrite à la main. Conceptuellement (🔴
syntaxe non figée) :
```
capacité frontend
    → marqueurs HTML/CSS
    → entry point(s) JS
```
La Forge devient seule autorité transformant ce vocabulaire en
représentation compilée (bits, mapping SQL, table AOT bit→imports). Ajouter
une capacité (`carousel`, par exemple) ne doit jamais obliger à modifier du
Rust, du SQL métier, ou `SchemaIndex`.

🔵 `SchemaIndex` reste strictement générique : il sait que
`content.core.js_deps` est un `INT8`, il ne connaît jamais le vocabulaire
frontend (`DISCLOSURE`, `MAP`, etc.). Cette fuite de domaine serait une
régression architecturale.

🟢 **Tension résolue — `scripts.rs` lu intégralement (988 lignes).** Le
commentaire de `theme.toml` (*« le compilateur concatène le contenu du
dossier »*) est **inexact**, contredit par le code réel dès son en-tête
(`crates/assets/src/scripts.rs:3-8`) :

> « Assemble et hache les composants en **ES Modules natifs**. **Aucune
> concaténation de bundling n'est effectuée** : chaque module source
> devient un fichier `.js` haché et indépendant. Les directives `import`
> sont réécrites à la volée pour pointer vers les URLs publiques des
> dépendances. »

Confirmé par la signature : `components: &HashMap<String, String>`
(`scripts.rs:580`) associe chaque cible logique (`main`, `map`, ...) à un
**chemin de fichier unique** (`theme_dir.join(&components[*name])`,
`scripts.rs:591`) — jamais un dossier énuméré. `main` est un point d'entrée
JS ordinaire, pas un répertoire à concaténer. Le commentaire de
`theme.toml` est un vestige de documentation à corriger le jour où on y
touche, sans conséquence sur l'architecture — **il n'y a jamais eu de
second mécanisme caché**.

🔵 **Décision actée** : `[scripts.components]` (mécanisme legacy
`main`/`more`/`map`) n'est probablement pas le bon point d'ancrage pour les
nouvelles capacités conditionnelles — une section distincte est à créer
(esquissée par GPT, non figée) :
```toml
[scripts.dependencies.disclosure]
entry = "scripts/development/disclosure.js"
markers = [".tabs", ".accordion"]
```
Pas une décision de syntaxe finale — juste la séparation de niveau
(`components` = quoi compiler / `dependencies` = quelles capacités
conditionnelles existent) qui est actée.

---

## 6. Représentation runtime : `INT8`, pas de varlena

🔵 **Tranché** : `content.core.js_deps` sera un `INT8` (bitset), jamais un
varlena. Le déplacement du vocabulaire vers `theme.toml` (§5) répond au
problème de *qui édite le vocabulaire* ; il ne change rien au problème de
*comment le résultat est stocké/testé à l'exécution*. Un varlena
réintroduirait un TOAST, une taille variable, et un test d'appartenance
O(n) — trois régressions par rapport à `mask & BIT != 0` en O(1), sans
gagner de flexibilité qui ne soit pas déjà acquise par ailleurs.

🟢 Cohérent avec la discipline d'alignement déjà en place dans
`db/05_content/01_components.sql` (`content.core`, commentaire de
tuple : 72 B, blocs 8/4/2/1 octets, `~113 tuples/page`) — un `INT8`
s'insère dans le bloc 8 octets sans rien perturber.

🔴 Emplacement exact du champ dans `content.core` (bloc 8 octets déjà
occupé par `walsn`/`published_at`/`created_at`/`modified_at` = 32 B — un
`INT8` de plus ferait 40 B sur ce bloc, à vérifier contre l'alignement visé)
— non décidé.

---

## 7. Contrainte de capacité mémoire — `TOTAL_CAP`

🟢 **Fait vérifié**, `crates/core/schema/src/lib.rs` : chaque composant a
une capacité de buffer figée à la compilation
(`{NAME}_TOTAL_CAP = {NAME}_STATIC_CAP + {NAME}_DYNAMIC_CAP}`), avec une
assertion explicite en debug/test contre tout `REALLOC` détecté après
`render()` (`buf.capacity()` ne doit jamais changer). Ce n'est pas une
aspiration de documentation — c'est vérifié à l'exécution (cohérent avec
`benches/hot_path_certify.rs`/`counting_alloc.rs`, présents dans l'arbre du
projet).

🔵 **Conséquence actée** : le contenu de `<!-- MARIUS_MODULES -->` (balise
`<script type="module">` finale) doit contribuer à `_TOTAL_CAP` par un
**majorant statique** — somme des longueurs de toutes les lignes `import`
possibles, tous bits confondus — jamais par une mesure dynamique par ligne
au moment de `render()`. Le runtime ne doit faire qu'une **sélection**
dans une table déjà résolue au build, jamais un calcul de longueur ni une
recherche d'asset.

```
BUILD :  theme.toml → capacités → bits assignés → table statique
         (bit → chaîne d'import déjà résolue/hachée) → majorant de taille

RUNTIME : js_deps (bitset) → parcours des bits positionnés uniquement
          (coût ∝ capacités actives, pas 64) → indexation table →
          écriture dans le buffer préalloué → zéro allocation
```

---

## 8. Points d'extension identifiés dans le pipeline `fragment-forge`/`build.rs`

🟢 Séquence exacte, `crates/core/schema/build.rs:758-843` :
```
validate_ast
  → hoist_and_dedupe_scripts / splice_hoisted_scripts   (SCRIPTS_PLACEHOLDER,
                                                           build.rs:46)
  → construction de SchemaIndex { fixed, varlena }
  → resolve_and_measure           (calcule les métriques de capacité)
  → generate_aot_snippet / generate_segmented_snippet   (émission du
                                                           code Rust de render())
```

🔵 Le nouveau mécanisme doit s'insérer **après** le hoisting (`{% script %}`
doit avoir fini de consommer son propre flux de tokens en premier) et doit
être reconnu par **deux** points d'extension, pas un seul :
- `resolve_and_measure` — pour la contribution au majorant de capacité (§7).
- `generate_aot_snippet`/`generate_segmented_snippet` — pour l'émission du
  code de sélection (`record.js_deps` → imports actifs).

🟢 `FlatPageToken` (`fragment-forge/src/lib.rs:511-565` et suivants)
distingue deux familles de tokens par leur variabilité :
- figés à la compilation, identiques pour toute ligne (`Static`,
  `AssetRef`, `ScriptStart`/`ScriptEnd`) ;
- résolus par ligne à l'exécution de `render()`, à partir de `record`
  (`Field`, `IfBool`).

🔵 Le nouveau token doit se comporter comme la seconde famille, mais rester
**générique** — jamais un token par capacité (`JsMap`, `JsDisclosure`, ...
explicitement écartés). Son nom exact n'est pas choisi (candidats évoqués
sans décision : `ModuleDeps`, autre). Son rôle exact, reformulé pour ne pas
laisser le vocabulaire du codegen réintroduire subrepticement
« bit = module » (déjà écarté en §4.1) : permettre au générateur de dire
*« à cet endroit, résous les capacités actives du record courant en leurs
descripteurs d'activation, puis émets les points d'entrée qu'ils
désignent »* — jamais *« émets les modules du `js_deps` »* directement.
Rien de plus : aucune interprétation de `{% script %}`, aucun parsing
HTML, aucune fusion des blocs hoistés.

🔴 Forme exacte du token — délibérément non tranchée, à faire après
vérification du point §5 (tension `[scripts.components]`) et §4 (mapping
marqueur→module complet).

---

## 9. Marqueurs de sortie HTML

🔵 Deux marqueurs distincts dans `<head>`, pas un seul :
```html
<!-- MARIUS_SCRIPTS -->   <!-- existant, inchangé, {% script %} -->
<!-- MARIUS_MODULES -->   <!-- nouveau, js_deps, pas encore implémenté -->
```
Nom retenu pour le second : `MARIUS_MODULES` (préféré à `MARIUS_ESM` —
décrit une catégorie conceptuelle, pas une technologie). 🔴 Emplacement
exact non tranché (juste en dessous de `MARIUS_SCRIPTS`, probable, non
confirmé contre le code de `split_static_at_marker`).

---

## 10. Ce qui reste bloquant côté PostgreSQL, non traité cette session

🟢 `db/05_content/02_systems.sql` (lu intégralement) : les seules
procédures d'écriture du domaine `content` sont `create_document`,
`publish_document`, `save_revision`, `create_tag`, `create_comment`,
`create_media`. **Aucune procédure d'édition post-publication de
`content.body`.** Un trigger `BEFORE INSERT OR UPDATE` sur `content.body`
n'a donc aujourd'hui qu'un seul chemin d'exécution réel (l'INSERT initial
via `create_document`) — pas de second passage à l'édition. Bloquant pour
toute conception de trigger de calcul de `js_deps` tant qu'une procédure
d'édition n'existe pas ou n'est pas identifiée.

🟢 `db/12_events/01_notify.sql` (lu intégralement) : patron de trigger
`AFTER INSERT OR UPDATE OR DELETE` existant sur `content.core`,
`content.identity`, `content.body`, `commerce.product_core` — réutilisable
comme modèle si un trigger `js_deps` est un jour écrit, mais **aucun
trigger de calcul de `js_deps` n'existe** à ce jour.

🔴 Architecture SQL du calcul — trois options désormais, pas deux :
- **A.** Table de définitions provisionnée par la Forge, lue à chaque
  écriture.
- **B.** Fonction PL/pgSQL générée par la Forge, bits câblés en dur —
  penchant existant, pour cohérence avec l'invariant AOT (*« le programme
  ne doit pas devenir un interpréteur runtime »*).
- **C.** (proposée par audit externe — Gemini) Calcul entièrement hors de
  PostgreSQL, dans un service Rust de réception/validation du contenu,
  qui insérerait l'`INT8` déjà calculé. 🔴 **Prémisse non vérifiée** :
  aucun service Rust de cette nature n'a été vu dans ce projet — tout ce
  qui a été lu du domaine `content` (`02_systems.sql`) est appelé comme
  fonctions PL/pgSQL directement, sans couche HTTP/Rust intermédiaire
  visible. C n'est utilisable que si un tel service existe réellement ;
  à vérifier avant de la considérer sérieusement, pas à assumer.
  **Rejetée telle que justifiée** (« éviter le chemin chaud ») : le calcul
  de `js_deps` a lieu à l'écriture d'un article, sur le **chemin de
  régénération**, explicitement distinct du **chemin chaud** (service
  HTTP) depuis la toute première révision du manifeste
  (`manifest-reactive-projection.md` §7). Aucune des trois options n'a de
  contrainte de zéro-allocation à respecter ici — cette contrainte ne
  s'applique qu'au service HTTP (`pread` sur le pack déjà rendu), jamais à
  l'écriture d'un article. Un trigger PL/pgSQL exécuté à la fréquence
  d'édition d'un site (bien plus rare que sa fréquence de lecture) n'est
  un « chemin chaud » nulle part dans le vocabulaire de ce projet.

**Toujours explicitement non tranché entre A et B** — il a été demandé de
ne pas écrire cette fonction avant d'avoir figé §4/§5.

🔴 Mécanisme de détection des marqueurs dans le HTML (`LIKE`/regex sur
`content.body`) — jamais discuté en détail. Point de vigilance confirmé
(audit Gemini, déjà noté ici avant) : un simple `body LIKE '%class="tabs"%'`
serait fragile aux frontières de mot (`class="tabs featured"` vs
`class="not-tabs"` vs classes multiples dans l'attribut) — à traiter
explicitement lors de la conception du calcul, quelle que soit
l'architecture A/B retenue (C écartée ci-dessus). Piste non explorée :
PostgreSQL dispose de fonctions plus robustes qu'un `LIKE` naïf
(`regexp_matches` avec classes de caractères pour les frontières de mot,
voire `xpath`/parsing HTML si le corps est XHTML-valide) — à évaluer en
détail lors de la conception, pas maintenant.

---

## 11. Checklist — prochaine session

Dans l'ordre de dépendance, pas de priorité arbitraire. `scripts.rs` a été
lu intégralement depuis la version précédente de ce document — l'ancien
point 1 (tension `[scripts.components]`) est retiré, résolu (§5).

0. ~~Lire le code de `marius-assets`~~ **Clos** (§4.2quater) : `config.rs`,
   `manifest.rs`, `resolve.rs`, `main.rs` lus intégralement. Dataflow
   reconstruit, point d'ancrage identifié (nouveau champ frère de `assets`
   dans `AssetManifest`, nouvelle table `Deserialize` dans `ScriptsConfig`,
   patron déjà utilisé pour `WebManifestConfig`/`ServiceWorkerConfig`).
   Reste ouvert : vérifier si `run_scripts_pipeline` supporte réellement un
   second appel sur le même `manifest: &mut HashMap` sans effet de bord
   imprévu (🟡 plausible, non vérifié sur le corps de la fonction pour ce
   cas précis) — et la syntaxe exacte de `[scripts.dependencies.*]`
   elle-même, jamais proposée faute de la lire en contexte du parseur réel.
1. **(a)** Point de **migration**, pas de conception — à ne pas traiter
   comme bloquant pour le contrat lui-même, qui doit rester valide même si
   **aucun** des candidats ci-dessous n'était encore compilé aujourd'hui.
   Pour chacun des candidats du tableau §4.1 (`imageFocus.js`,
   `mediaPlayer.js`, `youtube.js`, `range.js`, `map.js`, `lineMark.js`,
   `_base.js`) : est-il aujourd'hui importé, directement ou
   transitivement, par `main` ou `map` (`[scripts.components]`) ? Un module
   hors graphe n'a aujourd'hui aucune entrée manifeste, aucune URL hachée
   — mais déclarer sa capacité dans `[scripts.dependencies.*]` suffira à
   le faire compiler (§4.2quater, réutilisation de `run_scripts_pipeline`)
   : « ce module n'est pas encore dans le graphe » ≠ « l'architecture ne
   sait pas le gérer ». **Toujours ouvert, non bloquant.**
   **(b)** ~~Le module exporte-t-il une fonction d'initialisation nommée
   qui exige un appel explicite, ou s'auto-exécute-t-il à l'import ?~~
   **Clos** (§4.2bis) : le contrat retenu est import nommé + appel
   explicite sans argument, comme `main.js` aujourd'hui — pas
   d'auto-exécution à concevoir. Reste seulement à vérifier, module par
   module, le **nom exact** de la fonction exportée par chacun des
   candidats — nécessaire pour remplir le champ `activation` du descripteur
   (§4.2bis), jamais fait cette session.
2. Confirmer si `_base.js` cible réellement `.progress` (le propriétaire du
   système reste incertain sur ce point).
3. Fixer la syntaxe de `theme.toml` pour `scripts.dependencies.*` (ou
   équivalent) — en incluant, si le point 1(b) l'exige, un champ pour le nom
   de la fonction d'initialisation par capacité.
4. Décider l'architecture SQL A vs B (§10), une fois 1-3 stabilisés.
5. Identifier/créer la procédure d'édition de `content.body` (§10) —
   bloquant pour tout trigger `BEFORE UPDATE`.
6. Concevoir le mécanisme de détection des marqueurs (au niveau SQL ou
   ailleurs) avec la précaution de frontière de mot (§10).
7. Nommer et concevoir le nouveau `FlatPageToken` (§8), seulement après 1-6.
8. Décider de l'emplacement exact de `<!-- MARIUS_MODULES -->` (§9).
9. Revoir le sort de `main.js`/`more.js` en tant que fichiers compilés une
   fois la nouvelle balise en place — disparaissent, ou restent en
   fallback ? (repris de `HANDOFF-js-runtime-per-page.md` §5, toujours
   ouvert). Vérifier au passage si `more` existe encore comme cible dans le
   `[scripts.components]` réel — l'extrait de `theme.toml` fourni cette
   session ne déclare que `main`/`map`, sans `more` ; le test
   `scripts.rs:847-951` utilise `main`/`more` mais reproduit un scaffolding
   antérieur, pas nécessairement l'état actuel. **`leaflet.js` a un sort
   désormais clair, distinct de `main.js`/`more.js`** (§4.2bis, option C) :
   une fois `import "leaflet.js";` ajouté dans `map.js`, son bloc
   `{% script %}` dans `base.marius:19-20` doit être retiré — sinon
   Leaflet serait chargé deux fois (une fois via `{% script %}`, une fois
   via l'import interne de `map.js`), silencieusement, sans erreur de
   build pour le signaler.

---

## 12. Vérification finale du pipeline JS — clôture de la conception

🟢 **Trois points vérifiés sur pièce, aucune incompatibilité avec le
contrat `js_deps`.** Cette section clôt la dernière inconnue
infrastructurelle avant implémentation.

**Minification (`js_minify.rs` + `scripts.rs`)** : `patch_and_hash_modules`
(`scripts.rs:513-574`) exécute, dans cet ordre strict : résolution/
réécriture des imports (528-545) → `minify_javascript` (551-552) →
`hash_content` sur les octets minifiés (554) → écriture disque + URL
(561-566). Commentaire du code : *« le hash doit porter sur les octets
RÉELLEMENT servis »*. `js_minify.rs` est orthogonal au mécanisme
`js_deps` — confirmé, pas supposé. Les noms exportés survivent au
mangling (testé bout en bout, `scripts.rs:928-939`), cohérent avec le
contrat `import{X as _n}from...`.

**Hash → URL** : `PatchedModule.url` (calculé une fois, `scripts.rs:566`)
alimente `AssetEntry.url` aussi bien pour les cibles déclarées que pour
les dépendances transitives par stem — un seul calcul, jamais recalculé.
Le test `scripts.rs:920-939` vérifie la chaîne complète : le fichier
`main.js` réellement écrit sur disque contient, dans son import réécrit,
l'URL **exacte** de `manifest["navigation.js"].url` — pas un préfixe
coïncident.

**Concaténation** : confirmée absente. `patch_and_hash_modules` traite
chaque nœud de l'arène individuellement (`scripts.rs:523`) — un seul site
`fs::write` dans tout le fichier (ligne 563), un fichier par module. Le
mot « concaténation » dans le commentaire de `theme.toml` (§5) n'a jamais
eu de contrepartie dans le code réel.

**Conséquence** : le graphe ESM/AOT que `js_deps` doit consommer
correspond exactement à ce que `crates/assets` produit réellement. Aucune
modification requise dans `js_minify.rs`, `verbatim.rs`, ou la logique de
hachage/URL de `scripts.rs` pour accueillir le nouveau mécanisme — ces
trois éléments sont déjà, par construction, compatibles avec un modèle de
modules individuels important explicitement leurs dépendances.

**Handoff considéré clos pour la phase de conception à l'issue de cette
vérification.** Les 🔴/🟡 restants (checklist §11) sont des vérifications
et décisions d'implémentation localisées, pas des inconnues
architecturales.

---

_Rédigé à l'issue d'une session de conception pure, sans implémentation.
Remplace `HANDOFF-js-runtime-per-page.md` §3-5 ; §0-2 et la structure de ce
document restent la référence pour le contexte général (pipeline ESM,
`{% script %}` conservé tel quel). Toute reprise de ce document par une
nouvelle session doit revérifier les éléments 🟡 avant de les traiter comme
acquis — ne jamais requalifier un 🟡 en 🟢 sans relecture directe du fichier
source concerné._
