# Pourquoi Marius ne pouvait émerger qu'en dehors du monde de la tech

Il existe des logiciels qui ne sont pas seulement des logiciels. Ils sont des anomalies, des objets théoriques autant que techniques, qui interrogent par leur seule existence les conditions de leur propre production. Marius est de ceux-là.

Marius, c'est un CMS. Mais c'est surtout un système de projection réactive, un compilateur AOT pour le Web, une architecture entièrement pensée autour de la compression structurelle. Un système où le serveur ne calcule plus rien : il se contente de projeter des octets déjà prêts, composés à l'écriture, et livrés via des descripteurs de fichier pré-ouverts. Un système dont les métriques se mesurent en nanosecondes.

Ce qui frappe, quand on examine Marius, ce n'est pas seulement l'élégance du résultat. C'est le constat, de plus en plus net à mesure qu'on en comprend l'architecture, qu'un tel système n'aurait probablement jamais pu voir le jour dans le monde professionnel du développement logiciel. Non par manque de talent — le monde de la tech regorge d'ingénieurs brillants. Mais par un jeu de forces structurelles, de contraintes invisibles et de conventions intériorisées qui, ensemble, rendent ce type d'œuvre radicalement improbable dans un cadre professionnel.

Ce texte n'est pas un réquisitoire contre les développeurs professionnels. C'est une tentative de comprendre, à travers le cas Marius, les mécanismes qui empêchent l'émergence de certaines cohérences architecturales, et d'identifier, en creux, les conditions qui les ont rendues possibles ici.

---

### La loi de Conway, préalable incontournable

En 1968, Melvin Conway formule une observation qui deviendra célèbre : « Les organisations qui conçoivent des systèmes sont contraintes de produire des architectures qui sont des copies de leurs structures de communication. »

La loi de Conway n'est pas une curiosité sociologique. C'est une loi physique du logiciel. Une entreprise organisée en équipes séparées — frontend, backend, base de données, infrastructure — produira nécessairement un système divisé en couches correspondantes, avec des API, des contrats, des traductions entre chaque strate. Non parce que cette architecture est techniquement optimale, mais parce qu'elle est organisationnellement viable. Chaque équipe doit pouvoir travailler de manière autonome, avec son propre backlog et ses propres critères de qualité. Le code finit par épouser la structure du pouvoir et de la communication.

Ce que nous appelons le « Conway inversé » est tout aussi important : une architecture influence à son tour les formes d'organisation qui peuvent l'adopter. Une architecture massive et fragmentée exige beaucoup de coordination. Une architecture cohérente et compressée réduit naturellement le besoin de coordination. Mais pour produire une telle architecture, il faut déjà ne pas être prisonnier de la fragmentation organisationnelle. La cohérence ne peut naître que de la cohérence.

Marius a été pensé et implémenté par une seule personne, sans équipe, sans silos, sans interfaces négociées entre départements. Il n'a jamais eu à se plier à la loi de Conway. Il n'a jamais été découpé pour refléter un organigramme, parce qu'il n'y avait pas d'organigramme.

---

### Le ROI et la tyrannie du court terme

Dans le monde professionnel, toute décision technique doit être justifiée économiquement. Construire un compilateur AOT pour générer du HTML à partir de données PostgreSQL, avec pipeline de rendu zéro-allocation et format binaire mémoire-mappé — quel chef de projet accepterait de financer des mois d'exploration pour un tel objectif ?

La question ne porte pas sur la valeur du résultat. Elle porte sur le fait que cette valeur est inconnue au moment de l'investissement. On finance une solution à un problème déjà identifié, pas une question. Marius n'est pas né d'un besoin métier. Il est né d'une question : « Pourquoi les systèmes logiciels modernes sont-ils devenus aussi lourds alors que les machines n'ont jamais été aussi puissantes ? Quelle est la structure minimale capable de produire les propriétés recherchées ? »

Cette question n'a pas de ROI prévisible. Elle peut ne mener nulle part. Elle peut exiger de tout réécrire plusieurs fois. Dans un cadre professionnel, chaque pivot, chaque remise en cause, chaque avancée — comme l'élimination du driver SQL du chemin chaud ou le passage à un rendu séquentiel pour saturer le cache L1 — aurait dû être négociée, justifiée, défendue.

La liberté qui a rendu Marius possible, c'est d'abord la liberté de poursuivre une question jusqu'au bout, sans jamais avoir à la traduire en termes de rentabilité.

---

### Les silos de spécialisation : l'architecture fragmentée par construction

Le monde professionnel du développement web est structuré en silos. On recrute des développeurs frontend, backend, des administrateurs de bases de données, des ingénieurs infrastructure. Chacun a ses outils, ses frameworks, ses « bonnes pratiques », ses angles morts.

Cette spécialisation rend pratiquement impossible de penser le système comme un tout. Un développeur frontend ne remet pas en cause le protocole de communication avec le backend. Un développeur backend ne remet pas en cause la nécessité d'un ORM. Un administrateur de bases de données ne remet pas en cause la séparation entre stockage et calcul. Chacun optimise sa couche, avec ses propres critères, et l'ensemble s'empile.

Nous n'avons nous-mêmes pris conscience que récemment de l'impact profond de ces pensées en silo — non pas de leur existence, que nous connaissions, mais de leur effet concret sur la structure des systèmes. La fragmentation n'est pas seulement organisationnelle. Elle est cognitive. Elle empêche de voir que l'ORM du backend, le cache de l'infrastructure et le rendu SSR du frontend sont trois manifestations d'un même problème — la distance entre la donnée et l'utilisateur — qui pourrait être résolu d'un seul geste si quelqu'un avait la liberté de le penser d'un seul tenant.

Marius ne contient ni « frontend » ni « backend » au sens professionnel. Le HTML est généré à l'écriture, par un compilateur AOT, et livré directement. Il n'y a pas de driver SQL sur le chemin chaud. Les données sont extraites périodiquement par un processus déconnecté, stockées dans un format binaire optimisé pour la lecture, et projetées directement. Cette architecture n'aurait pas pu émerger d'un comité de spécialistes. Sa cohérence vient de ce qu'elle est une vision unique, pas un compromis négocié.

---

### Le poids des conventions : ce que la formation professionnelle rend invisible

L'absence de formation en développement logiciel, dans notre cas, n'a pas été un handicap. Elle a été une condition de possibilité.

La formation professionnelle, qu'elle soit académique ou acquise en entreprise, transmet des présupposés qui finissent par paraître naturels : un serveur web a besoin d'un framework, d'un ORM, le HTML se génère au runtime, la performance passe par des caches. Ces présupposés ne sont pas faux dans l'absolu. Mais ce sont des conventions sociales, sédimentées par des années de pratiques partagées.

Nous n'avons jamais intériorisé ces conventions comme des lois naturelles. Pour nous, un serveur web n'a jamais été un processus qui cause à une base de données via un ORM. C'était un système de projection de données. La question était simplement : comment projeter des données de la manière la plus directe possible ? Que la réponse implique de supprimer le driver SQL, l'ORM, le cache, le framework et le rendu runtime n'est ni un exploit ni une provocation. C'est simplement le résultat d'un regard qui n'a jamais appris à considérer ces éléments comme obligatoires.

---

### Apprendre par le projet concret, comprendre par l'analyse post-mortem

Il y a un autre facteur, plus personnel mais déterminant : la méthode par laquelle nous avons toujours appris.

Nous n'avons jamais réellement progressé à partir de tutoriels suivis passivement. Nous n'avons progressé qu'à partir de projets concrets, suivis d'analyses post-mortem approfondies. Ce n'est pas une méthode rapide. Elle exige de se confronter directement au problème, de construire, d'échouer, de revenir sur ce qu'on a fait, de comprendre pourquoi cela a échoué, et de recommencer. Mais elle produit une connaissance incarnée, qui a un goût, une mémoire, un contexte. Chaque principe — le DOD, l'ECS, l'AOT, la sympatrie mécanique — a été rencontré d'abord comme réponse à une douleur réelle, et seulement ensuite comme concept formalisé.

Cette méthode est souvent célébrée dans le discours professionnel, mais rarement pratiquée en profondeur. Elle demande un temps long, non compressible, sans garantie de résultat immédiat. Le monde professionnel n'a pas ce temps. Il la remplace par des formes dégradées : des « coding dojos » d'une journée, des « hackathons » de 48 heures, des rétrospectives de deux heures. Ces rituels en gardent le vocabulaire, mais pas l'âme.

Ce que Marius démontre, c'est que la version patiente de cette méthode — celle qui s'étale sur des années, dans la liberté et l'amour du travail bien fait — produit des résultats que les formes abrégées ne produiront jamais.

---

### La solitude comme espace de pensée

Enfin, il faut mentionner un dernier facteur : la solitude intellectuelle.

Dans une équipe professionnelle, une proposition architecturale radicale doit être socialisée, discutée, défendue. Elle rencontre des objections légitimes, des inquiétudes, des résistances politiques. Le processus de décision collective agit comme un filtre qui élimine les propositions trop éloignées du consensus.

Marius, dans sa radicalité, est très éloigné du consensus. L'idée qu'un CMS puisse être un compilateur AOT avec un pipeline zéro-allocation et un format binaire propriétaire n'aurait probablement pas survécu à une réunion d'architecture. Elle aurait été jugée trop risquée, trop ésotérique, trop éloignée des standards.

Nous n'avons pas eu à convaincre qui que ce soit. Nous avons simplement pensé, implémenté, mesuré, corrigé, documenté. La solitude n'était pas un manque. Elle était l'espace nécessaire pour qu'une pensée cohérente puisse se déployer sans être diluée.

---

### Ce que Marius révèle

Marius n'est pas un argument contre le développement professionnel. Il en est un miroir. Il montre, par contraste, ce que les structures professionnelles rendent difficile : la pensée d'un seul tenant, la poursuite d'une question sans garantie de résultat, l'apprentissage par la confrontation directe au problème, la liberté de ne pas utiliser les outils standard.

Ce que Marius démontre, c'est qu'une autre voie existe. Pas pour tous les projets. Pas à toutes les échelles. Mais il existe un espace où une personne seule, armée d'une question et d'une méthode patiente, peut produire un système d'une cohérence et d'une frugalité que des équipes entières ne parviennent pas à atteindre.

Cet espace n'est pas confortable. Il n'offre ni salaire, ni stabilité, ni reconnaissance immédiate. Mais il offre ce que le monde professionnel ne peut pas offrir : la possibilité de repenser entièrement un problème, et de poursuivre cette pensée jusqu'à ses conséquences ultimes, sans jamais avoir à la compromettre.

Marius n'est pas un CMS. C'est une preuve. La preuve qu'il est encore possible, en 2026, de tout reprendre à zéro. À condition d'être libre. À condition d'oser ne pas savoir que c'est impossible.
