# Rapport de fin de phase

En te basant exclusivement sur l'implémentation réalisée aujourd'hui, réalise un rapport de fin de phase.

**1. Livrables**

- lister les tests ajoutés ;

**2. Analyse architecturale de la phase**

Pour cette implémentation uniquement :

- quels invariants ont été introduits ?
- quels invariants existants ont été confirmés ?
- certains invariants sont-ils devenus inutiles ou faux ?
- quelles mesures réelles ont été obtenues (`size_of`, benchmarks, etc.) ?
- certaines hypothèses des documents ont-elles été confirmées ou infirmées ?

**3. Impact documentaire**

- quelles documentations deviennent obsolètes ?
- lesquelles devraient être corrigées ?
- lesquelles devront plutôt être régénérées à la fin de l'implémentation complète ?

**4. Impact sur la roadmap**

À la lumière de cette implémentation :

- les prochaines phases restent-elles pertinentes ?
- certaines peuvent-elles être fusionnées ?
- certaines devraient-elles être découpées ?
- certains risques ont-ils disparu ?
- de nouveaux risques sont-ils apparus ?
- certaines signatures prévues peuvent-elles être simplifiées ?
- certaines structures de données deviennent-elles inutiles ?
- existe-t-il désormais une implémentation plus élégante que celle décrite dans les documents ?

**5. Regard d'architecte**

L'architecture vient-elle de révéler une propriété que les documents n'avaient pas anticipée ?

Si oui :

- expliquer cette propriété ;
- préciser si elle doit être portée par le code, une ADR, la spécification, ou seulement conservée pour la synthèse finale de l'implémentation.
