# Scénario — ajouter un script ou une bibliothèque JS

Ce mémo est pour vous si vous voulez ajouter du JavaScript à une page — le
vôtre, ou une bibliothèque récupérée ailleurs (npm, CDN téléchargé, etc.) —
sans avoir besoin de comprendre comment tout ça fonctionne en coulisses.
Pour les détails techniques : `scripts-librairies-capacites-frontend-guide.md`.

## Une seule question à se poser

**Ce script doit-il être présent tout le temps, ou seulement quand c'est
utile ?**

| Votre besoin | Recette |
|---|---|
| Un script à moi, chargé sur certaines pages, sans condition | A |
| Un script à moi, qui ne doit apparaître que si le contenu en a besoin (une vidéo, une carte, un carrousel...) | B |
| J'utilise le code de quelqu'un d'autre (une bibliothèque JS) | C |

## Recette A — un script à moi, toujours chargé

1. Placez votre fichier dans `assets/default/scripts/...`.
2. Donnez-lui un nom dans `theme.toml` :
   ```toml
   [scripts.components]
   mon-script = "scripts/mon-script.js"
   ```
3. Utilisez-le dans le template concerné avec `{% asset mon-script.js %}`
   (vérifiez la syntaxe exacte avec un dev si le tag ne se comporte pas
   comme attendu — la convention peut varier légèrement selon le
   template).
4. Relancez la commande de build des assets (demandez-la si vous ne
   l'avez pas) et rechargez la page.

C'est tout — pas de condition, pas de base de données à toucher.

## Recette B — un script qui n'apparaît que si nécessaire

C'est le système appelé `js_deps`. Deux ingrédients :

- Une **classe HTML** qui signale le besoin — par exemple `class=
  "carousel-embed"`, posée soit par un éditeur dans le contenu d'une page,
  soit directement dans un template si le besoin est systématique pour ce
  template.
- Un **bit** attribué à cette classe côté base de données, pour que le
  serveur sache quelle case tester.

Étapes :

1. Écrivez votre script comme une fonction exportée :
   ```js
   export function boot() {
     // ...
   }
   ```
2. Déclarez-le dans `theme.toml`, avec le nom de la classe qui doit le
   déclencher (voir §5.3 du guide technique pour le détail exact des
   fichiers à modifier).
3. **Un développeur backend doit ajouter ce bit côté base de données** —
   c'est la seule partie que vous ne pouvez pas faire seul(e).
4. Une fois fait, posez la classe dans un contenu (déclenchement au cas
   par cas) ou dans un template (déclenchement systématique pour ce
   template).

## Recette C — utiliser une bibliothèque externe

1. Récupérez les fichiers de la bibliothèque tels quels — pas besoin de
   les transformer — et déposez-les dans `assets/default/libraries/<nom>/`.
2. Déclarez-la dans `theme.toml` :
   ```toml
   [libraries.ma-lib]
   root = "libraries/ma-lib"
   ```
3. Dans votre propre script (Recette A ou B), importez-la par son chemin
   complet, pas par un chemin relatif :
   ```js
   import { Truc } from "libraries/ma-lib/ma-lib.js";
   ```
4. Relancez la commande de build des assets.

## Pourquoi ce n'est pas magique

Deux choses se passent automatiquement à chaque build :

- **Chaque fichier est renommé avec une empreinte unique** (ex.
  `main.a81f9.js`), pour que les navigateurs puissent le garder en cache
  indéfiniment sans jamais servir une vieille version par erreur. Vous
  n'écrivez jamais ce nom généré vous-même.
- **Vos `import` sont réécrits automatiquement** pour pointer vers le bon
  fichier renommé. C'est pour ça que vous écrivez un chemin clair et
  stable (`libraries/ma-lib/ma-lib.js`) et que ça fonctionne malgré le
  renommage — le build fait la traduction à votre place.

**Une limite à connaître (Recette C) :** si la bibliothèque que vous
ajoutez importe elle-même d'autres fichiers en interne, ces
références-là ne sont *pas* réécrites — seuls vos propres scripts (A et B)
en bénéficient. Préférez une bibliothèque livrée en un seul fichier, ou
vérifiez qu'elle gère ses propres chemins internes autrement (bundle déjà
assemblé par son éditeur, par exemple).

## Si quelque chose ne marche pas

- **Le build échoue avec un message clair** (fichier introuvable,
  bibliothèque non déclarée, chemin incorrect) → corrigez ce que le
  message indique, c'est fiable.
- **Le build est vert, mais rien ne change à l'écran** → ce n'est presque
  jamais votre script. Demandez à un backend de vérifier que la page a
  bien été régénérée côté serveur après le déploiement (§6 du guide
  technique) — c'est l'étape la plus souvent oubliée, et elle ne prévient
  jamais par une erreur.
