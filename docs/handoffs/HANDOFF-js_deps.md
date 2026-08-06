# HANDOFF-js_deps.md
## Bitmask de dépendances JS, calculé au write-time

---

## 0. Pourquoi cette fonctionnalité existe

Marius est un moteur de rendu HTML **AOT** : chaque page est produite par une
fonction `render()` générée à la compilation, qui écrit dans un buffer
pré-alloué sans allocation dynamique. Le principe directeur du projet est de
décider le plus tôt possible (au *build*, ou à défaut à l'*écriture* en
base) tout ce qui peut l'être, pour ne jamais recalculer au moment de
servir une requête.

Aujourd'hui, le template d'un article (`content.core`) inclut
**inconditionnellement** plusieurs balises `<script>`, dont au moins une
bibliothèque tierce potentiellement lourde (`leaflet.js`, pour l'affichage
de cartes). Chaque page transfère donc ce script, même les articles qui
n'en ont pas l'usage.

**`js_deps` doit rendre cette inclusion conditionnelle, décidée à l'écriture
de l'article, pas au moment du rendu HTTP.** Concrètement : quand un article
est créé ou édité, on détermine une fois pour toutes quels scripts optionnels
son contenu utilise réellement (présence d'une carte → `leaflet`, etc.), on
stocke ce résultat comme métadonnée statique sur la ligne, et `render()`
n'émet que les balises `<script>` correspondantes — zéro calcul, zéro
branchement coûteux au moment de servir la page.

---

## 1. Décision actée — bitmask, pas varlena

**`js_deps` sera un champ `INT8` sur `content.core`, un bit par script
optionnel possible.** Pas un tableau varlena (`TEXT[]` ou équivalent).

Raisons :
- Le vocabulaire de scripts optionnels est **fermé et petit** (30 à 50 clés
  maximum, confirmé) — un `INT8` (64 bits) tient large.
- L'**ordre d'injection** entre deux scripts donnés, quand ils co-occurrent,
  est toujours le même (ex. une dépendance `leaflet` → `leaflet-plugin`
  serait toujours dans cet ordre) — jamais un choix propre à un article
  donné. `render()` peut donc itérer les bits dans un **ordre canonique fixé
  à la compilation** (dérivé du manifeste de scripts, cf. §2.1), sans jamais
  avoir besoin de stocker un ordre par ligne — ce qui achève de rendre le
  bitmask suffisant.
- Un champ varlena (texte à échapper ou HTML à injecter tel quel) serait un
  mécanisme inadapté à un ensemble fermé de clés : il faudrait inventer une
  sérialisation et un parsing au rendu (allocation par lecture), pour un
  gain nul face à un test de bit direct.

**Marge de croissance** : si le vocabulaire dépassait un jour 64 clés, un
bitmask multi-mots est un pattern déjà existant ailleurs dans le projet
(recherche `WORDS` dans le crate `collector`) — non nécessaire aujourd'hui,
à ne pas anticiper.

---

## 2. Ce qui reste à concevoir

### 2.1 Vocabulaire — correspondance bit ↔ clé de script

Le vocabulaire fermé des scripts existe déjà dans le manifeste de gestion
d'assets du projet (fichier `manifest.toml`, côté outillage `marius-assets`
— **ce fichier n'a jamais été audité**, à fournir en priorité). Reste à
concevoir : comment ce vocabulaire devient une correspondance stable
« position de bit → clé de script », lisible à la fois par PostgreSQL (pour
écrire le bitmask) et par le compilateur Rust (pour générer les
`if bits & ... != 0` correspondants). Aucune piste actée — trois esquisses
possibles à évaluer, sans préférence tranchée :
- Une table `meta.js_deps_vocabulary(bit_position, script_key)`, synchronisée
  depuis `manifest.toml` par un script ou manuellement.
- Une fonction PL/pgSQL figée, régénérée à chaque évolution du manifeste.
- Une lecture directe de `manifest.toml` par le générateur Rust au moment du
  `cargo build`, sans passer par une table SQL intermédiaire.

### 2.2 Déclencheur et calcul du bitmask

Le calcul revient probablement à une fonction PL/pgSQL (scan du corps HTML
de l'article à la recherche de marqueurs qui impliquent tel ou tel script —
règle de détection non conçue, cf. §2.3), déclenchée par un trigger sur
l'écriture de `content.body`.

**Point bloquant à vérifier avant d'implémenter** : à ce jour, seule la
procédure de création initiale d'un article écrit `content.body` — aucune
procédure d'édition après publication n'a été confirmée existante. Si c'est
toujours le cas :
- soit cette procédure d'édition doit être construite en préalable (portée
  probablement plus large que `js_deps` seul — un article publié devrait
  pouvoir être modifié, indépendamment de cette fonctionnalité) ;
- soit on décide explicitement que `js_deps` n'est calculé qu'à la création,
  jamais recalculé après édition — un choix produit à valider en session,
  pas à supposer silencieusement (un article édité garderait alors un
  `js_deps` figé sur son état initial).

**À fournir en session** : le code SQL réel de la procédure de création
d'article et de toute procédure d'édition/révision existante, pour trancher
ce point avec le code sous les yeux plutôt que par supposition.

### 2.3 Règle de détection — marqueur → script

Non conçu. Si le déclencheur est un scan du corps HTML (§2.2), il faut une
règle explicite associant un motif détectable (ex. une classe CSS, une
balise) à chaque script optionnel. Aucune règle actée à ce jour.

### 2.4 Lecture au rendu

Le mécanisme de résolution d'assets (`{% script %}` ou équivalent côté
template) existe déjà et gère la résolution de chemin/versioning des
fichiers JS — seul le **déclenchement conditionnel** de son inclusion par
article est nouveau. Pas de nouveau mécanisme de résolution à écrire ici,
seulement la condition qui décide de l'appeler ou non pour un script donné.

---

## 3. À vérifier en ouverture de session, sans lien direct avec `js_deps`

Un travail d'évolution du rendu est prévu entre ce handoff et sa reprise :
une gestion d'**états de page** (connexion, signature utilisateur) visant à
éviter une explosion du nombre de copies de rendu. C'est un problème de la
même famille que `js_deps` — produire un rendu conditionnel sans dupliquer
tout le pipeline de génération. **Vérifier en premier lieu si ce travail a
produit un mécanisme générique réutilisable pour l'inclusion conditionnelle**
avant de construire quoi que ce soit de spécifique au bitmask — pourrait
éviter de dupliquer un effort de conception.

---

## 4. Checklist de démarrage

1. Fournir `manifest.toml` (`marius-assets`) — §2.1.
2. Fournir le code SQL réel de création/édition de `content.body` — §2.2.
3. Vérifier si le travail sur les états de page (§3) a produit un mécanisme
   réutilisable.
4. Concevoir la règle de détection marqueur → script (§2.3).
5. Séquencer l'implémentation : vocabulaire → colonne bitmask sur
   `content.core` → déclencheur write-time → lecture conditionnelle au
   rendu.

---

_Rédigé le 26 juillet 2026. Document de travail — décrit un besoin et son
état d'avancement, pas un historique de session à conserver tel quel._
