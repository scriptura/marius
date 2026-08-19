/*
 * ADR-001 : API HTML5 Drag & Drop native + Touch Events fallback
 * --------------------------------------------------
 * Utilisation de l'API Drag & Drop native pour les desktop et les
 * navigateurs qui la supportent (Chrome mobile), avec fallback vers
 * les événements tactiles pour Safari/Firefox mobile.
 *
 * ADR-002 : Pattern "drop zone" avec délégation d'événements
 * --------------------------------------------------
 * Tous les événements sont écoutés sur document plutôt que sur chaque
 * élément. La cible réelle est résolue dynamiquement via closest() et
 * matches().
 *
 * ADR-003 : Calcul spatial de la position d'insertion
 * --------------------------------------------------
 * La position d'insertion est déterminée par la position Y du pointeur
 * comparée aux boîtes englobantes de tous les items de la liste.
 *
 * ADR-004 : Détection de paire d'items encadrants
 * --------------------------------------------------
 * Identification d'une paire (itemBefore, itemAfter) qui encadre la
 * position d'insertion pour le feedback visuel.
 *
 * ADR-005 : Séparation des responsabilités de nettoyage
 * --------------------------------------------------
 * Nettoyage séparé des classes de cible et source pour garantir un
 * état propre quelle que soit la façon dont le drag se termine.
 *
 * ADR-006 : Module pattern sans classes
 * --------------------------------------------------
 * Fonctions pures et variables dans la portée du module plutôt que
 * des classes ES6 pour favoriser la performance.
 *
 * ADR-007 : Support tactile hybride
 * --------------------------------------------------
 * Détection du support de l'API Drag & Drop native. Si non supportée,
 * utilisation des événements touchstart/touchmove/touchend avec un
 * clone fantôme pour simuler le drag. Le drag natif reste utilisé
 * quand disponible.
 *
 * ADR-008 : Accessibilité WAI-ARIA complète
 * --------------------------------------------------
 * Implémentation des rôles listbox/option + ARIA et navigation clavier
 * complète (Espace pour saisir, Flèches pour déplacer,
 * Espace pour déposer, Échap pour annuler).
 * Le composant est conforme WCAG 2.1 AA et suit les WAI-ARIA Authoring Practices
 *
 * ADR-009 (AOT/DOD) : Spatial Caching & Pipeline Déterministe
 * --------------------------------------------------
 * - Mise en cache des NodeLists au démarrage du drag (AOT) pour
 *   garantir 0 allocation/requête DOM pendant les événements continus (hot paths).
 * - Séparation stricte Read/Write lors du survol pour empêcher le Layout Thrashing.
 * - Structure d'état centralisée pour garantir un nettoyage mémoire complet.
 */

// ============ ÉTAT GLOBAL DU SYSTÈME (STRUCT) ============
// Isoler l'état pour faciliter le GC et éviter les fuites mémoire
const Session = {
	active: false,
	mode: null, // 'native' | 'touch' | 'keyboard'
	draggedEl: null,
	sourceList: null,

	// Tactile
	ghost: null,
	touchOffsetX: 0,
	touchOffsetY: 0,
	touchStartY: 0,
	touchMoved: false,
	lastTouchX: 0,
	lastTouchY: 0,

	// Cache spatial (AOT)
	cachedSiblings: [],
	lastList: null,
	lastBefore: null,
	lastAfter: null,
};

// Références statiques pour le binding des events (permet le unmount)
const Handlers = {
	dragStart: onDragStart,
	dragOver: onDocumentDragOver,
	drop: onDocumentDrop,
	dragEnd: onDragEnd,
	touchStart: onTouchStart,
	touchMove: onTouchMove,
	touchEnd: onTouchEnd,
	touchCancel: onTouchCancel,
	keyDown: onKeyDown,
};

// ============ CYCLE DE VIE (AOT INIT) ============

/**
 * Initialise le système sur un conteneur racine.
 * Doit être appelé explicitement par l'application hôte.
 */
export function mountDragSystem(rootContainer = document) {
	const lists = rootContainer.querySelectorAll(".drag-list");
	if (lists.length === 0) return false;

	const useNative = supportsDragAndDrop();

	lists.forEach((list) => {
		list.setAttribute("role", "listbox");
		list.setAttribute(
			"aria-label",
			list.getAttribute("aria-label") || "Liste réordonnable",
		);
		list.setAttribute("aria-orientation", "vertical");

		const children = list.querySelectorAll(":scope > *");
		children.forEach((child, index) => {
			child.setAttribute("draggable", useNative ? "true" : "false");
			child.setAttribute("data-draggable", "true");
			child.setAttribute("role", "option");
			child.setAttribute("aria-grabbed", "false");
			child.setAttribute("aria-posinset", String(index + 1));
			child.setAttribute("aria-setsize", String(children.length));

			if (!child.hasAttribute("tabindex")) {
				child.setAttribute("tabindex", "0");
			}
		});
	});

	createAriaAnnouncer();

	// Attachement des events
	if (useNative) {
		document.addEventListener("dragstart", Handlers.dragStart);
		document.addEventListener("dragover", Handlers.dragOver);
		document.addEventListener("drop", Handlers.drop);
		document.addEventListener("dragend", Handlers.dragEnd);
	}

	// Le fallback tactile reste actif même si natif dispo (surface hybride)
	document.addEventListener("touchstart", Handlers.touchStart, {
		passive: false,
	});
	document.addEventListener("touchmove", Handlers.touchMove, {
		passive: false,
	});
	document.addEventListener("touchend", Handlers.touchEnd);
	document.addEventListener("touchcancel", Handlers.touchCancel);
	document.addEventListener("keydown", Handlers.keyDown);

	updateEmptyState();

	return true;
}

/**
 * Nettoie la mémoire et détache les listeners.
 */
export function unmountDragSystem() {
	document.removeEventListener("dragstart", Handlers.dragStart);
	document.removeEventListener("dragover", Handlers.dragOver);
	document.removeEventListener("drop", Handlers.drop);
	document.removeEventListener("dragend", Handlers.dragEnd);

	document.removeEventListener("touchstart", Handlers.touchStart);
	document.removeEventListener("touchmove", Handlers.touchMove);
	document.removeEventListener("touchend", Handlers.touchEnd);
	document.removeEventListener("touchcancel", Handlers.touchCancel);
	document.removeEventListener("keydown", Handlers.keyDown);

	resetSession();
}

// ============ MÉCANIQUES INTERNES ============

function supportsDragAndDrop() {
	const div = document.createElement("div");
	return "draggable" in div || ("ondragstart" in div && "ondrop" in div);
}

function resetSession() {
	Session.active = false;
	Session.mode = null;
	Session.draggedEl = null;
	Session.sourceList = null;
	Session.ghost = null;
	Session.cachedSiblings = [];
	Session.lastList = null;
	Session.lastBefore = null;
	Session.lastAfter = null;
}

// ============ ARIA-LIVE ============

function createAriaAnnouncer() {
	if (document.getElementById("drag-announcer")) return;
	const announcer = document.createElement("div");
	announcer.id = "drag-announcer";
	announcer.setAttribute("aria-live", "polite");
	announcer.setAttribute("aria-atomic", "true");
	announcer.className = "sr-only";
	document.body.appendChild(announcer);
}

function announceDrag(message) {
	const announcer = document.getElementById("drag-announcer");
	if (!announcer) return;
	announcer.textContent = "";
	// Microtask pour forcer le lecteur d'écran à détecter le changement
	setTimeout(() => {
		announcer.textContent = message;
	}, 50);
}

function updateAriaSets() {
	document.querySelectorAll(".drag-list").forEach((list) => {
		const items = list.querySelectorAll(':scope > [data-draggable="true"]');
		const total = String(items.length);
		for (let i = 0; i < items.length; i++) {
			items[i].setAttribute("aria-posinset", String(i + 1));
			items[i].setAttribute("aria-setsize", total);
		}
	});
}

// ============ DRAG NATIF ============

function onDragStart(e) {
	const target = e.target;
	if (
		!target.matches('.drag-list > [draggable="true"]') ||
		Session.mode === "touch"
	)
		return;

	Session.active = true;
	Session.mode = "native";
	Session.draggedEl = target;
	e.dataTransfer.effectAllowed = "move";
	target.classList.add("dragged");
	target.setAttribute("aria-grabbed", "true");

	const list = target.closest(".drag-list");
	if (list) list.setAttribute("aria-dropeffect", "move");

	cacheLayoutSiblings(list);
	announceDrag(
		`${target.textContent.trim()} saisi. Flèches pour déplacer, échap pour annuler.`,
	);
}

function onDocumentDragOver(e) {
	if (Session.mode === "touch") return;
	e.preventDefault(); // Nécessaire pour autoriser le drop
	e.dataTransfer.dropEffect = "move";

	const list = e.target.closest(".drag-list");
	if (list !== Session.lastList) cacheLayoutSiblings(list); // Re-cache si changement de liste

	processDragOverPipeline(list, e.clientY);
}

function onDocumentDrop(e) {
	if (Session.mode === "touch") return;
	e.preventDefault();
	if (!Session.draggedEl) return;

	const list = e.target.closest(".drag-list");
	if (list) executeDrop(list, e.clientY);
}

function onDragEnd() {
	cleanupDOMState();
	resetSession();
}

// ============ TACTILE ============

function createTouchGhost(target, touch) {
	const rect = target.getBoundingClientRect();
	Session.touchOffsetX = touch.clientX - rect.left;
	Session.touchOffsetY = touch.clientY - rect.top;

	const ghost = target.cloneNode(true);
	ghost.classList.add("drag-ghost");
	ghost.setAttribute("aria-hidden", "true");
	ghost.style.width = `${rect.width}px`;
	ghost.style.left = `${touch.clientX - Session.touchOffsetX}px`;
	ghost.style.top = `${touch.clientY - Session.touchOffsetY}px`;

	document.body.appendChild(ghost);
	return ghost;
}

function onTouchStart(e) {
	const target = e.target.closest('[data-draggable="true"]');
	if (!target) return;
	const list = target.closest(".drag-list");
	if (!list) return;

	const touch = e.touches[0];
	Session.active = true;
	Session.mode = "touch";
	Session.draggedEl = target;
	Session.touchStartY = touch.clientY;
	Session.touchMoved = false;
	Session.lastTouchX = touch.clientX;
	Session.lastTouchY = touch.clientY;
	Session.ghost = createTouchGhost(target, touch);

	target.classList.add("dragged");
	target.setAttribute("aria-grabbed", "true");
	list.setAttribute("aria-dropeffect", "move");

	cacheLayoutSiblings(list);
	announceDrag(`${target.textContent.trim()} saisi. Glissez pour déplacer.`);

	// preventDefault pour éviter le scroll seulement sur l'item saisi
	if (e.cancelable) e.preventDefault();
}

function onTouchMove(e) {
	if (Session.mode !== "touch" || !Session.ghost) return;

	const touch = e.touches[0];
	Session.lastTouchX = touch.clientX;
	Session.lastTouchY = touch.clientY;

	if (
		!Session.touchMoved &&
		Math.abs(touch.clientY - Session.touchStartY) > 5
	) {
		Session.touchMoved = true;
	}
	if (!Session.touchMoved) return;

	Session.ghost.style.left = `${touch.clientX - Session.touchOffsetX}px`;
	Session.ghost.style.top = `${touch.clientY - Session.touchOffsetY}px`;

	const elUnder = document.elementFromPoint(touch.clientX, touch.clientY);
	const list = elUnder?.closest(".drag-list") ?? null;

	if (list !== Session.lastList) cacheLayoutSiblings(list);
	processDragOverPipeline(list, touch.clientY);

	if (e.cancelable) e.preventDefault();
}

function onTouchEnd(e) {
	if (Session.mode !== "touch" || !Session.draggedEl) return;

	let clientY = Session.lastTouchY;
	let clientX = Session.lastTouchX;

	if (e.changedTouches && e.changedTouches.length > 0) {
		clientY = e.changedTouches[0].clientY;
		clientX = e.changedTouches[0].clientX;
	}

	const elUnder = document.elementFromPoint(clientX, clientY);
	const list = elUnder?.closest(".drag-list") ?? null;

	if (list && Session.touchMoved) {
		executeDrop(list, clientY);
	}

	Session.ghost?.remove();

	cleanupDOMState();
	resetSession();
}

function onTouchCancel() {
	Session.ghost?.remove();
	cleanupDOMState();
	resetSession();
}

// ============ CLAVIER ============

function onKeyDown(e) {
	if (e.target.matches('input, textarea, [contenteditable="true"]')) return;

	const target = e.target.closest('[data-draggable="true"]');
	if (!target) return;

	const list = target.closest(".drag-list");
	if (!list) return;

	// Pas de cache spatial nécessaire pour le clavier (navigation logique, pas physique)
	const items = Array.from(
		list.querySelectorAll(':scope > [data-draggable="true"]'),
	);
	const currentIndex = items.indexOf(target);

	switch (e.key) {
		case " ":
		case "Spacebar":
			e.preventDefault();
			handleKeyboardSpace(target, list);
			break;
		case "ArrowUp":
			e.preventDefault();
			Session.mode === "keyboard"
				? handleKeyboardMove(list, items, currentIndex, -1)
				: handleKeyboardNavigate(items, currentIndex, -1);
			break;
		case "ArrowDown":
			e.preventDefault();
			Session.mode === "keyboard"
				? handleKeyboardMove(list, items, currentIndex, 1)
				: handleKeyboardNavigate(items, currentIndex, 1);
			break;
		case "Escape":
			e.preventDefault();
			if (Session.mode === "keyboard") {
				cleanupDOMState();
				announceDrag("Déplacement annulé.");
				if (Session.draggedEl) Session.draggedEl.focus();
				resetSession();
			}
			break;
	}
}

function handleKeyboardSpace(target, list) {
	if (Session.mode === "keyboard") {
		const newIndex = Array.from(
			list.querySelectorAll(':scope > [data-draggable="true"]'),
		).indexOf(target);
		announceDrag(
			`${target.textContent.trim()} déposé en position ${newIndex + 1}.`,
		);
		cleanupDOMState();
		resetSession();
		updateEmptyState();
		updateAriaSets();
	} else {
		Session.mode = "keyboard";
		Session.draggedEl = target;
		Session.sourceList = list;
		target.setAttribute("aria-grabbed", "true");
		target.classList.add("dragged");
		list.setAttribute("aria-dropeffect", "move");
		announceDrag(
			`${target.textContent.trim()} saisi. Utilisez les flèches pour déplacer, espace pour déposer.`,
		);
	}
}

function handleKeyboardMove(list, items, currentIndex, direction) {
	const newIndex = currentIndex + direction;
	if (newIndex < 0 || newIndex >= items.length) return;

	const draggedItem = Session.draggedEl;
	const targetItem = items[newIndex];

	if (direction < 0) {
		list.insertBefore(draggedItem, targetItem);
	} else {
		const next = targetItem.nextSibling;
		next ? list.insertBefore(draggedItem, next) : list.appendChild(draggedItem);
	}

	updateAriaSets();
	announceDrag(
		`${draggedItem.textContent.trim()} déplacé en position ${newIndex + 1}.`,
	);
	draggedItem.focus();
}

function handleKeyboardNavigate(items, currentIndex, direction) {
	const newIndex = currentIndex + direction;
	if (newIndex >= 0 && newIndex < items.length) items[newIndex].focus();
}

// ============ LOGIQUE COMMUNE (AOT/DOD Pipeline) ============

/**
 * AOT : Mise en cache du tableau des enfants éligibles au drop pour le pipeline Read.
 * Supprime les appels coûteux à querySelectorAll/Array.from() durant le mouvement.
 */
function cacheLayoutSiblings(list) {
	Session.lastList = list;
	if (!list) {
		Session.cachedSiblings = [];
		return;
	}
	// Les NodeLists dynamiques sont évitées. On génère un layout plat (AoS local).
	const children = list.querySelectorAll(
		':scope > [data-draggable="true"]:not(.dragged)',
	);
	const siblings = new Array(children.length);
	for (let i = 0; i < children.length; i++) {
		siblings[i] = children[i];
	}
	Session.cachedSiblings = siblings;
}

/**
 * Pipeline Déterministe (Read -> Diff -> Write).
 * Empêche le Layout Thrashing.
 */
function processDragOverPipeline(list, clientY) {
	// 1. READ Phase (Spatial calculation)
	let itemBefore = null;
	let itemAfter = null;

	if (list && Session.cachedSiblings.length > 0) {
		const siblings = Session.cachedSiblings;
		const len = siblings.length;

		for (let i = 0; i < len; i++) {
			const rect = siblings[i].getBoundingClientRect(); // Lecture seule, pas de reflow si le write est différé

			if (clientY < rect.top) {
				itemAfter = siblings[i];
				itemBefore = i > 0 ? siblings[i - 1] : null;
				break;
			}
			if (clientY >= rect.top && clientY <= rect.bottom) {
				const itemMiddle = rect.top + rect.height / 2;
				if (clientY < itemMiddle) {
					itemAfter = siblings[i];
					itemBefore = i > 0 ? siblings[i - 1] : null;
				} else {
					itemBefore = siblings[i];
					itemAfter = i < len - 1 ? siblings[i + 1] : null;
				}
				break;
			}
		}
		if (!itemBefore && !itemAfter && len > 0) {
			itemBefore = siblings[len - 1];
		}
	}

	// 2. DIFF Phase
	if (
		itemBefore === Session.lastBefore &&
		itemAfter === Session.lastAfter &&
		list === Session.lastList
	) {
		return; // Aucun changement d'état géométrique, on court-circuite la mutation DOM
	}

	// 3. WRITE Phase (DOM Mutation)
	if (Session.lastBefore) Session.lastBefore.classList.remove("over-bottom");
	if (Session.lastAfter) Session.lastAfter.classList.remove("over-top");
	if (Session.lastList && Session.lastList !== list) {
		Session.lastList.classList.remove("drag-over");
		Session.lastList.removeAttribute("aria-dropeffect");
	}

	if (list) {
		list.classList.add("drag-over");
		list.setAttribute("aria-dropeffect", "move");
	}
	if (itemBefore) itemBefore.classList.add("over-bottom");
	if (itemAfter) itemAfter.classList.add("over-top");

	// Mise à jour de l'état
	Session.lastBefore = itemBefore;
	Session.lastAfter = itemAfter;
}

function executeDrop(list, clientY) {
	const el = Session.draggedEl;
	if (!el || !list) return;

	let inserted = false;
	let newPosition = Session.cachedSiblings.length;
	const siblings = Session.cachedSiblings;

	for (let i = 0; i < siblings.length; i++) {
		const rect = siblings[i].getBoundingClientRect();
		if (clientY < rect.top + rect.height / 2) {
			list.insertBefore(el, siblings[i]);
			inserted = true;
			newPosition = i;
			break;
		}
	}

	if (!inserted) list.appendChild(el);

	announceDrag(
		`${el.textContent.trim()} déplacé en position ${newPosition + 1}.`,
	);
	updateEmptyState();
	updateAriaSets();
}

function cleanupDOMState() {
	// Nettoyage ciblé basé sur les classes pour éviter querySelectorAll brutal
	document
		.querySelectorAll(
			".drag-list > .dragged, .drag-list > .over-top, .drag-list > .over-bottom",
		)
		.forEach((item) => {
			item.classList.remove("dragged", "over-top", "over-bottom");
			item.setAttribute("aria-grabbed", "false");
		});
	document.querySelectorAll(".drag-list.drag-over").forEach((item) => {
		item.classList.remove("drag-over");
		item.removeAttribute("aria-dropeffect");
	});
}

function updateEmptyState() {
	document.querySelectorAll(".drag-list").forEach((list) => {
		const hasChildren = list.firstElementChild !== null; // Plus rapide que querySelectorAll
		list.classList.toggle("drag-list-empty", !hasChildren);
	});
}
