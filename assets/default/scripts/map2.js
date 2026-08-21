/**
 * @file map2.js
 * @description Rendu cartographique GPU via Deck.gl UMD
 */

// Extraction depuis le namespace global instancié par le script statique UMD
const { DeckGL, TileLayer, BitmapLayer, IconLayer } = window.deck;

const TILE_DEFAULT = "https://tile.openstreetmap.org/{z}/{x}/{y}.png";
const TILE_PROBE = Object.freeze({ z: "16", x: "33440", y: "23491" });
const SUBDOMAINS = Object.freeze(["a", "b", "c"]);

const SVG_ICON = `<svg xmlns="http://www.w3.org/2000/svg" width="512" height="512" viewBox="0 0 512 512">
  <defs>
    <filter id="shadow" x="-20%" y="-20%" width="140%" height="140%">
      <feDropShadow dx="15" dy="15" stdDeviation="15" flood-color="#000000" flood-opacity="0.5" />
    </filter>
  </defs>
  <g filter="url(#shadow)">
    <path fill="hsl(16, 100%, 66%)" d="M256 14C146 14 57 102 57 211c0 172 199 295 199 295s199-120 199-295c0-109-89-197-199-197zm0 281a94 94 0 1 1 0-187 94 94 0 0 1 0 187z"></path>
    <path fill="hsl(9,100%,64%)" d="M256 14v94a94 94 0 0 1 0 187v211s199-120 199-295c0-109-89-197-199-197z"></path>
  </g>
</svg>`;
const SVG_URI = `data:image/svg+xml;charset=utf-8,${encodeURIComponent(SVG_ICON)}`;

// ─── Extraction DOM (Single-Pass) ────────────────────────────────────────────

const collectMapConfigs = () => {
	const elements = document.querySelectorAll(".map");
	const configs = new Array(elements.length);
	for (let i = 0; i < elements.length; i++) {
		const el = elements[i];
		configs[i] = {
			el,
			tileServer: el.dataset.tileserver || null,
			minZoom: Number(el.dataset.minzoom) || 2,
			maxZoom: Number(el.dataset.maxzoom) || 19,
			zoom: el.dataset.zoom ? Number(el.dataset.zoom) : null,
			attribution: el.dataset.attribution || "",
			placesRaw: el.dataset.places || "[]",
		};
	}
	return configs;
};

// ─── Résolution statique (Zero-Network Latency) ──────────────────────────────

const parseTileServer = (template) => {
	if (!template) return TILE_DEFAULT;

	const cleanTmpl = template.replace(/&amp;/g, "&");

	// Si un sous-domaine est requis, on génère le tableau d'URLs pour le GPU
	if (cleanTmpl.includes("{s}")) {
		const urls = new Array(SUBDOMAINS.length);
		for (let i = 0; i < SUBDOMAINS.length; i++) {
			urls[i] = cleanTmpl.replace("{s}", SUBDOMAINS[i]);
		}
		return urls;
	}

	return cleanTmpl;
};

// ─── Instanciation WebGL (Lazy) ──────────────────────────────────────────────

const initMap = async (config) => {
	const { el, tileServer, minZoom, maxZoom, zoom, attribution, placesRaw } =
		config;

	// Phase 1 : Layout mémoire CPU
	const rawData = JSON.parse(placesRaw);
	const dataLength = rawData.length;
	const gpuData = new Array(dataLength);
	let minX = Infinity,
		minY = Infinity,
		maxX = -Infinity,
		maxY = -Infinity;

	for (let i = 0; i < dataLength; i++) {
		const item = rawData[i];
		const popup = item[0];
		const lat = item[1][0];
		const lng = item[1][1]; // Inversion spatiale AOT : Lat/Lng -> Lng/Lat

		gpuData[i] = { position: [lng, lat], popup };

		if (lng < minX) minX = lng;
		if (lat < minY) minY = lat;
		if (lng > maxX) maxX = lng;
		if (lat > maxY) maxY = lat;
	}

	// Phase 2 : Calcul de la matrice de vue globale
	// Compensateur de projection : ajuste la valeur Leaflet pour le moteur Deck.gl
	const ZOOM_OFFSET = -1;
	const targetZoom = zoom !== null ? zoom + ZOOM_OFFSET : null;

	// 1. Calcul du clamping de la caméra (application stricte du compensateur)
	const camMinZoom =
		minZoom !== null && minZoom !== undefined ? minZoom + ZOOM_OFFSET : 0;
	const camMaxZoom =
		maxZoom !== null && maxZoom !== undefined ? maxZoom + ZOOM_OFFSET : 20;

	// 2. Injection des limites (minZoom/maxZoom) dans l'état de la caméra
	let initialViewState = {
		longitude: 2.2137,
		latitude: 46.2276,
		zoom: 5,
		minZoom: camMinZoom,
		maxZoom: camMaxZoom,
	};

	if (dataLength > 0) {
		if (minX === maxX && minY === maxY) {
			// 1 point détecté
			initialViewState = {
				longitude: minX,
				latitude: minY,
				zoom: targetZoom || 16,
				minZoom: camMinZoom,
				maxZoom: camMaxZoom,
			};
		} else {
			// N points : Résolution de la matrice de vue via le WebMercatorViewport

			// 1. Lecture des dimensions physiques du conteneur (fallback à 400px si non monté)
			const rect = el.getBoundingClientRect();
			const width = rect.width || 800;
			const height = rect.height || 400;

			// 2. Instanciation éphémère du calculateur de projection
			const viewport = new deck.WebMercatorViewport({ width, height });

			// 3. Calcul mathématique strict (lon, lat, zoom)
			const fitted = viewport.fitBounds(
				[
					[minX, minY],
					[maxX, maxY],
				],
				{ padding: 40 }, // Marge de sécurité de 40px pour l'ombre des icônes
			);

			initialViewState = {
				longitude: fitted.longitude,
				latitude: fitted.latitude,
				// Verrouillage du zoom généré pour ne pas dépasser la résolution des tuiles (camMaxZoom)
				// Note: fitted.zoom est déjà à l'échelle Deck.gl, le ZOOM_OFFSET n'y est pas nécessaire.
				zoom: Math.min(fitted.zoom, camMaxZoom),
				minZoom: camMinZoom,
				maxZoom: camMaxZoom,
			};
		}
	}

	// Phase 3 : UI déportée
	if (attribution) {
		const attrNode = document.createElement("div");
		// Isolation absolue requise car .map est positionné en absolute par la librairie
		attrNode.style.cssText =
			"position: absolute; bottom: 0; right: 0; z-index: 1; background: rgba(255,255,255,0.9); padding: 2px 5px; font-size: 11px; font-family: sans-serif; pointer-events: auto;";
		attrNode.innerHTML = attribution;
		el.appendChild(attrNode);
	}

	const resolvedServer = parseTileServer(tileServer);

	// Phase 4 : Injection GPU
	new DeckGL({
		container: el,
		initialViewState,
		controller: true,
		getTooltip: ({ object }) => {
			if (!object) return null;

			// DeckGL gère le positionnement absolu du nœud DOM projeté depuis les coordonnées de la souris
			return {
				html: `<strong>${object.popup}</strong>`,
				style: {
					backgroundColor: "#ffffff",
					color: "#333333",
					padding: ".5em 1em",
					borderRadius: ".2em",
					boxShadow: "0 2px 4px rgba(0,0,0,0.3)",
					fontFamily: "sans-serif",
					fontSize: "1em",
					pointerEvents: "none", // Empêche l'infobulle de bloquer les événements de survol sous-jacents
				},
			};
		},
		layers: [
			new TileLayer({
				id: `raster-tiles-${el.id || Math.random().toString(16).slice(2)}`,
				data: resolvedServer,
				minZoom,
				maxZoom,
				tileSize: 256,
				renderSubLayers: (props) => {
					const { boundingBox } = props.tile;
					return new BitmapLayer(props, {
						data: null,
						image: props.data,
						bounds: [
							boundingBox[0][0],
							boundingBox[0][1],
							boundingBox[1][0],
							boundingBox[1][1],
						],
					});
				},
			}),
			new IconLayer({
				id: `vector-markers-${el.id || Math.random().toString(16).slice(2)}`,
				data: gpuData,
				pickable: true,
				getPosition: (d) => d.position,
				getIcon: () => ({
					url: SVG_URI,
					width: 512,
					height: 512,
					anchorY: 512,
				}),
				getSize: 40,
			}),
		],
	});
};

// ─── Pipeline de boot (Intersection Observer) ────────────────────────────────

const observeMaps = (configs) => {
	const pending = new Map(configs.map((c) => [c.el, c]));

	const observer = new IntersectionObserver(
		(entries) => {
			for (let i = 0; i < entries.length; i++) {
				const entry = entries[i];
				if (!entry.isIntersecting || entry.intersectionRatio < 0.1) continue;

				const el = entry.target;
				const config = pending.get(el);
				if (!config) continue;

				initMap(config);

				pending.delete(el);
				observer.unobserve(el);
			}
		},
		{ threshold: 0.1 },
	); // Seuil abaissé à 0.1 pour anticiper le chargement réseau des tuiles

	for (let i = 0; i < configs.length; i++) {
		observer.observe(configs[i].el);
	}
};

const bootstrap = () => {
	// Attente de l'évaluation complète du script UMD global
	if (typeof window.deck === "undefined") {
		console.warn("Pipeline AOT: deck.gl global namespace is missing.");
		return;
	}
	const configs = collectMapConfigs();
	if (configs.length) observeMaps(configs);
};

document.readyState === "loading"
	? document.addEventListener("DOMContentLoaded", bootstrap)
	: bootstrap();
