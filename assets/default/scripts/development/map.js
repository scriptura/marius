/**
 * @file map.js
 * @description Gestionnaire de cartes Leaflet (ES Module).
 *
 * Pipeline :
 *   1. collectMapConfigs : Lecture pure (zéro mutation DOM), tableau pré-alloué.
 *   2. observeMaps       : Lazy-loading via IntersectionObserver unique.
 *   3. resolveTileServer : Sonde parallèle avec annulation (AbortController).
 *   4. initMap           : Parsing data et instanciation Leaflet isolés.
 */

// ─── Constantes immuables ────────────────────────────────────────────────────

import "leaflet.js";

const TILE_DEFAULT = "https://tile.openstreetmap.org/{z}/{x}/{y}.png";
const TILE_PROBE = Object.freeze({ z: "16", x: "33440", y: "23491" });
const ANIM_CLASS = "start-map";
const ANIM_DURATION = 1500;
const SUBDOMAINS = Object.freeze(["a", "b", "c"]);

const SVG_ICON =
	'<svg class="marker-icon" xmlns="http://www.w3.org/2000/svg" viewBox="0 0 512 512">' +
	'<path d="M256 14C146 14 57 102 57 211c0 172 199 295 199 295s199-120 199-295c0-109-89-197-199-197zm0 281a94 94 0 1 1 0-187 94 94 0 0 1 0 187z"/>' +
	'<path d="M256 14v94a94 94 0 0 1 0 187v211s199-120 199-295c0-109-89-197-199-197z"/>' +
	"</svg>";

// ─── Lazy singleton divIcon ──────────────────────────────────────────────────

let _divIcon = null;

const getDivIcon = () =>
	(_divIcon ??= L.divIcon({
		className: "leaflet-data-marker",
		html: SVG_ICON,
		iconAnchor: [20, 40],
		iconSize: [40, 40],
		popupAnchor: [0, -60],
	}));

// ─── Extraction Data-Oriented ────────────────────────────────────────────────

/**
 * @typedef {Object} MapConfig
 * @property {HTMLElement}   el
 * @property {string|null}   tileServer
 * @property {number|string} minZoom
 * @property {number|string} maxZoom
 * @property {string|null}   zoom
 * @property {string}        attribution
 * @property {string}        placesRaw
 */

/**
 * Extraction pure. Zéro mutation du DOM. Pré-allocation du tableau.
 * @returns {MapConfig[]}
 */
const collectMapConfigs = () => {
	const nodes = document.querySelectorAll(".map");
	const len = nodes.length;
	const configs = new Array(len);

	for (let i = 0; i < len; i++) {
		const el = nodes[i];
		configs[i] = {
			el,
			tileServer: el.dataset.tileserver || null,
			minZoom: el.dataset.minzoom || 2,
			maxZoom: el.dataset.maxzoom || 18,
			zoom: el.dataset.zoom || null,
			attribution: el.dataset.attribution || "",
			placesRaw: el.dataset.places,
		};
	}
	return configs;
};

// ─── Résolution I/O ──────────────────────────────────────────────────────────

/**
 * Sonde réseau avec AbortController pour garantir la fermeture des sockets
 * des requêtes perdantes.
 */
const resolveTileServer = async (template) => {
	if (!template) return TILE_DEFAULT;

	const ac = new AbortController();

	const buildProbeUrl = (tmpl, subdomain = "") =>
		tmpl
			.replace("{s}", subdomain)
			.replace("{z}", TILE_PROBE.z)
			.replace("{x}", TILE_PROBE.x)
			.replace("{y}", TILE_PROBE.y);

	const probe = (url) =>
		fetch(url, { method: "HEAD", signal: ac.signal }).then((r) => {
			if (!r.ok) throw new Error(r.status);
			return template;
		});

	const candidates = template.includes("{s}")
		? SUBDOMAINS.map((s) => probe(buildProbeUrl(template, s)))
		: [probe(buildProbeUrl(template))];

	try {
		const winner = await Promise.any(candidates);
		ac.abort(); // Coupe les requêtes concurrentes inutiles
		return winner;
	} catch {
		return TILE_DEFAULT;
	}
};

// ─── Initialisation Leaflet ──────────────────────────────────────────────────

/**
 * Instancie Leaflet directement sur l'HTMLElement stocké.
 */
const initMap = async (config) => {
	const { el, tileServer, minZoom, maxZoom, zoom, attribution, placesRaw } =
		config;

	const places = JSON.parse(placesRaw);
	const map = L.map(el); // Leaflet gère le Node DOM directement
	const resolvedServer = await resolveTileServer(tileServer);

	const tileLayer = L.tileLayer(resolvedServer, {
		minZoom,
		maxZoom,
		attribution,
	}).addTo(map);

	tileLayer.on("tileerror", () => {
		if (resolvedServer !== TILE_DEFAULT) {
			L.tileLayer(TILE_DEFAULT, { minZoom, maxZoom, attribution }).addTo(map);
		}
	});

	const icon = getDivIcon();
	const bounds = L.latLngBounds();
	const placesLen = places.length;

	for (let i = 0; i < placesLen; i++) {
		const [popup, latlng] = places[i];
		bounds.extend(latlng);
		const marker = L.marker(latlng, { icon });
		if (popup) marker.bindPopup(popup);
		marker.addTo(map);
	}

	map.fitBounds(bounds);
	if (zoom) map.setZoom(Number(zoom));
};

// ─── Observer (Pipeline) ─────────────────────────────────────────────────────

/**
 * Initialisation différée au viewport.
 */
const observeMaps = (configs) => {
	const configsLen = configs.length;
	const pending = new Map();

	// Remplissage explicite sans itérateur Map implicite
	for (let i = 0; i < configsLen; i++) {
		pending.set(configs[i].el, configs[i]);
	}

	const observer = new IntersectionObserver(
		(entries) => {
			const entriesLen = entries.length;
			for (let i = 0; i < entriesLen; i++) {
				const entry = entries[i];
				if (!entry.isIntersecting || entry.intersectionRatio < 0.5) continue;

				const el = entry.target;
				const config = pending.get(el);
				if (!config) continue;

				el.classList.add(ANIM_CLASS);
				setTimeout(() => el.classList.remove(ANIM_CLASS), ANIM_DURATION);

				initMap(config);

				pending.delete(el);
				observer.unobserve(el);
			}
		},
		{ threshold: 0.5 },
	);

	for (let i = 0; i < configsLen; i++) {
		observer.observe(configs[i].el);
	}
};

// ─── API ESM Publique ────────────────────────────────────────────────────────

/**
 * Point d'entrée à invoquer par l'AssetSystem une fois le DOM et Leaflet garantis.
 */
export const initMaps = () => {
	if (typeof L === "undefined") {
		console.warn("initMaps: Leaflet (L) n'est pas instancié.");
		return;
	}

	const configs = collectMapConfigs();
	if (configs.length > 0) {
		observeMaps(configs);
	}
};
