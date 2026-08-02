# ADR-011 : Des pages monolithiques aux projections AOT ordonnancées

**Statut :** Proposé (pré-v1)
**Révision de cette version :** consolidation post-discussion (clarification d'ontologie, invariant de capacité). Remplace le brouillon initial dans son intégralité.

---

## 1. Contexte

Les premières versions de Marius considéraient une page HTML comme une unité indivisible. Chaque projection réactive produisait un document HTML complet, stocké dans un pack binaire puis servi directement par le runtime.

Cette approche possède plusieurs propriétés recherchées :

- représentation entièrement AOT ;
- coût constant sur le chemin chaud ;
- absence d'allocation dynamique ;
- absence de calcul de rendu au runtime.

L'analyse des composants présentant des états indépendants (session utilisateur, widgets transactionnels, menus, notifications, etc.) a mis en évidence une limite structurelle : une page n'est pas toujours la véritable unité d'invalidation. Certaines parties évoluent selon des cycles différents. D'autres sont communes à des milliers de pages. Enfin, certaines dépendent directement de la route et non d'un état indépendant.

Le modèle « une page = une projection » mélange donc plusieurs domaines ayant des cycles de mutation différents. L'objectif de cette ADR est de redéfinir l'unité fondamentale du moteur.

**Périmètre explicite (post-discussion) :** le Minimum Viable Document d'une page — navigation, breadcrumb, pied de page, structure minimale — reste sous la doctrine ADR-008 : pré-composition à l'écriture, invalidation batchée par le Dispatcher. Cette ADR ne cherche plus à éliminer ce coût de duplication ; ADR-008 continue de le gérer, sans changement. Cette ADR traite exclusivement des projections dont le cycle de mutation est réellement découplé de celui de la page qui les contient — typiquement des états volatils dépendant de la requête ou de la session (§6, troisième ligne de la taxonomie ADR-008 §4.3), que ni ADR-008 ni ADR-009 ne couvrent puisqu'ils sont par construction hors du modèle AOT pré-rendu.

---

## 2. Décision

La page HTML cesse d'être l'unité fondamentale de génération. La nouvelle unité architecturale devient la **Projection**.

Une projection représente un domaine fonctionnel cohérent partageant :

- le même cycle d'invalidation ;
- les mêmes sources de données ;
- les mêmes invariants de cohérence.

Une réponse HTTP devient l'ordonnancement déterministe du document pré-composé (ADR-008) et, lorsqu'elle en contient, des projections volatiles qui lui sont propres. Le runtime ne construit plus une page depuis zéro : il ordonnance des unités déjà compilées, entrelaçant au besoin du contenu statique pré-assemblé avec du contenu résolu à la requête. Cette ADR **ajoute** une seconde dimension au modèle ADR-008 ; elle ne le remplace pas.

---

## 3. Ontologie — quatre niveaux, pas trois

La rédaction initiale de cette ADR confondait trois choses distinctes sous un même terme. La distinction suivante est désormais la référence :

| Niveau | Nom | Produit par | Portée |
| --- | --- | --- | --- |
| 1 | **Projection** | Forge (AOT) | Concept exclusivement AOT. Domaine de données (navigation, article, breadcrumb, pied de page, résultat de recherche...). Ne désigne ni un composant DOM ni un fragment de template. Possède son invalidation, son pipeline, son générateur. |
| 2 | **Artefact** | Forge (AOT) | Ce que produit une projection à la compilation/régénération — aujourd'hui un packfile. Une projection produit un artefact. |
| 3 | **Segment** | Runtime | Plage mémoire contiguë. Provenance ignorée du point de vue du runtime : packfile mmap'd aujourd'hui, buffer dynamique ou toute autre source adressable demain. |
| 4 | **Réponse HTTP** | Runtime | Ordonnancement de Segments vers l'émission. |

**Le runtime ne connaît que le niveau 3 et 4.** Il n'a jamais besoin de savoir qu'un Segment provient d'une Projection ou d'un domaine fonctionnel particulier — cette information est épuisée à la compilation.

Point de vigilance terminologique : le trait applicatif nommé `Projection` dans le code existant fusionne aujourd'hui les niveaux 1 et 2 (extraction de données, génération, écriture d'artefact, dans une seule interface, 1:1 avec une table SQL). Ce nommage est historique et antérieur à la présente clarification. Son évolution éventuelle (scission, renommage) relève du DESIGN Runtime, pas de la présente décision — cette ADR fixe le vocabulaire cible, pas la migration du code.

---

## 4. Segment

Un Segment est une plage mémoire contiguë. Exemples de provenance possible :

- données statiques compilées ;
- contenu précompilé issu d'un artefact ;
- un bloc mémoire continu d'une autre nature.

Le runtime ignore la signification du Segment. Il ne manipule que des plages mémoire.

**`PackfileEntry` (structure d'indexation du packfile HTML existant) est une implémentation particulière d'un Segment, pas un renommage de celui-ci.** Tous les segments proviennent aujourd'hui d'un packfile ; rien n'impose que ce soit vrai demain. Cette distinction découple complètement l'architecture métier des primitives d'émission propres au système d'exploitation. La Forge raisonne en Projections et Artefacts ; le Runtime raisonne en Segments. La conversion vers les primitives d'émission (représentation POSIX finale) n'intervient qu'au dernier instant du runtime, et relève du DESIGN, pas de cette ADR.

---

## 5. Redéfinition du runtime

Le runtime n'est plus un serveur de pages. Il devient un ordonnanceur de segments — et non un ordonnanceur de projections : la Projection est épuisée par la Forge avant que le runtime n'entre en jeu.

```
URL
  ↓
RequestEntity
  ↓
Segment[]
  ↓
Émission
```

La Forge aplanit le graphe des projections lors de la compilation. Le runtime ignore jusqu'à l'existence du concept de Projection : il mappe une requête vers une séquence de Segments, sans jamais réifier de notion de domaine fonctionnel au runtime.

La résolution de l'URL doit être déterministe et optimisée AOT. La structure exacte (hash parfait, table indexée, arbre compact, etc.) ne relève pas de cette ADR. L'ADR impose uniquement que cette résolution ne réintroduise pas une logique de rendu ou de composition dynamique.

---

## 6. Frontière JavaScript

Toute page doit demeurer complète sans JavaScript. Cette règle constitue un invariant architectural.

Une page sans JavaScript doit conserver : son contenu, sa navigation, son breadcrumb, ses liens, son accessibilité, son référencement.

JavaScript ne peut intervenir que comme accélérateur. Il ne constitue jamais une dépendance fonctionnelle de la page. Les composants dont le cycle de mutation est fortement volatil peuvent être chargés ou rafraîchis indépendamment (état utilisateur, panier, notifications, éléments transactionnels).

La frontière entre projections statiques et projections volatiles est déterminée par le cycle de mutation des données, jamais par leur position dans le DOM.

---

## 7. Chemin chaud

Le runtime ne réalise :

- aucun calcul de rendu ;
- aucune concaténation HTML ;
- aucune interprétation de template ;
- aucune allocation liée à la composition de la page.

Son rôle consiste uniquement à résoudre une `RequestEntity`, récupérer les Segments correspondants, les ordonnancer, et déléguer leur émission au système d'exploitation.

Le runtime devient ainsi un ordonnanceur de mémoire plutôt qu'un moteur de rendu.

**Invariants de capacité — trois propriétés distinctes, à ne jamais fusionner :**

- **Zéro allocation** : aucune construction de `Vec`/`String`/buffer intermédiaire sur le chemin chaud pour composer la réponse. Invariant fondamental de cette ADR.
- **Zéro reconstruction** : les segments existants sont uniquement ordonnancés — aucune concaténation, aucune réécriture de leur contenu. Invariant fondamental de cette ADR.
- **Zéro copie (au sens transfert réseau)** : propriété distincte, plus forte, qui ne découle pas automatiquement des deux précédentes. `sendfile(2)` (ADR-006) l'obtient nativement (transfert kernel-to-kernel). Une émission par ordonnancement de segments multiples (`writev`/`sendmsg` ou équivalent) ne l'obtient pas par défaut — le noyau peut copier le contenu des pages utilisateur vers le buffer socket. L'obtenir exige un mécanisme distinct (ex. `MSG_ZEROCOPY`) dont la conception et le coût relèvent du DESIGN Runtime, jamais présumés acquis par cette ADR.

Les deux premiers invariants ne sont pas satisfaits par l'implémentation de référence actuelle, y compris pour N = 1 (le chemin de lecture actuel alloue un `Vec<u8>` par requête). Leur mise en conformité relève du DESIGN Runtime, pas de la présente décision.

---

## 8. Budget de Segments

Chaque projection possède un nombre fini de Segments. Une réponse HTTP possède donc un budget total de Segments. Cette métrique est une propriété AOT vérifiée par la Forge.

Le budget de Segments constitue une contrainte spatiale comparable à un budget mémoire. Son objectif est d'empêcher la micro-fragmentation. La Forge garantit que la granularité retenue reste compatible avec les capacités de la plateforme cible. Le runtime suppose cette garantie acquise et ne réalise aucune correction dynamique.

Le compilateur garantit. Le runtime exécute.

---

## 9. Neutralité du format

Cette architecture ne dépend pas du HTML. Les Segments représentent uniquement des plages mémoire. Le runtime ignore leur contenu.

Une projection pourrait tout aussi bien produire HTML, JSON, XML, RSS, texte ou données binaires. Le HTML devient un backend parmi d'autres. Le moteur reste identique.

---

## 10. Conséquences

Cette décision transforme profondément la nature de Marius. Le moteur n'est plus défini comme un générateur de pages HTML. Il devient un compilateur AOT de projections ordonnancées.

Les pages sont désormais une conséquence de l'ordonnancement de domaines de données indépendants, et non plus l'unité fondamentale du système.

Cette évolution permet simultanément :

- de supprimer les explosions combinatoires liées aux états réellement indépendants ;
- de supprimer les explosions combinatoires liées aux états réellement indépendants, sans remettre en cause la doctrine de pré-composition du document minimal définie par ADR-008 ;
- de préserver un chemin chaud déterministe ;
- de maintenir une architecture sans calcul de rendu au runtime ;
- de conserver une séparation stricte entre conception (Forge) et exécution (Runtime).

---

## 11. Hors périmètre de cette ADR

Explicitement non traités ici — relèvent du DESIGN Runtime à venir :

- Layout et propriétés POD du descripteur de segment (`SegmentDescriptor`).
- Mécanisme de résolution d'origine d'un segment (`SourceId`/`SourceRuntime`) et sa durée de vie.
- Transition de l'implémentation d'émission réseau actuelle vers une émission sans copie (`writev`/`sendmsg`/équivalent).
- Devenir du trait `Projection` existant dans le code (fusion actuelle des niveaux 1 et 2, cf. §3).
- Sources de segments non adressables par artefact statique (buffer PostgreSQL live, JSON généré, composants volatils) — angle mort assumé de la Phase 1, à rouvrir lors du premier cas réel.
- Mécanisme d'obtention du zéro-copie réseau (`MSG_ZEROCOPY` ou équivalent) — cf. §7, invariant distinct non couvert par cette ADR.
- Formule fermée de calcul du budget de segments : aucune formule n'est normative. Le §8 reste la seule règle — la Forge calcule le budget exact depuis le graphe réel du template. Une formule peut apparaître dans le DESIGN à titre pédagogique, jamais ici.

**Relation avec les ADR existants :**

- **ADR-006** (sendfile, chemin de lecture) : statut historique pour le cas général. Reste la description exacte du chemin de lecture pour toute réponse composée uniquement de contenu ADR-008 (aucune projection volatile) — cas encore majoritaire. Pour toute réponse comportant une projection volatile, cette ADR (011) devient la référence du read path ; ADR-006 doit porter une mention de statut renvoyant ici (action de documentation distincte, hors du présent texte).
- **ADR-008/ADR-009** : non remises en cause. Le Minimum Viable Document et l'adressage par PK restent la doctrine pour tout contenu non volatil.
