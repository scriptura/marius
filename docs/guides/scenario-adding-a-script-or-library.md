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

### Mon script est-il une capability (B) ou un component (A) ?

Une seule vraie différence : **a-t-il une condition d'apparition ?**
- Non, il doit juste être là quand le template l'inclut → **component**
  (Recette A).
- Oui, il ne doit apparaître que si une classe précise est présente (posée
  par un éditeur dans le contenu, ou en dur dans un template) →
  **capability** (Recette B).

Une capability peut faire tout ce qu'un component fait, avec en plus cette
condition — mais elle demande deux fichiers de configuration
supplémentaires (voir Recette B). Si vous n'avez pas besoin de condition,
un component est plus simple.

## Recette A — un script à moi, toujours chargé

1. Placez votre fichier dans `assets/default/scripts/...`.
2. Donnez-lui un nom dans `theme.toml` :
   ```toml
   [scripts.components]
   mon-script = "scripts/mon-script.js"
   ```
3. Utilisez-le dans le template concerné avec `{% asset scripts/mon-script.js %}`
   — c'est le **chemin relatif du fichier**, sans le `/` racine.
4. Relancez la commande de build des assets (demandez-la si vous ne
   l'avez pas) et rechargez la page.

C'est tout — pas de condition, pas de base de données à toucher. Un
component ne peut pas dépendre d'une bibliothèque via la Recette E
ci-dessous (`deps`) — seule une capability le peut. S'il a besoin d'une
bibliothèque, importez-la directement (Recette D).

## Recette B — un script qui n'apparaît que si nécessaire

C'est ce qu'on appelle une « capability ». Deux ingrédients :

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
   déclencher (voir §6.3 du guide technique pour le détail exact des
   fichiers à modifier).
3. **Un développeur backend doit ajouter ce bit côté base de données** —
   c'est la seule partie que vous ne pouvez pas faire seul(e).
4. Une fois fait, posez la classe dans un contenu (déclenchement au cas
   par cas) ou dans un template (déclenchement systématique pour ce
   template).

Rien à écrire dans votre `.marius` au-delà de la classe elle-même — la
balise `<script>` de la capability est générée et injectée automatiquement
partout où c'est nécessaire.

## Recette C — déclarer une bibliothèque externe

1. Récupérez les fichiers de la bibliothèque tels quels — pas besoin de
   les transformer — et déposez-les dans `assets/default/libraries/<nom>/`.
2. Déclarez-la dans `theme.toml` :
   ```toml
   [libraries.ma-lib]
   root = "libraries/ma-lib"
   ```
3. Relancez la commande de build des assets.

À ce stade, la bibliothèque est connue du système mais n'est chargée par
personne — il vous reste à choisir **comment** un script va s'en servir :
Recette D (bibliothèque moderne) ou Recette E (bibliothèque classique).

## Ma bibliothèque est ancienne/classique (UMD) — que faire ?

Beaucoup de bibliothèques JS plus anciennes (ou non prévues pour ce
système de modules) s'utilisent en posant leur contenu sur une variable
globale du navigateur (`window.MaLib`), plutôt qu'avec la syntaxe moderne
`import`/`export`. Si la documentation de votre bibliothèque parle de
`<script src="...">` classique, de `window.X`, ou du terme « UMD », c'est
ce cas-là.

Ajoutez simplement `module = false` à sa déclaration :

```toml
[libraries.ma-lib]
root = "libraries/ma-lib"
module = false
```

Sans cette ligne, le système suppose par défaut qu'une bibliothèque est
moderne (`module = true`) — c'est le seul indicateur à poser, et il ne se
devine jamais tout seul depuis le contenu du fichier. Une fois cette ligne
posée, passez à la Recette E — **jamais** à la Recette D pour cette
bibliothèque : un `import` sur du code classique compile sans erreur mais
casse au chargement dans le navigateur.

## Recette D — bibliothèque moderne : l'importer directement

Si votre bibliothèque est moderne (`module = true`, la valeur par défaut,
ou omise), un component ou une capability peut l'importer directement,
comme n'importe quel autre module :

```js
import { Truc } from "libraries/ma-lib/ma-lib.js";
```

Chemin complet depuis la racine des assets, jamais un chemin relatif
(`./...`) — un chemin relatif chercherait un fichier à côté de votre
propre script, pas dans la bibliothèque.

## Recette E — faire dépendre une capability d'une bibliothèque classique

C'est le cas d'une bibliothèque `module = false` (Recette « UMD »
ci-dessus) : elle ne peut pas être importée avec `import`, elle doit être
chargée **avant** que votre script ne s'exécute, pour que la variable
globale qu'elle expose (`window.MaLib`) existe déjà.

Ajoutez `deps` à la déclaration de votre **capability** (uniquement
possible pour une capability, pas pour un component) :

```toml
[scripts.capabilities.ma-capacite]
entry = "scripts/ma-capacite.js"
markers = ["mon-marqueur"]
activation = "boot"
deps = ["libraries/ma-lib/ma-lib.js"]
```

Votre script (`ma-capacite.js`) n'écrit **aucun** `import` pour cette
bibliothèque — il accède simplement à `window.MaLib` directement dans son
code, en sachant qu'elle sera déjà chargée quand `boot()` s'exécute.

Rien d'autre à faire : le système se charge de charger la bibliothèque
avant votre script, dans le bon ordre, en une seule fois même si plusieurs
capabilities partagent la même dépendance (voir plus bas).

## Que dois-je écrire dans mon `.marius` ?

- **Component** : la balise `{% asset scripts/mon-script.js %}` (Recette A).
- **Capability** : rien de spécial — juste la classe marqueur (`class=
  "mon-marqueur"`), soit dans le contenu, soit en dur dans le template.
- **`deps`** : littéralement rien — ni balise, ni classe supplémentaire.
  Une fois déclarée dans `theme.toml`, la dépendance est injectée
  automatiquement partout où la capability qui la consomme apparaît.

## Qu'est-ce qui est automatique ?

- **Le renommage des fichiers** avec une empreinte unique (ex.
  `main.a81f9.js`), pour la mise en cache navigateur — vous n'écrivez
  jamais ce nom généré vous-même.
- **La réécriture de vos propres `import`** vers le bon fichier renommé —
  mais uniquement dans les scripts que vous écrivez vous-même (component/
  capability). Si une bibliothèque tierce importe elle-même d'autres
  fichiers en interne, ces références-là ne sont jamais réécrites (voir
  plus bas).
- **L'ordre de chargement** : toute dépendance déclarée via `deps` est
  toujours chargée avant le script qui la consomme.
- **Le dédoublonnage** : si deux capabilities différentes déclarent la
  même bibliothèque en `deps`, elle n'est chargée qu'une seule fois sur la
  page, jamais deux.

**Une limite à connaître :** si la bibliothèque que vous ajoutez importe
elle-même d'autres fichiers en interne, ces références-là ne sont *pas*
réécrites — seuls vos propres scripts en bénéficient. Préférez une
bibliothèque livrée en un seul fichier, ou vérifiez qu'elle gère ses
propres chemins internes autrement (bundle déjà assemblé par son éditeur,
par exemple).

## Ce qu'il ne faut surtout pas mettre dans `[static.verbatim]`

**Jamais de fichier `.js` applicatif ou de bibliothèque.** `[static.verbatim]`
est réservé aux fichiers qui n'ont besoin d'aucun traitement particulier —
images, polices, fichiers `.map`, etc. Un script placé là échappe à toutes
les protections décrites ci-dessus : pas de `module = false` possible
(donc pas de bibliothèque classique correctement identifiable), pas de
réécriture de ses éventuelles références internes. Si c'est un script,
c'est toujours une des Recettes A à E ci-dessus — jamais `[static.verbatim]`.

## Si quelque chose ne marche pas

- **Le build échoue avec un message clair** (fichier introuvable,
  bibliothèque non déclarée, chemin incorrect, dépendance introuvable) →
  corrigez ce que le message indique, c'est fiable.
- **Le build est vert, mais rien ne change à l'écran** → ce n'est presque
  jamais votre script. Demandez à un backend de vérifier que la page a
  bien été régénérée côté serveur après le déploiement (§7 du guide
  technique) — c'est l'étape la plus souvent oubliée, et elle ne prévient
  jamais par une erreur.

---

_Document rédigé le 31 août 2026_
_Révisé le 2 septembre 2026_
