# ADR-011 : Des pages monolithiques aux projections AOT ordonnancées

**Statut :** Proposé (pré-v1)
**Révision de cette version :** consolidation post-discussion (clarification d'ontologie, invariant de capacité). Remplace le brouillon initial dans son intégralité.

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

## 2. Décision

La page HTML cesse d'être l'unité fondamentale de génération. La nouvelle unité architecturale devient la **Projection**.

Une projection représente un domaine fonctionnel cohérent partageant :

- le même cycle d'invalidation ;
- les mêmes sources de données ;
- les mêmes invariants de cohérence.

Une réponse HTTP devient l'ordonnancement déterministe du document pré-composé (ADR-008) et, lorsqu'elle en contient, des projections volatiles qui lui sont propres. Le runtime ne construit plus une page depuis zéro : il ordonnance des unités déjà compilées, entrelaçant au besoin du contenu statique pré-assemblé avec du contenu résolu à la requête. Cette ADR **ajoute** une seconde dimension au modèle ADR-008 ; elle ne le remplace pas.

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

## 4. Segment

Un Segment est une plage mémoire contiguë. Exemples de provenance possible :

- données statiques compilées ;
- contenu précompilé issu d'un artefact ;
- un bloc mémoire continu d'une autre nature.

Le runtime ignore la signification du Segment. Il ne manipule que des plages mémoire.

**`PackfileEntry` (structure d'indexation du packfile HTML existant) est une implémentation particulière d'un Segment, pas un renommage de celui-ci.** Tous les segments proviennent aujourd'hui d'un packfile ; rien n'impose que ce soit vrai demain. Cette distinction découple complètement l'architecture métier des primitives d'émission propres au système d'exploitation. La Forge raisonne en Projections et Artefacts ; le Runtime raisonne en Segments. La conversion vers les primitives d'émission (représentation POSIX finale) n'intervient qu'au dernier instant du runtime, et relève du DESIGN, pas de cette ADR.

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

## 6. Frontière JavaScript

Toute page doit demeurer complète sans JavaScript. Cette règle constitue un invariant architectural.

Une page sans JavaScript doit conserver : son contenu, sa navigation, son breadcrumb, ses liens, son accessibilité, son référencement.

JavaScript ne peut intervenir que comme accélérateur. Il ne constitue jamais une dépendance fonctionnelle de la page. Les composants dont le cycle de mutation est fortement volatil peuvent être chargés ou rafraîchis indépendamment (état utilisateur, panier, notifications, éléments transactionnels).

La frontière entre projections statiques et projections volatiles est déterminée par le cycle de mutation des données, jamais par leur position dans le DOM.

## 7. Chemin chaud

Le runtime ne réalise :

- aucun calcul de rendu ;
- aucune concaténation HTML ;
- aucune interprétation de template ;
- aucune allocation liée à la composition de la page.

Son rôle consiste uniquement à résoudre une `RequestEntity`, récupérer les Segments correspondants, les ordonnancer, et déléguer leur émission au système d'exploitation.

Le runtime devient ainsi un ordonnanceur de mémoire plutôt qu'un moteur de rendu.

**Invariants de capacité — dépréciés (décision du 7 octobre 2026).** La rédaction initiale posait trois propriétés distinctes — zéro allocation, zéro reconstruction, zéro copie au sens transfert réseau — comme invariants de tout chemin chaud. Tenir cette intention pour toute réponse aurait exigé de réécrire la pile HTTP (Axum/Hyper) : elle est abandonnée. Ce qui est retenu :

- le runtime n'effectue ni rendu, ni concaténation, ni interprétation de template (liste ci-dessus) : il ordonnance des segments déjà compilés ;
- la garantie de **zéro copie au sens transfert réseau** (`sendfile(2)`, ADR-006) est circonscrite à l'émission **monolithique** ;
- pour l'émission **segmentée**, `docs/archived/SPECIFICATION-transport-segmente-t2a.md` (§3 à §6) fait foi : aucun payload n'est copié par Marius à la frontière `ResolvedRange → Bytes`, le coût de matérialisation est borné par le nombre de segments, et les coûts internes du transport (Hyper, Tokio, noyau) n'appartiennent pas au contrat Marius.

## 8. Budget de Segments

Chaque projection possède un nombre fini de Segments. Une réponse HTTP possède donc un budget total de Segments. Cette métrique est une propriété AOT vérifiée par la Forge.

Le budget de Segments constitue une contrainte spatiale comparable à un budget mémoire. Son objectif est d'empêcher la micro-fragmentation. La Forge garantit que la granularité retenue reste compatible avec les capacités de la plateforme cible. Le runtime suppose cette garantie acquise et ne réalise aucune correction dynamique.

Le compilateur garantit. Le runtime exécute.

## 9. Neutralité du format

Cette architecture ne dépend pas du HTML. Les Segments représentent uniquement des plages mémoire. Le runtime ignore leur contenu.

Une projection pourrait tout aussi bien produire HTML, JSON, XML, RSS, texte ou données binaires. Le HTML devient un backend parmi d'autres. Le moteur reste identique.

## 10. Conséquences

Cette décision transforme profondément la nature de Marius. Le moteur n'est plus défini comme un générateur de pages HTML. Il devient un compilateur AOT de projections ordonnancées.

Les pages sont désormais une conséquence de l'ordonnancement de domaines de données indépendants, et non plus l'unité fondamentale du système.

Cette évolution permet simultanément :

- de supprimer les explosions combinatoires liées aux états réellement indépendants, sans remettre en cause la doctrine de pré-composition du document minimal définie par ADR-008 ;
- de préserver un chemin chaud déterministe ;
- de maintenir une architecture sans calcul de rendu au runtime ;
- de conserver une séparation stricte entre conception (Forge) et exécution (Runtime).

## 11. Hors périmètre de cette ADR

Explicitement non traités ici — relèvent du DESIGN Runtime à venir :

- Layout et propriétés POD du descripteur de segment (`SegmentDescriptor`).
- Mécanisme de résolution d'origine d'un segment (`SourceId`/`SourceRuntime`) et sa durée de vie.
- Transition de l'implémentation d'émission réseau actuelle vers une émission sans copie (`writev`/`sendmsg`/équivalent).
- Devenir du trait `Projection` existant dans le code (fusion actuelle des niveaux 1 et 2, cf. §3).
- Sources de segments non adressables par artefact statique : le premier cas réel est traité (segment volatile `nav_profile`, produit à la requête à partir d'un contexte de requête ; contrat dans `CONTRAT-volatile-v1.md`, état d'implémentation en §12). Restent hors périmètre : une source lisant un buffer PostgreSQL live, un JSON généré, d'autres composants volatils.
- Mécanisme d'obtention du zéro-copie réseau (`MSG_ZEROCOPY` ou équivalent) — hors contrat Marius pour l'émission segmentée (§7).
- Formule fermée de calcul du budget de segments : aucune formule n'est normative. Le §8 reste la seule règle — la Forge calcule le budget exact depuis le graphe réel du template. Une formule peut apparaître dans le DESIGN à titre pédagogique, jamais ici.

**Relation avec les ADR existants :**

- **ADR-006** (sendfile, chemin de lecture) : statut historique pour le cas général. Reste la description exacte du chemin de lecture pour toute réponse composée uniquement de contenu ADR-008 (aucune projection volatile) — cas encore majoritaire. Pour toute réponse comportant une projection volatile, cette ADR (011) devient la référence du read path ; ADR-006 doit porter une mention de statut renvoyant ici (action de documentation distincte, hors du présent texte).
- **ADR-008/ADR-009** : non remises en cause. Le Minimum Viable Document et l'adressage par PK restent la doctrine pour tout contenu non volatil.

## 12. État d'implémentation (6 octobre 2026)

Cette section constate ce que le code réalise ; elle ne modifie aucune décision de ce document.

- **Niveaux 1 et 2 (Forge).** `publication.toml` déclare les artefacts, les routes et les régions volatiles (`[[artifact]]`, `[[route]]`, `[[volatile_region]]`). Le build en génère `ARTIFACTS` et `ROUTE_DESCRIPTORS`. La route `/content/{id}` est aplatie à la compilation en trois segments : artefact `content_core_head`, source volatile `nav_profile`, artefact `content_core_tail`. Un même composant (`content.core`) produit plusieurs artefacts à partir d'une seule ingestion.
- **Niveau 3 (Segment).** Deux provenances existent : un packfile mmap (`MaterializedSource::Mmap`, `PackfileEntry` en étant l'indexation) et un stockage volatile possédé (`MaterializedSource::Volatile`). Le runtime ne manipule que des plages mémoire (`ResolvedRange`).
- **Niveau 4 (Réponse HTTP).** `content_document.rs` ordonnance les segments dans l'ordre du descripteur et remet le résultat à Hyper (`Bytes::from_owner` → `Body`) ; le chemin monolithique reste monté à `/__monolithic/content/{id}` comme voie de comparaison.
- **Contexte de requête.** Le contenu volatile dépend aujourd'hui d'un paramètre de requête expérimental (`?user=`), pas d'une authentification ou d'une session ; ce n'est pas une décision d'identité.
- **Budget de segments (§8).** Le nombre de segments est une propriété de la représentation générée (K=3 pour cette route) ; aucune formule n'est normative (§11).
- **Invariants de capacité (§7).** Dépréciés : le zéro-copie réseau est circonscrit à l'émission monolithique ; l'émission segmentée relève de la SPEC T2A v2 (voir §7 ci-dessus).
