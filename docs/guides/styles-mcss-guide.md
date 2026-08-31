# Styles Marius (.mcss) — guide frontend

Ce guide décrit ce que vous pouvez écrire dans un fichier de style Marius
(`.mcss`) en plus du CSS standard. C'est du CSS normal, avec trois
extensions en plus. Chacune est retirée du fichier avant qu'il n'arrive au
navigateur : ce que vous écrivez ici n'existe que pendant le build.

Règle générale à connaître avant tout le reste : **toute directive mal
écrite ou toute référence qui n'existe pas fait planter le build**, avec un
message d'erreur qui dit précisément quoi et où. Il n'y a pas de mode
dégradé silencieux — si le build passe, c'est que tout ce que vous avez
écrit est correct.

## Variables — `$nom`

Déclarez une variable et réutilisez-la où vous voulez dans le fichier (et
dans les fichiers qui l'importent) :

```css
$primary-color: #3366ff;

.button {
  background: $primary-color;
}
```

- Une variable doit être déclarée avant d'être utilisée quelque part dans le
  graphe de fichiers (le fichier qui la déclare peut être un `@import`
  d'un autre fichier — l'ordre entre fichiers n'a pas d'importance).
- Utiliser une variable qui n'a jamais été déclarée fait échouer le build
  immédiatement. Le message vous propose une correction si le nom
  ressemble à une variable existante (faute de frappe, casse différente).

## Boucles — `@for`

Pour générer une série de règles répétitives :

```css
@for $i from 1 to 4 {
  .col-$(i) {
    width: $(i) * 25%;
  }
}
```

La boucle est entièrement dépliée au build : le fichier final ne contient
plus de `@for`, juste les règles générées. Vous pouvez imbriquer une
boucle dans une autre.

## Mixins — `@mixin` / `@include`

Pour éviter de répéter un même bloc de règles à plusieurs endroits :

```css
@mixin range-block-thumb {
  pointer-events: auto;
  cursor: pointer;
}

.range::-webkit-slider-thumb {
  @include range-block-thumb;
}

.range::-moz-range-thumb {
  @include range-block-thumb;
}
```

Ce qu'il faut savoir sur les mixins :

- **Un `@mixin` se déclare au niveau racine du fichier**, jamais à
  l'intérieur d'une règle ou d'une boucle `@for`.
- **Un `@include` s'utilise n'importe où**, y compris à l'intérieur d'une
  règle imbriquée ou d'un `@for`.
- **Pas de paramètres.** Un mixin est un bloc nommé fixe, pas une fonction —
  s'il vous faut des variantes, utilisez une `$variable` à l'intérieur du
  mixin plutôt qu'un paramètre.
- **Le nom d'un mixin doit être unique dans tout le projet**, pas seulement
  dans le fichier où il est déclaré. Deux fichiers différents ne peuvent pas
  déclarer un `@mixin` du même nom, même s'ils ne s'importent pas
  directement — le build échoue avec les deux emplacements en cause.
- **Un mixin peut inclure un autre mixin**, mais pas au-delà de 3 niveaux
  d'imbrication (mixin A inclut B, B inclut C, C ne doit plus inclure
  personne). Ce n'est pas une limite technique arbitraire : au-delà de 3
  niveaux, retrouver ce qu'un `@include` finit par produire devient
  pénible, et une feuille de style Marius ne doit pas dépendre de chaînes
  de mixins aussi longues pour être comprise. Si vous atteignez cette
  limite, c'est le signal qu'il faut aplatir ou redécouper les mixins
  concernés plutôt que d'en ajouter un niveau de plus.
- Inclure un mixin qui n'existe pas, ou qui forme un cycle (A inclut B qui
  inclut A), fait échouer le build avec un message explicite.

## Ce qui est garanti

- Les commentaires CSS (`/* ... */`) neutralisent tout ce qu'ils
  contiennent : une variable, une boucle ou un mixin commenté est ignoré,
  comme attendu.
- Aucune de ces trois extensions ne survit dans le CSS final : si vous
  voyez un `$`, un `@for` ou un `@mixin`/`@include` dans un fichier généré,
  c'est un bug du pipeline, pas un comportement normal.
- Toute erreur (variable, mixin ou boucle) indique le fichier concerné et,
  quand c'est pertinent, une suggestion de correction.
