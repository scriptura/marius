# DESIGN — Pipeline Runtime de Segments (ADR-011)

**Statut** : Accepté — cinq sections (Ontologie/`SegmentDescriptor`/`MaterializedSource`/`EmissionPlan` ; `IoSlice` et descente monotone ; sélection de backend ; arène de requête ; contrat `RouteDescriptor`/`SourceSpec`). Reste hors périmètre, à traiter séparément : intégration `hyper`/Axum, devenir du trait `Projection` historique, mesure `MSG_ZEROCOPY`.

**Documents amont** : ADR-011 (révisée), ADR-008 (Minimum Viable Document, non remis en cause), ADR-006 (amendée — périmètre restreint au cas sans volatil), ADR-009 (adressage PK, non remis en cause).

**Fichiers non vus cette session** : `crates/shell/render/src/registry.rs` (`LiveRegistry`), `crates/shell/render/src/handlers.rs`. Les décisions ci-dessous s'appuient sur `render-shell-spec` (`PackfileEntry`, `ROUTE_TABLE`) et sur `DESIGN-store-registry.md` (`Arc`/`RwLock` comme patron déjà retenu ailleurs pour un problème de durée de vie analogue).

---

## 1. Chaîne d'IR — vue d'ensemble

```
Forge (AOT, compile-time)                    Runtime (par requête)
──────────────────────────                   ──────────────────────
Projection → Artefact → SegmentDescriptor[]  →  résolution SourceId  →  EmissionPlan → IoSlice[] → backend
   (niveaux 1-2, ADR-011 §3)   (niveau 3, fixe   (SourceRuntime,         (résultat de    (niveau 4)  (writev/
                                par route)         snapshot des sources)  la résolution)              sendmsg/...)
```

Frontière stricte : tout ce qui est à gauche de « résolution SourceId » est produit une fois, à la compilation ou à la régénération d'un artefact, et ne varie plus par requête. Tout ce qui est à droite est reconstruit à chaque requête, à coût constant, sans allocation tas.

Chaque niveau perd de la sémantique métier et gagne en proximité matérielle — la Forge ne connaît que des Projections, `IoSlice` ne connaît que des adresses. Aucun niveau ne doit connaître les invariants du niveau qui le précède de plus d'un cran (le backend d'émission n'a pas besoin de savoir ce qu'est une Projection ; `EmissionPlan` n'a pas besoin de savoir ce qu'est un backend).

**Stratification à trois familles, pas une simple suite d'étapes :**

| Famille | Éléments | Nature |
| --- | --- | --- |
| IR statique (Forge) | Projection, Artefact, `SegmentDescriptor[]` | Compilée une fois, figée par route |
| IR d'exécution (Runtime Marius) | `MaterializedSource`, `EmissionPlan` | Instanciée une fois par requête, propre à Marius |
| Représentation POSIX (backend) | `IoSlice`, `msghdr`, appel `writev`/`sendmsg` | N'est plus une IR de Marius — un backend interchangeable |

`IoSlice` n'est pas le niveau 4 de l'ontologie métier, c'est déjà une traduction vers une API système particulière. Un futur backend (`io_uring`, QUIC/HTTP3) remplacerait cette seule ligne sans toucher à `SegmentDescriptor`, `MaterializedSource` ni `EmissionPlan`.

**Le backend d'émission ne distingue jamais `Mmap` de `Volatile`.** Cette distinction disparaît intégralement lors de la construction de l'`EmissionPlan` puis des `IoSlice` (§7) : à partir de ce point, le backend ne manipule que des couples `(ptr, len)` sans origine attachée. C'est une conséquence directe de la descente monotone (§6) — verrouillée ici explicitement pour éviter qu'une future implémentation ne réintroduise un `match` sur la variante de Source à l'intérieur du backend, ce qui romprait la séparation IR Marius / représentation POSIX.

---

## 2. `SegmentDescriptor` — IR produite par la Forge

```rust
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SegmentDescriptor {
    pub source: SourceId,  // origine logique, résolue au runtime — jamais un pointeur ni un indice d'implémentation
    pub offset: u64,
    pub len:    u32,
    pub flags:  SegmentFlags,   // ex: Volatile, réservé pour extension — voir §4
}

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SourceId(u16);
```

Propriétés non négociables, actées sur plusieurs tours de discussion :

- `Copy`, POD, `#[repr(C)]` — pas de `Vec`, `String`, `Box`, `Arc`, pas de lifetime propre.
- `SourceId` désigne une **origine logique** (« le packfile navigation », « l'emplacement du panier dans l'arène de requête »), jamais un indice d'implémentation ni une adresse. La Forge ne connaît que des Sources ; c'est le Runtime qui décide comment les matérialiser (§3).
- Le tableau `SegmentDescriptor[]` d'une route est fixe, généré par la Forge, et son cardinal est vérifié à la compilation contre le budget de segments (ADR-011 §8). Le runtime ne le modifie ni ne le régénère — il le **résout**.

---

## 3. `MaterializedSource` — matérialisation d'une origine, résolue une fois par requête

`SourceId` est un identifiant fermé, pas un type ouvert (pas de trait object — voir §5). Sa résolution produit une valeur concrète :

```rust
#[repr(C)]
pub enum MaterializedSource {
    Mmap { base_ptr: *const u8 },          // artefact statique (packfile), aujourd'hui l'unique variante active
    Volatile { arena_ptr: *const u8 },      // segment de requête (session, panier...) — Phase 1 volatile uniquement
}
```

Enum fermé, pas trait object : le jeu de variantes est connu à la compilation, le dispatch est un `match`, pas une vtable — cohérent avec l'interdiction d'indirection dynamique sur le chemin chaud (ADR-011 §7).

Le Request Context résout, **une fois en tête de requête**, un petit tableau `[MaterializedSource; K]` (K borné par le budget de segments de la route) :

- pour chaque Source de type artefact statique : une résolution qui applique l'invariant défini par `DESIGN-store-registry.md` §7 (« un batch/une requête observe exactement une génération du monde ») — un seul point de résolution par requête, jamais un par segment. Ce DESIGN dépend de cet invariant, pas du mécanisme (`RwLock`+`Arc`) qui le réalise aujourd'hui : si `StoreRegistry`/`LiveRegistry` change d'implémentation demain sans violer l'invariant, cette section reste valide sans modification ;
- pour chaque Source volatile : un pointeur dans l'arène de requête (buffer sur pile ou pool pré-alloué, jamais alloué sur le tas pour cette requête).

Cette résolution est le seul endroit du pipeline qui touche un `Arc`/verrou. Le résultat (`[MaterializedSource; K]`) ne porte plus aucune lifetime explicite au-delà de la durée de la requête — la durée de vie réelle est garantie par la détention de l'`Arc` cloné dans le Request Context, pas par `SegmentDescriptor` ni par `MaterializedSource` eux-mêmes.

---

## 4. `EmissionPlan` — résultat de la résolution, jamais sa source

Point de vocabulaire introduit tardivement dans la discussion, avec une correction de position indispensable : `EmissionPlan` se situe **après** `SegmentDescriptor[]` dans la chaîne, jamais avant. Il nomme la combinaison, pour une requête donnée, du plan AOT fixe et de sa résolution runtime :

```rust
pub struct EmissionPlan<'req> {
    segments:     &'req [SegmentDescriptor],   // emprunté depuis la table statique générée par la Forge
    sources:      [MaterializedSource; K],     // résolu une fois, ci-dessus
    backend_kind: EmissionBackendKind,          // décidé par la Forge par route (§9.2), jamais redérivé ici
}
```

`EmissionPlan` ne construit rien — il **porte** la paire (plan fixe, sources résolues) le temps de la conversion finale vers `IoSlice[]` (section suivante du DESIGN). Aucune allocation, aucune copie de `SegmentDescriptor`. Sa seule responsabilité est de garantir, par construction du type, qu'on ne peut pas calculer un pointeur final sans être passé par la résolution complète des sources.

---

## 5. Ce que cette section n'tranche pas

- Construction exacte de `IoSlice[]` depuis `EmissionPlan` (section suivante).
- Nature du backend d'émission (`writev`/`sendmsg`/`MSG_ZEROCOPY`/futur `io_uring`) — volontairement indépendante de cette section, cf. ADR-011 §7 (trois invariants distincts) et l'amendement ADR-006.
- Devenir du trait `Projection` existant (ADR-011 §3) — sans impact sur cette section, qui ne s'appuie que sur `SegmentDescriptor`/`SourceId`, produits en aval de ce trait, quel que soit son nom final.

---

## 6. Principe directeur de la suite du DESIGN — descente monotone, aucune remontée

```
Projection → Artefact → SegmentDescriptor[] → MaterializedSource[] → EmissionPlan → IoSlice[] → Backend
```

Chaque étape abaisse le niveau d'abstraction vers le matériel. Aucune étape ne recompose une information déjà perdue par l'étape précédente — exactement la discipline d'une chaîne de compilation (AST → MIR → IR bas niveau → code machine), jamais un aller-retour. Concrètement pour la section qui suit :

- `IoSlice` ne réintroduit aucune sémantique métier (pas de notion de Projection, de Segment nommé, de domaine fonctionnel) — uniquement `(ptr, len)`.
- La construction d'`IoSlice[]` ne fait que traduire `EmissionPlan`, elle ne prend aucune décision nouvelle sur *quoi* émettre — cette décision est déjà entièrement figée par `SegmentDescriptor[]` (Forge) et `MaterializedSource[]` (résolution runtime, §3).
- Ce garde-fou sert de test pour toute extension future : si une modification de `IoSlice` ou du backend d'émission nécessite de consulter à nouveau une Projection ou un Artefact, c'est un signal que la descente n'est plus monotone, et que la modification est mal placée dans la chaîne.

---

## 7. `IoSlice[]` — traduction finale, représentation POSIX

Construction, sur pile, bornée par le budget de segments de la route (`K`, ADR-011 §8) :

```rust
fn build_io_slices<'req>(plan: &'req EmissionPlan<'req>) -> [IoSlice<'req>; K] {
    let mut slices: [MaybeUninit<IoSlice>; K] = /* ... */;
    for (i, seg) in plan.segments.iter().enumerate() {
        let base = plan.sources[seg.source.0 as usize].base_ptr(); // match exhaustif sur MaterializedSource, §3
        let ptr  = unsafe { base.add(seg.offset as usize) };
        let len  = seg.len as usize;
        slices[i] = MaybeUninit::new(IoSlice::new(unsafe {
            std::slice::from_raw_parts(ptr, len)
        }));
    }
    // transmute vers [IoSlice; K] une fois tous les éléments initialisés
}
```

Aucune allocation : `K` est une constante par route, connue à la compilation (ADR-011 §8), le tableau vit sur la pile de l'appel.

**Vérification de plateforme à intégrer au build (nouveau point, pas encore couvert par ADR-011 §8) :** le budget de segments `K` doit être vérifié par la Forge non seulement contre un plafond arbitraire, mais contre `IOV_MAX` (`UIO_MAXIOV`, 1024 sur Linux) — la limite réelle acceptée par `writev`/`sendmsg` en un seul appel. Dépasser cette limite transformerait un budget de segments valide en erreur `EINVAL` au runtime, exactement le type d'échec que la Forge doit intercepter à la compilation plutôt que le runtime au chemin chaud (cf. le contexte initial de cette conversation : la même limite avait été identifiée comme motivant l'ADR-011 dans son ensemble). Cette vérification appartient à `build.rs`, aux côtés de la vérification déjà décrite en §8 d'ADR-011 — pas un nouveau mécanisme, une contrainte supplémentaire sur le même contrôle.

**Point de sûreté clarifié — capacité connue AOT, longueur effective connue à la matérialisation (pas une exception au §6).** Les segments issus d'artefacts statiques possèdent une longueur exacte déterminée par la Forge : `offset` et `len` sont l'un et l'autre des faits AOT. Les segments volatils ne possèdent qu'une **capacité maximale** déterminée à la compilation ; leur **longueur effective** est fixée lors de leur matérialisation (§3). Cette opération complète une information volontairement laissée ouverte par la Forge — elle ne remet pas en cause la nature descendante du pipeline (§6), puisqu'aucune décision architecturale n'est prise à ce stade, seulement une valeur de fait renseignée dans les bornes déjà garanties.

Cette distinction n'est pas encore répercutée dans la structure `SegmentDescriptor` de la section 2 (`len` unique) — volontairement : en Phase 1, seule la variante `Mmap` existe réellement, donc `len` reste un fait AOT exact dans tous les cas traités aujourd'hui. Voir §8 pour le point différé.

---

## 8. Ce que cette section n'tranche pas (complète §5)

- **Évolution possible de `SegmentDescriptor.len` vers une paire `capacity`/`used_len`**, accompagnée d'un objet `MaterializedSegment { ptr, used_len }` distinct de `MaterializedSource` : `MaterializedSource` (§3) reste la résolution de l'**origine** (par Source, donc partagée entre plusieurs segments d'un même artefact) ; `MaterializedSegment` serait le résultat de la matérialisation **par segment** (ptr calculé + longueur effective), inséré entre la résolution des sources (§3) et la construction d'`IoSlice[]` (§7) — pas un remplacement de `MaterializedSource`, un niveau supplémentaire qui n'existe que pour les segments de variante `Volatile`. Non tranché maintenant : Phase 1 ne couvre que la variante `Mmap`, où `len` reste un fait AOT exact ; changer la structure prématurément figerait une représentation qui pourrait devoir être cassée dès le premier cas volatil réel.
- Le mécanisme de bornage exact des segments volatils à longueur variable (§7, point de sûreté) — nécessite une décision avant tout composant volatil réel, hors périmètre Phase 1 (ADR-011 §11).
- Le choix du backend d'émission consommant `IoSlice[]` (`writev` vs `sendmsg` vs futur) — section suivante.
- La gestion d'erreur si `writev`/`sendmsg` retourne une écriture partielle (short write) — comportement POSIX standard à spécifier au niveau backend, pas au niveau `IoSlice`.

---

## 9. Backend d'émission — sélection, pas bifurcation de l'IR

Conformément au garde-fou du §6, cette section ne prend aucune décision qui remonterait vers `SegmentDescriptor`, `MaterializedSource` ou `EmissionPlan`. L'IR reste une chaîne unique et ne bifurque jamais :

```
SegmentDescriptor[] → MaterializedSource[] → EmissionPlan
```

Le backend n'est pas une nouvelle étape de cette chaîne : c'est un **consommateur** d'`EmissionPlan`, sélectionné une fois, jamais redérivé à chaque appel.

```
EmissionPlan
      │
      ▼
EmissionBackendKind   (déterminé par la Forge, cf. §9.2 — pas recalculé au runtime)
      │
      ├── SingleFile → sendfile(fd, offset, len_total)
      └── Scatter    → writev()/sendmsg() sur IoSlice[]
```

### 9.1 Condition de compatibilité `SingleFile` — propriété sémantique, pas un compte de segments

La condition n'est pas `segments.len() == 1`. Elle est double :

- **aucun segment de la route n'est de variante `Volatile`** — pas seulement pour la requête courante, pour la route elle-même : le gabarit d'une route (quels emplacements sont statiques, lesquels sont volatils) est fixé par la Forge, indépendant de l'état de la requête ;
- **tous les segments statiques de la route proviennent de la même Source physique, à des offsets contigus** — deux segments issus de deux artefacts différents (`Nav.pack` + `Footer.pack`) restent incompatibles avec `sendfile()` même en l'absence totale de volatil, puisque deux descripteurs de fichier distincts ne peuvent pas être couverts par un seul appel.

### 9.2 Où cette décision est prise — AOT, pas par requête

Point de correction par rapport à une version antérieure de cette section : la compatibilité `SingleFile` **ne dépend d'aucune valeur connue seulement à la requête**. Elle ne dépend que du gabarit de la route (quels emplacements sont volatils, quelles Sources statiques sont utilisées) — deux informations entièrement connues de la Forge à la compilation, puisque c'est elle qui fixe la structure `SegmentDescriptor[]` de la route.

`EmissionBackendKind` est donc calculé **une fois, par la Forge, par route** — stocké à côté de `SegmentDescriptor[]` dans la table de routes (`ROUTE_TABLE`), pas recalculé ni redérivé par une méthode `EmissionPlan::is_sendfile_compatible()` au moment de la requête. Le Request Context lit ce champ, il ne le déduit jamais. C'est la même discipline que le budget de segments (§8 d'ADR-011) : le compilateur garantit, le runtime exécute — y compris pour le choix du backend, pas seulement pour ses bornes.

### 9.4 `writev`/`sendmsg` — comportement pour `EmissionBackendKind::Scatter`

```rust
fn emit(fd: RawFd, slices: &[IoSlice]) -> io::Result<usize> {
    // writev(2) : suffisant si aucune option socket (MSG_ZEROCOPY, flags) n'est requise
    // sendmsg(2) : requis si MSG_ZEROCOPY ou toute option socket est engagée (cf. §9.5)
}
```

**Écriture partielle (short write) — cas normal, pas une erreur.** `writev`/`sendmsg` peuvent retourner un nombre d'octets inférieur à la somme des `IoSlice`, sans que ce soit un échec (buffer socket plein, notamment sous forte charge). Le comportement retenu :

- Calculer les octets déjà émis, avancer dans le tableau de segments (ajuster le premier `IoSlice` partiellement consommé, sauter les suivants déjà émis) — mécanique standard, symétrique de ce que `sendfile()` gère déjà en interne pour un fichier unique.
- Aucune réallocation : l'avancement se fait par re-slicing des `IoSlice` existants, jamais par une copie.
- Ceci doit rester une boucle bornée (le nombre d'itérations ne peut pas dépasser le nombre de segments, lui-même borné par le budget AOT) — pas une boucle potentiellement non terminée.

### 9.5 `MSG_ZEROCOPY` — décision explicitement différée, pas présumée

Rappel de l'invariant déjà posé (ADR-011 §7, amendement ADR-006) : le passage à `writev`/`sendmsg` garantit zéro allocation et zéro reconstruction, **pas** zéro-copie réseau. Obtenir cette dernière propriété pour le cas composé exigerait `MSG_ZEROCOPY` (noyau ≥ 4.14), avec deux coûts propres, non encore chiffrés dans cette session :

- gestion asynchrone de la notification de complétion (`MSG_ERRQUEUE`) — le buffer ne peut pas être considéré libre tant que le noyau n'a pas confirmé la copie effective vers le NIC ;
- rentabilité dépendante de la taille : sous un certain seuil, le coût de la notification dépasse le gain, ce qui rendrait `MSG_ZEROCOPY` contre-productif précisément pour de petits segments volatils (le cas visé par ADR-011 §6).

**Décision retenue pour Phase 1 : `writev` sans `MSG_ZEROCOPY`.** La copie noyau résiduelle sur le chemin composé est acceptée comme coût connu et documenté (pas une régression silencieuse — l'amendement ADR-006 l'a déjà nommée). L'activation de `MSG_ZEROCOPY` reste une optimisation future, à ne considérer qu'après mesure, jamais par anticipation — même discipline que celle déjà appliquée par ADR-007/ADR-008 dans ce projet (ne pas construire avant la preuve du besoin).

---

## 10. Ce que cette section n'tranche pas

- Chiffrage réel du coût `MSG_ZEROCOPY` vs copie noyau simple — nécessite un banc de mesure, hors périmètre de ce DESIGN.
- Comportement exact en cas d'erreur irrécupérable en cours d'émission partielle (connexion coupée à mi-`writev`) — relève de la gestion de connexion HTTP générale, pas spécifique à cette section.
- Intégration avec `hyper::upgrade` ou équivalent pour obtenir un accès direct au socket sous Axum — point d'intégration pratique, pas une question de conception du pipeline de segments.

---

## 11. Arène de requête — support mémoire des segments volatils

Cette section verrouille des **propriétés**, pas une implémentation. L'implémentation de référence (arène par worker, réinitialisée entre requêtes) est un choix parmi d'autres compatibles avec ces propriétés — pas une obligation architecturale. Si le modèle d'exécution change un jour (workers, exécuteurs asynchrones, `io_uring`), seule l'implémentation est à revoir ; les invariants ci-dessous restent la référence.

### 11.1 Invariants verrouillés

1. Aucune allocation sur le chemin chaud (cohérent avec ADR-011 §7).
2. Allocation par curseur (« bump ») uniquement — jamais de structure d'allocation générale (pas de free-list, pas de recherche de bloc).
3. **Remise à zéro en O(1), à l'acquisition de l'arène par une requête, jamais à sa libération.**
4. Durée de vie strictement bornée à la requête qui l'a acquise.
5. Absence de partage concurrent d'une même arène entre deux requêtes simultanées.
6. Capacité exigée dérivée des bornes calculées par la Forge, jamais d'une constante choisie indépendamment.

### 11.2 Pourquoi le reset a lieu à l'acquisition, pas à la libération

Deux protocoles étaient possibles :

```
acquisition → utilisation → cleanup → libération      (rejeté)
acquisition → reset → utilisation → abandon            (retenu)
```

Le premier protocole dépend de l'exécution correcte d'un chemin de sortie (`cleanup`) pour rester sûr — un retour anticipé (erreur, panic, timeout) qui saute cette étape laisse l'arène dans un état incohérent pour la requête suivante, sans échec immédiat visible. Le second protocole est idempotent : aucune étape de nettoyage n'est requise sur les chemins d'erreur, parce que rien ne dépend de leur exécution — la garantie est reconstituée systématiquement à l'acquisition suivante, quel que soit l'état laissé par la précédente. C'est le même raisonnement que celui déjà appliqué ailleurs dans ce DESIGN et dans ADR-011 : préférer une garantie vérifiée en amont (ici, à l'acquisition) à une vérification tardive dont la fiabilité dépend de la discipline du code appelant.

### 11.3 Capacité — publiée par la Forge, jamais choisie indépendamment par le runtime

La Forge calcule et publie les exigences de capacité associées aux routes compilées (dérivées des capacités AOT de chaque segment volatil, §7-§8). Le runtime garantit que l'arène mise à disposition satisfait ces exigences — sans que cette section n'impose comment ces exigences sont agrégées (maximum global, par groupe de routes, par profil de worker, etc.). Cette latitude est volontaire : elle laisse la possibilité de spécialiser des profils mémoire différenciés plus tard, sans revoir cette section.

Ceci suit la même discipline que le budget de segments (§8 d'ADR-011) et la vérification `IOV_MAX` (§9.2) : une seule source de vérité (la Forge), le runtime consomme une exigence, il ne la redéfinit jamais.

### 11.4 Esquisse de structure (implémentation de référence, pas contrat)

```rust
pub struct RequestArena {
    buf:    *mut u8,   // pool pré-alloué, propriété du worker (ou de toute unité d'exécution retenue)
    cursor: usize,
    cap:    usize,     // dimensionné selon §11.3
}

impl RequestArena {
    fn acquire(&mut self) {
        self.cursor = 0;               // §11.2 : reset ici, pas ailleurs
    }
    fn bump(&mut self, len: usize) -> Option<*mut u8> {
        if self.cursor + len > self.cap { return None; }  // dépassement = erreur explicite, jamais une écriture hors bornes
        let ptr = unsafe { self.buf.add(self.cursor) };
        self.cursor += len;
        Some(ptr)
    }
}
```

Le cas de dépassement (`bump` retournant `None`) doit être un échec explicite et détectable — pas un déni silencieux ni une écriture hors bornes. Le traitement exact de ce cas (tronquer le segment volatil, rejeter la requête, autre) n'est pas tranché ici : c'est un point produit-métier, pas un point d'architecture mémoire.

**Alignement — hypothèse à expliciter, pas à laisser implicite.** L'esquisse ci-dessus avance octet par octet et présume une matérialisation de segments sous forme de `[u8]` plats, pour lesquels un alignement de 1 est suffisant — c'est le cas visé par cette section (contenu textuel/binaire opaque). Si le runtime devait un jour allouer dans cette arène des structures typées avec des contraintes d'alignement propres, le `bump` devrait intégrer un calcul de padding correspondant ; l'omettre serait un comportement indéfini classique des allocateurs bump en Rust. Non pertinent pour Phase 1, mais à ne pas oublier si l'usage de l'arène s'étend au-delà de segments `[u8]` plats.

---

## 12. Ce que cette section n'tranche pas

- Le traitement du cas de dépassement de capacité (`bump` → `None`, §11.4) : troncature, rejet, autre — décision produit, pas architecture.
- L'unité d'exécution exacte possédant l'arène (worker de thread, tâche asynchrone, autre) — implémentation de référence seulement, cf. préambule §11.
- Le mécanisme précis d'agrégation des exigences de capacité entre routes (§11.3) — volontairement laissé ouvert.

---

## 13. `RouteDescriptor` — le contrat explicite Forge → Runtime

Point identifié en relecture : les sections précédentes supposent que la Forge produit, par route, un ensemble d'informations cohérentes (`SegmentDescriptor[]`, §2 ; `EmissionBackendKind`, §9.2 ; exigence de capacité d'arène, §11.3) — mais aucune structure commune ne les rassemble, et rien ne spécifie comment un `SourceId` se rattache à un artefact réel. Ce contrat doit être explicite avant de figer ce document.

### 13.1 Séparation à respecter

`RouteDescriptor` porte uniquement des métadonnées **produites par la Forge** — un contrat AOT pur. Il ne contient jamais de type appartenant au runtime (pas de `PackfileEntry`, pas d'`Arc`, pas de `RawFd`). La manière dont le runtime associe ensuite ce contrat à un artefact concret via `ROUTE_TABLE`/`LiveRegistry` relève du render-shell-spec, pas de ce document.

```rust
#[repr(C)]
pub struct RouteDescriptor {
    pub segments:          &'static [SegmentDescriptor],  // §2 — fixe par route
    pub sources:           &'static [SourceSpec],          // §13.2 — table de résolution des SourceId
    pub backend_kind:      EmissionBackendKind,            // §9.2
    pub volatile_capacity: u32,                            // §11.3 — somme des capacités des segments Volatile
}
```

### 13.2 `SourceSpec` — le chaînon manquant : d'un `SourceId` logique à une recette de résolution

`SegmentDescriptor.source` est un `SourceId` — une origine logique, jamais un indice d'implémentation (§2), mais dont la portée est **locale à la route** (un indice dans le `sources` de ce `RouteDescriptor`). Quelque chose doit dire au Runtime *comment* matérialiser cette origine (§3). C'est le rôle de `SourceSpec`, indexé par `SourceId` au sein d'une route :

```rust
#[repr(C)]
pub enum SourceSpec {
    StaticArtifact { key: SourceKey },   // à résoudre via LiveRegistry (render-shell-spec)
    VolatileSlot   { capacity: u32 },     // à réserver dans l'arène de requête (§11)
}

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SourceKey(u16);
```

Correction par rapport à une version antérieure de cette section : le champ était initialement `registry_key: &'static str`. Un identifiant de type chaîne détonnait dans un contrat qui se veut entièrement POD/AOT — il implique une résolution par hachage/comparaison de chaîne au runtime, alors que le reste du pipeline n'utilise que des indices compacts. `SourceKey(u16)` corrige ce point, avec une distinction à ne pas confondre :

- **`SourceId`** (§2) : portée **locale à une route** — indice dans le `sources: &'static [SourceSpec]` de ce `RouteDescriptor` uniquement. Deux routes différentes peuvent réutiliser la même valeur numérique de `SourceId` pour désigner des origines complètement différentes.
- **`SourceKey`** (ici) : portée **globale au registre**, attribuée une fois par la Forge à chaque artefact nommé du système (`nav`, `article`, `footer`...), stable across toutes les routes qui le référencent. C'est cette valeur, pas `SourceId`, que la Forge transmet ; le Runtime la résout en un `Arc<PackHtmlIndex>` via un tableau ou une table `LiveRegistry` indexée par `SourceKey`, plutôt que par une clé de chaîne.

`SourceSpec` reste ainsi un contrat AOT pur, entièrement POD. La résolution `SourceKey → Arc<PackHtmlIndex>` via `LiveRegistry` (§3) reste du ressort du runtime et du render-shell-spec — ce document ne spécifie que la forme du contrat, pas le mécanisme de lookup, ni la manière dont `SourceKey` est assignée (probablement par le même passage de build que celui qui calcule le budget de segments, §8 d'ADR-011 — à confirmer lors de l'implémentation).

Avec `RouteDescriptor`/`SourceSpec`, la boucle Forge → Runtime est complète : `ROUTE_TABLE` (render-shell-spec) résout une URL vers un `RouteDescriptor` ; le Request Context parcourt `sources` pour construire `[MaterializedSource; K]` (§3) ; le reste du pipeline (§4 à §11) est inchangé.

### 13.3 Ce que cette section n'tranche pas

- La structure exacte de `ROUTE_TABLE` une fois qu'elle résout vers `RouteDescriptor` plutôt que vers `PackfileEntry` directement — modification du render-shell-spec, hors périmètre de ce DESIGN.
- Le mécanisme de lookup `SourceKey → Arc<PackHtmlIndex>` (existe déjà sous une forme voisine dans `LiveRegistry` — à confirmer par relecture du code, pas supposé ici).
- Le processus exact d'attribution des valeurs `SourceKey` par la Forge (numérotation séquentielle, hachage stable, autre) — détail de build, pas d'architecture.

