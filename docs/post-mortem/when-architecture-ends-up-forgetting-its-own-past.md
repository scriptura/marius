# Post-mortem – Quand l'architecture finit par oublier son propre passé

Ce jalon mérite d'être conservé, non parce qu'il révèle une erreur, mais parce qu'il illustre une propriété rarement observée à cette échelle : un projet suffisamment structuré finit par exercer sa propre force de rappel.

En préparant la documentation des templates `.marius`, un ancien pipeline de génération (`body.rs`, `generator.rs`, `orchestrator.rs`, `prologue.rs`) est réapparu. Sur le moment, impossible de me souvenir s'il s'agissait d'un code abandonné, d'une implémentation encore utilisée ou d'une base destinée à la suite du projet.

L'audit a montré une réalité plus intéressante.

Ces fichiers correspondaient bien à une étape réelle du développement. Ils n'étaient ni absurdes ni expérimentaux : ils répondaient exactement à l'état de l'architecture de l'époque. À ce moment-là, les champs _varlena_ n'étaient pas encore intégrés, le pipeline AOT était encore en construction, les invariants DOD n'étaient pas tous stabilisés et les futures extensions `{% extends %}` / `{% block %}` étaient déjà envisagées.

Puis le projet a continué à évoluer.

Les ADR se sont accumulées. Les spécifications ont été réécrites plusieurs fois. Le pipeline AOT a changé de forme. Les structures mémoire ont été adaptées aux contraintes `bytemuck::Pod`. Les capacités statiques sont devenues des expressions évaluées par le compilateur plutôt que des valeurs figées. Le générateur a abandonné plusieurs responsabilités pour respecter une séparation plus stricte entre données, orchestration et génération de code.

Au point que plusieurs mois plus tard, ces anciens fichiers ne racontaient plus l'histoire actuelle du moteur.

Le plus remarquable est que ce n'est pas un souvenir personnel qui a permis de trancher. C'est l'architecture elle-même.

Les invariants désormais en place rendaient objectivement certaines anciennes décisions impossibles à conserver. La divergence `bool` contre `u8` n'était plus une préférence de style ; elle violait directement les contraintes du layout mémoire. Le découpage un fichier par table était devenu incompatible avec la génération unifiée retenue ensuite. Les constantes numériques figées entraient en contradiction avec les garanties recherchées vis-à-vis des différences CRLF/LF. Autrement dit, ce n'est pas parce que je ne me souvenais plus de ces fichiers qu'ils étaient devenus faux ; c'est parce que le système avait progressivement construit des invariants plus forts qu'eux.

Cette enquête rappelle également une autre réalité des projets de recherche.

Pendant plusieurs semaines, le développement de Marius a alterné entre conception d'ADR, rédaction de spécifications, implémentations techniques, corrections d'invariants, orchestration générale, génération de code et validation expérimentale. À mesure que ces couches s'empilent, la mémoire humaine cesse d'être une source fiable de vérité. Les décisions deviennent trop nombreuses, trop interdépendantes et trop espacées dans le temps.

À partir d'un certain seuil de complexité, il devient plus sûr d'interroger le système que son auteur.

C'est précisément ce qui s'est produit ici.

L'audit n'a pas consisté à demander « pourquoi avais-je écrit cela ? », mais « ce code satisfait-il encore les invariants actuels ? ». La réponse est venue naturellement, sans interprétation personnelle.

C'est probablement le signe le plus encourageant de cette phase du projet.

Un système atteint une certaine maturité lorsque ses propres contraintes architecturales deviennent suffisamment fortes pour corriger les oublis de son concepteur. La cohérence n'est alors plus portée par la mémoire de l'auteur, mais par les invariants eux-mêmes.

Finalement, cet épisode n'est pas un rappel d'humilité face à une erreur.

C'est une preuve que l'architecture de Marius commence à devenir plus grande que celui qui l'écrit.

---

Avec le recul, ce n'est probablement pas un événement isolé. Ce n'est déjà plus la première fois que Marius me conduit vers une solution différente de celle que j'avais imaginée plusieurs semaines auparavant.

Le plus surprenant est que cette évolution ne vient ni d'un changement d'avis, ni d'une meilleure intuition, ni même d'une mémoire retrouvée.

Elle vient du système lui-même.

À mesure que les ADR se sont accumulées, que les spécifications se sont précisées et que les invariants se sont renforcés, l'espace des solutions possibles s'est progressivement refermé. Certaines idées qui paraissaient pertinentes au début sont devenues incompatibles avec l'architecture. D'autres, au contraire, se sont imposées presque mécaniquement, comme si elles avaient toujours été là.

Ce phénomène s'est déjà produit à plusieurs reprises : lors du travail sur les bornes mémoire, lors de la conception du provisioning idempotent, et aujourd'hui encore pendant l'audit du compilateur de templates. Chaque fois, le point commun est le même : ce n'est pas la mémoire de l'auteur qui a permis de retrouver la bonne direction, mais les invariants du système.

C'est sans doute l'un des critères les plus exigeants de maturité pour une architecture. Lorsqu'un projet atteint ce stade, il cesse progressivement d'être un assemblage de décisions individuelles. Il devient un ensemble de contraintes cohérentes qui orientent elles-mêmes les décisions futures.

Un bon système ne se contente plus de fonctionner.

Il devient capable de guider son propre concepteur.

C'est probablement la plus belle définition que je puisse aujourd'hui donner d'une architecture véritablement data-driven.

---

_1 juillet 2026_
