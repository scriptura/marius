# ADR-002 : Déclassement du HashSet au profit du Bit-Vector (Collector)

- **Statut :** Accepté
- **Contexte :** Moteur de rendu réactif "Marius"
- **Supersède :** [ADR-001 / Manifeste] Section 5 : Le Collector (HashSet)

---

## 1. Problématique (The "Why")

La conception initiale du **Collector** reposait sur un `HashSet<ID>` Rust pour assurer le dédoublonnement des signaux `LISTEN/NOTIFY`. Bien que fonctionnelle, cette structure viole les principes **DOD (Data-Oriented Design)** de Marius pour trois raisons critiques :

1.  **Fragmentation Mémoire :** Le `HashSet` alloue des buckets sur le tas de manière non contiguë, provoquant des _cache misses_ lors de l'itération par le Dispatcher.
2.  **Coût de Hachage :** Chaque signal entrant subit une fonction de hachage, gaspillant des cycles CPU pour une donnée (l'ID entier) qui est déjà une clé primaire unique.
3.  **Contention de Verrouillage :** La synchronisation multi-thread (Mutex/RwLock) nécessaire pour protéger le `HashSet` entre le listener et le dispatcher devient un goulot d'étranglement sous haute charge d'écriture.

## 2. Décision Architecturale

Nous remplaçons le `HashSet` par un **Bit-Vector (via la crate `bitvec`)** agissant comme une table de présence indexée par l'ID de l'entité.

### Invariants Techniques

- **Layout Contigu :** Utilisation d'un bloc de mémoire plat (`BitBox` ou tableau d'atomiques).
- **Indexation Directe :** L'ID SQL (`SERIAL` / `BIGINT`) sert directement d'index dans le vecteur de bits.
- **Idempotence Native :** L'opération `set(id, true)` est atomique et ne nécessite pas de vérification préalable d'existence.

## 3. Analyse CPU / Mémoire

### Densité des Données

Pour 1 000 000 d'entités potentielles, l'empreinte mémoire passe de plusieurs mégaoctets (HashSet) à exactement **125 Ko** (1 bit/entité). Ce volume garantit une résidence quasi permanente dans le **cache L2** du processeur.

### Efficacité du Dispatcher

Lors du cycle de `flush`, le Dispatcher n'itère plus sur des structures complexes. Il scanne des mots machine (`usize` de 64 bits) :

- Si un mot vaut `0`, **64 entités** sont ignorées en une seule instruction.
- Si un mot est non nul, l'utilisation de l'instruction CPU `TZCNT` (Trailing Zero Count) permet d'extraire les IDs modifiés sans scan linéaire.

## 4. Conséquences

### Avantages

- **Déterminisme Temporel :** Le temps d'insertion d'un signal est désormais constant et indépendant de la taille du Collector.
- **Zéro Allocation au Runtime :** La structure est pré-allouée au démarrage du service Marius.
- **Lock-Free :** Possibilité d'utiliser des `AtomicUsize` pour permettre l'écriture (Signal) et la lecture (Dispatch) sans aucun Mutex.

### Contraintes & Risques

- **Densité des IDs :** Cette approche impose des IDs entiers contigus. L'utilisation d'UUIDs rendrait ce pattern inefficace (vecteur trop large).
- **Gestion de la Taille :** La taille maximale du vecteur doit être définie à la compilation ou au démarrage (ex: `MAX_ID_ALLOWED`).

---

**Conclusion :** Le passage au Bit-Vector transforme le Collector d'une structure de données "boîte noire" en un tampon de signalisation haute performance, parfaitement aligné avec le pipeline AOT de Marius.

Le design des IDs de votre base de données actuelle permet-il d'anticiper une borne maximale (ex: 2^24 entités) pour figer la taille du bit-vector au démarrage ?
