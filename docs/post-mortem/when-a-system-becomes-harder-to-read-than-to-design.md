# Post-mortem — Quand un système devient plus difficile à relire qu'à concevoir

Il existe un paradoxe fréquent dans les projets techniques de longue haleine : le plus difficile n'est pas toujours de concevoir une architecture cohérente, mais de parvenir à la relire plusieurs mois plus tard avec le même niveau de certitude. Nous en avons fait l'expérience au cours du développement de Marius.

L'architecture n'avait pas été construite en quelques semaines, dans un contexte isolé et avec un temps de travail continu. Elle s'est développée progressivement, au rythme de longues périodes d'analyse alternant avec des phases d'implémentation, elles-mêmes fragmentées par les contraintes de la vie quotidienne et professionnelle. Une partie importante des décisions a été prise tard le soir, sur des créneaux parfois très courts, entre deux obligations, avant de reprendre plusieurs jours plus tard sans toujours retrouver immédiatement le même contexte mental. Cette manière de travailler n'empêche pas de produire une architecture cohérente. En revanche, elle rend beaucoup plus difficile la reconstruction de son modèle mental plusieurs mois après.

Un concepteur expérimenté s'appuie souvent sur une mémoire implicite de son propre système. Dans notre cas, cette mémoire n'existait pas réellement. Une partie des concepts eux-mêmes était découverte au fur et à mesure du projet : compilation AOT, organisation DOD, séparation stricte entre les phases de construction et le runtime, protocoles binaires, projections réactives, orchestration Copy-on-Write... Beaucoup de ces idées étaient nouvelles pour nous au moment où elles étaient conçues. La difficulté n'était donc pas seulement de se souvenir du code. Il fallait également retrouver le raisonnement qui avait conduit à ce code.

Cette situation est devenue visible lorsque plusieurs documents de référence ont commencé à diverger de l'implémentation réelle. Rien de spectaculaire : quelques hypothèses devenues obsolètes, un ancien modèle mental encore présent dans un guide, un flux de données qui avait évolué sans que tous les documents suivent immédiatement.

Le problème n'était pas que l'architecture soit incohérente. Le problème était que nous ne savions plus avec certitude laquelle des différentes représentations faisait réellement autorité : Le code ? La documentation ? Le manifeste de la projection réactive ? Les guides de cycle de vie ? Ou simplement notre souvenir ?

À partir de ce moment, toute tentative de raisonnement devenait fragile. Chaque nouveau document risquait de réintroduire une hypothèse déjà abandonnée, et chaque évolution obligeait à vérifier de nouveau l'ensemble de la chaîne.

La résolution est venue d'un changement méthodologique beaucoup plus que d'une correction technique. Nous avons commencé par accepter un principe simple : le code réel est la seule source de vérité. Une documentation n'a pas pour rôle de corriger le code ; elle doit décrire fidèlement son comportement ou définir explicitement une cible architecturale encore non atteinte. À partir de cette règle, nous avons construit une **Data Flow Specification**. Contrairement à une documentation classique, ce document ne décrivait pas un composant particulier mais le chemin complet parcouru par les données, depuis la mutation SQL jusqu'au contenu finalement servi par HTTP.

Ce choix a profondément changé notre manière de relire le système. Chaque composant pouvait désormais être confronté à un flux global plutôt qu'à son fonctionnement local. Le Dispatcher, le StoreRegistry, les projections, les packfiles, le moteur de rendu et le serveur HTTP cessaient d'être des éléments indépendants ; ils devenaient les maillons successifs d'une même chaîne.

Cette représentation a ensuite été utilisée comme colonne vertébrale d'une série d'audits systématiques. Chaque fichier était confronté au modèle de référence. Chaque divergence était classée. Était-ce un bug ? Une documentation obsolète ? Une hypothèse devenue fausse ? Ou simplement une limite de l'audit, faute d'avoir encore vu un fichier ?

Une autre évolution importante est apparue au cours de cette démarche : nous avons progressivement cessé de demander à l'assistant « d'avoir raison ». Nous lui avons demandé de qualifier le niveau de preuve de chacune de ses affirmations.

Une différence fondamentale est apparue entre :

* ce qui avait été exécuté ;
* ce qui avait été compilé ;
* ce qui avait été vérifié directement dans le code ;
* ce qui restait une déduction.

Cette distinction, apparemment anodine, a considérablement amélioré la qualité des revues d'architecture. Une hypothèse n'était plus confondue avec une certitude, et un comportement observé n'était plus généralisé sans justification.

Peu à peu, la nature même des retours a changé. Au début des audits, les analyses révélaient essentiellement des contradictions entre les documents et l'implémentation. À la fin, elles validaient principalement des hypothèses déjà formulées. Ce changement de nature était en lui-même un indicateur de convergence. L'architecture n'était plus reconstruite à chaque lecture. Elle devenait stable.

Avec le recul, cette expérience dépasse largement le cadre du projet Marius. Elle montre qu'une architecture ambitieuse ne peut pas reposer uniquement sur la mémoire de son concepteur, surtout lorsque celui-ci travaille de manière autodidacte, dans un temps fortement fragmenté et sur plusieurs mois. Elle doit progressivement acquérir sa propre mémoire. Cette mémoire n'est pas constituée uniquement de documentation. Elle repose sur un ensemble cohérent formé par une spécification d'architecture, des invariants explicitement formulés, des audits réguliers, des documents synchronisés avec le code et une discipline consistant à confronter systématiquement chaque représentation au comportement réel du système.

Nous ne considérons plus ces documents comme des livrables annexes. Ils sont devenus une partie intégrante de l'architecture elle-même. Ils permettent non seulement de transmettre le projet à d'autres, mais surtout de pouvoir, plusieurs mois plus tard, le relire avec la même confiance que le jour où il a été conçu.

_le 21 juillet 2026_
