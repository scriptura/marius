# Théorie de la Compression Structurelle

## Introduction

Marius n'est pas né d'une volonté de construire un CMS plus rapide, ni même d'une volonté de construire un CMS tout court. Le CMS n'est que le terrain d'expérimentation choisi. La question qui motive réellement le projet est plus fondamentale : Pourquoi les systèmes logiciels modernes sont-ils devenus aussi lourds alors que les machines n'ont jamais été aussi puissantes ? Ou formulé autrement : Pourquoi faut-il aujourd'hui autant de couches, autant de dépendances, autant de mémoire et autant de ressources pour produire des capacités qui, souvent, existaient déjà il y a plusieurs décennies ? Cette interrogation constitue le point de départ de Marius. Le projet est avant tout une exploration. Une tentative de comprendre ce qui, dans le logiciel contemporain, relève de la nécessité réelle, et ce qui relève de l'accumulation historique. Cette démarche s'inscrit dans l'esprit du hacking au sens originel du terme, tel qu'il était pratiqué au MIT : non pas casser les systèmes, mais découvrir une formulation plus élégante, plus compacte et plus cohérente d'un problème donné.

---

## La complexité comme phénomène physique

La plupart des discussions architecturales abordent la complexité comme une notion abstraite. Marius adopte une hypothèse différente. La complexité est considérée comme une propriété physique du système. Elle possède un coût mesurable. Ce coût peut apparaître sous de nombreuses formes : temps CPU, consommation mémoire, trafic disque, latence réseau, temps de compilation, charge cognitive, difficulté de maintenance, complexité organisationnelle ou encore coût de transmission des connaissances. Ces phénomènes semblent souvent distincts. Pourtant, ils sont fréquemment les manifestations différentes d'une même structure sous-jacente. Une abstraction inutile ne consomme pas seulement des cycles processeur. Elle consomme également de l'attention humaine. Une couche supplémentaire ne produit pas seulement des appels de fonction. Elle produit aussi de la documentation, des conventions, des points de synchronisation et des besoins de coordination. Pour cette raison, Marius considère que la complexité doit être traitée comme une ressource physique rare.

---

## La notion de masse structurelle

Toute structure possède une masse : Une couche logicielle possède une masse, une abstraction possède une masse, un protocole possède une masse, une convention possède une masse, une dépendance possède une masse, même un document possède une masse. Cette masse n'est pas nécessairement mauvaise, toute capacité utile exige une certaine quantité de structure. La question fondamentale n'est donc jamais de savoir si une structure est élégante, populaire ou conforme aux bonnes pratiques du moment, la seule question pertinente est de savoir si cette structure produit réellement une capacité qui justifie son existence. Une structure qui ne produit aucune capacité identifiable constitue une dette structurelle, une structure qui produit une capacité disproportionnée par rapport à son coût constitue au contraire un actif architectural.

---

## La compression structurelle

Le principe central de Marius est celui de la compression structurelle. La compression structurelle consiste à réduire la masse totale du système sans réduire les capacités qu'il produit. L'objectif n'est pas la simplicité pour elle-même. L'objectif n'est pas non plus l'austérité technique. L'objectif est de supprimer les structures redondantes, les intermédiaires inutiles et les traductions superflues afin de concentrer le système sur ce qui produit effectivement de la valeur. Cette démarche ne cherche pas à nier la complexité. Elle cherche à identifier où cette complexité est réellement nécessaire.

---

## La complexité n'est pas supprimée, elle est déplacée

Une erreur fréquente consiste à croire qu'un système compressé est un système simple. Ce n'est pas le cas. Les compilateurs ne sont pas simples. Les bases de données ne sont pas simples. Les moteurs de rendu ne sont pas simples. Ils sont profondément complexes. La différence est que leur complexité est concentrée. Marius adopte la même approche. La compression structurelle ne supprime pas la complexité. Elle déplace cette complexité vers les endroits où elle coûte le moins cher et où elle produit le plus de valeur. Cette logique conduit naturellement vers l'AOT, la génération de code, la compilation de modèles, les projections matérialisées et les pipelines déterministes. Le projet préfère payer un coût une fois lors de la construction du système plutôt que de le payer à chaque exécution. Il préfère payer un coût au moment de la génération plutôt qu'au moment du runtime. Il préfère payer un coût dans les outils plutôt que dans les serveurs de production. Cette philosophie peut être résumée par une idée simple : Tout ce qui peut être résolu avant l'exécution doit être résolu avant l'exécution.

---

## Frugalité avant performance

La performance est souvent présentée comme l'objectif principal des systèmes optimisés. Marius adopte une position différente. La performance n'est pas un objectif. Elle est une conséquence. L'objectif est la frugalité. Une structure frugale utilise moins de mémoire. Une structure frugale effectue moins de copies. Une structure frugale réalise moins d'allocations. Une structure frugale transporte moins d'informations inutiles. Une structure frugale produit moins de travail. Lorsqu'un système devient réellement frugal, les performances apparaissent naturellement comme un effet secondaire. La recherche de performance conduit souvent à ajouter des mécanismes correctifs : caches, optimisations locales, couches spécialisées. La recherche de frugalité conduit au contraire à interroger directement la nécessité de chaque coût.

---

## Réduire la distance entre intention et exécution

Une source majeure de complexité provient de la distance séparant l'intention de son exécution réelle. Dans de nombreux systèmes modernes, cette distance est considérable. Une opération métier traverse des couches successives d'abstractions, de conversions, de représentations intermédiaires et de mécanismes automatiques avant d'atteindre la machine. Chaque transformation ajoute de la masse structurelle. Chaque traduction ajoute de l'incertitude. Chaque intermédiaire éloigne l'auteur du système de son comportement réel. Marius cherche à réduire cette distance autant que possible. L'intention doit rester proche de la structure. La structure doit rester proche de l'exécution. L'exécution doit rester observable.

---

## Transparence contre magie

La compression structurelle comporte un risque. Lorsqu'un système devient très dense, la tentation apparaît de masquer cette densité derrière de nouvelles couches d'abstraction. Cette tentation est particulièrement forte dans les outils de génération de code. Marius refuse cette approche. La Forge existe pour réduire la friction d'accès à la théorie du système. Elle n'existe pas pour masquer cette théorie. Les artefacts générés doivent rester compréhensibles. Les transformations doivent rester auditables. Les décisions doivent rester traçables. Un outil qui produit un comportement que son utilisateur ne peut plus expliquer devient une nouvelle source de dette structurelle. La méta-programmation n'est acceptable que si elle demeure transparente. Autrement dit, la Forge ne doit jamais devenir un ORM qui refuse de se nommer comme tel.

---

## Marius Core et la Forge

Marius Core représente la théorie fondamentale du système. C'est le lieu où la compression structurelle est poussée aussi loin que possible. Les choix liés au DOD, à l'ECS, au no_std, à l'AOT, à la génération de code ou aux futures architectures de mémoire partagée participent tous de cette recherche. Cette densité structurelle possède toutefois un coût d'accès. La Forge existe précisément pour absorber ce coût. Son rôle n'est pas d'ajouter de nouvelles couches, son rôle est de rendre la théorie praticable. Elle constitue une interface cognitive vers un système volontairement dense. L'objectif n'est donc pas seulement de compresser l'exécution. L'objectif est également d'industrialiser l'accès à cette compression.

---

## Conway inversé

La loi de Conway affirme que les systèmes reflètent les structures de communication des organisations qui les produisent. Marius s'intéresse au mouvement inverse. Une architecture influence également les formes d'organisation qu'elle rend possibles. Une architecture lourde exige davantage de coordination. Une architecture fragmentée exige davantage de synchronisation. Une architecture opaque exige davantage de transmission. À l'inverse, une architecture cohérente et compressée réduit naturellement certaines formes de coordination. La compression structurelle ne concerne donc pas seulement les machines. Elle concerne également les humains. La qualité d'une architecture se mesure aussi à sa capacité à transmettre ses propres principes.

---

## Conclusion

Marius n'est pas une tentative de retour nostalgique vers un passé supposément plus simple. Le projet ne cherche pas à rejeter la modernité. Il cherche à interroger les coûts que cette modernité a progressivement rendus invisibles. Chaque couche, chaque abstraction, chaque dépendance et chaque mécanisme doivent pouvoir justifier leur existence par les capacités qu'ils produisent. L'ambition de Marius est d'explorer jusqu'où cette exigence peut être poussée. La question qui guide le projet reste finalement très simple : Quelle est la structure minimale capable de produire les propriétés recherchées ? Toutes les décisions architecturales, toutes les expérimentations et tous les outils développés dans le cadre du projet constituent des tentatives de réponse à cette question.

---

Document rédigé le 17 juin 2026.
